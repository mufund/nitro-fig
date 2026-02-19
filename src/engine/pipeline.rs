//! Shared signal processing pipeline.
//!
//! Used by both the live engine (`runner.rs`) and the backtest engine
//! (`bin/backtest/engine.rs`) to ensure identical signal deconfliction,
//! sorting, risk checking, and house-side coherence.
//!
//! Engine-specific behavior (telemetry logging, order dispatch, fill
//! recording) is abstracted via the [`SignalSink`] trait.

use crate::engine::risk::StrategyRiskManager;
use crate::engine::state::{MarketState, StrategyStats};
use crate::types::{Order, Side, Signal};

// ─── Sink trait ─────────────────────────────────────────────────────────────

/// Callbacks for engine-specific side-effects during signal processing.
///
/// The live engine implements this with async channel sends and telemetry.
/// The backtester implements this with direct `Vec<Fill>` / `Vec<TradeRecord>` pushes.
pub trait SignalSink {
    /// Called for every signal before risk check (logging / telemetry).
    fn on_signal(&mut self, sig: &Signal, state: &MarketState, now_ms: i64);

    /// Called when a signal passes risk and an order is produced.
    fn on_order(&mut self, sig: &Signal, order: &Order, state: &MarketState, now_ms: i64);
}

// ─── Config ─────────────────────────────────────────────────────────────────

/// Configuration knobs that differ between live and backtest.
pub struct ProcessConfig {
    /// Simulated slippage added to fill price (0 for live, 0.01 for backtest).
    pub slippage_cents: f64,
}

impl ProcessConfig {
    pub fn live() -> Self {
        Self { slippage_cents: 0.0 }
    }
    pub fn backtest() -> Self {
        Self { slippage_cents: 0.01 }
    }
}

// ─── Shared pipeline ────────────────────────────────────────────────────────

/// Maximum number of directional flips allowed per market.
/// A flip is when house_side changes from Some(Up) to Some(Down) or vice versa.
/// Prevents churn from repeated thesis reversals mid-market.
const MAX_DIRECTION_FLIPS: u32 = 1;

/// Process a batch of signals through the full pipeline.
///
/// Steps:
/// 1. **House-side filter**: If `house_side` is set, drop active signals on the
///    wrong side (passive signals are exempt). If `flip_count >= MAX_DIRECTION_FLIPS`,
///    also block active signals that would flip direction.
/// 2. **Deconfliction**: If `house_side` is `None` and active signals disagree,
///    score each side by `sum(edge * confidence)`, keep the dominant side only.
/// 3. **Sort** by `edge * confidence` descending so the best signals hit the
///    risk manager first (matters when budget is tight).
/// 4. **Log** every signal via `sink.on_signal`.
/// 5. **Risk check** each signal. On approval: apply slippage, update stats,
///    set `house_side` (only if `confidence >= 0.7`), call `sink.on_order`.
///
/// Returns `true` if at least one order was dispatched.
pub fn process_signals(
    signals: &mut Vec<Signal>,
    state: &mut MarketState,
    risk: &mut StrategyRiskManager,
    house_side: &mut Option<Side>,
    flip_count: &mut u32,
    next_order_id: &mut u64,
    now_ms: i64,
    config: &ProcessConfig,
    sink: &mut dyn SignalSink,
) -> bool {
    if signals.is_empty() {
        return false;
    }

    // ── Step 1: House-side filter ──
    if let Some(hs) = *house_side {
        signals.retain(|s| s.is_passive || s.side == hs);
        if signals.is_empty() {
            return false;
        }
    }
    // ── Step 2: Deconflict when no house view yet ──
    else {
        let active: Vec<&Signal> = signals.iter().filter(|s| !s.is_passive).collect();
        if active.len() > 1 {
            let (mut up_score, mut down_score) = (0.0_f64, 0.0_f64);
            for s in &active {
                match s.side {
                    Side::Up => up_score += s.edge * s.confidence,
                    Side::Down => down_score += s.edge * s.confidence,
                }
            }
            if up_score > 0.0 && down_score > 0.0 {
                let dominant = if up_score >= down_score { Side::Up } else { Side::Down };
                signals.retain(|s| s.is_passive || s.side == dominant);
            }
        }
    }

    // ── Step 3: Sort by edge * confidence descending ──
    signals.sort_unstable_by(|a, b| {
        let score_a = a.edge * a.confidence;
        let score_b = b.edge * b.confidence;
        score_b.partial_cmp(&score_a).unwrap_or(std::cmp::Ordering::Equal)
    });

    // ── Step 4: Log all signals ──
    for sig in signals.iter() {
        sink.on_signal(sig, state, now_ms);
    }

    // ── Step 5: Risk check + dispatch ──
    let mut any_dispatched = false;

    for sig in signals.iter() {
        state.total_signals += 1;
        state.strategy_stats
            .entry(sig.strategy)
            .or_insert_with(StrategyStats::new)
            .signals += 1;

        if let Some(mut order) = risk.check_strategy(sig, state, *next_order_id, now_ms) {
            // Apply slippage
            if config.slippage_cents > 0.0 {
                order.price = (order.price + config.slippage_cents).min(0.99);
            }

            state.total_orders += 1;
            let strat_stats = state
                .strategy_stats
                .entry(sig.strategy)
                .or_insert_with(StrategyStats::new);
            strat_stats.orders += 1;
            strat_stats.total_edge += sig.edge;

            // Only high-confidence active signals can set the house direction.
            // Prevents low-conviction strategies (e.g. convexity_fade at 0.3-0.65)
            // from locking the portfolio into a direction based on a weak signal.
            // Once MAX_DIRECTION_FLIPS is reached, house_side is locked for the market.
            if !sig.is_passive && sig.confidence >= 0.7 {
                match *house_side {
                    None => {
                        *house_side = Some(sig.side);
                    }
                    Some(prev) if prev != sig.side && *flip_count < MAX_DIRECTION_FLIPS => {
                        *house_side = Some(sig.side);
                        *flip_count += 1;
                    }
                    _ => {} // same side or flips exhausted — no change
                }
            }

            risk.on_order_sent(sig.strategy, now_ms, order.size);
            state.position.on_order_sent();

            sink.on_order(sig, &order, state, now_ms);

            *next_order_id += 1;
            any_dispatched = true;
        }
    }

    any_dispatched
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::test_helpers::*;

    /// Test sink that records calls for assertions.
    struct TestSink {
        signals: Vec<String>,
        orders: Vec<(String, Side, f64, f64)>, // (strategy, side, price, size)
    }

    impl TestSink {
        fn new() -> Self {
            Self {
                signals: Vec::new(),
                orders: Vec::new(),
            }
        }
    }

    impl SignalSink for TestSink {
        fn on_signal(&mut self, sig: &Signal, _state: &MarketState, _now_ms: i64) {
            self.signals.push(sig.strategy.to_string());
        }
        fn on_order(&mut self, sig: &Signal, order: &Order, _state: &MarketState, _now_ms: i64) {
            self.orders
                .push((sig.strategy.to_string(), sig.side, order.price, order.size));
        }
    }

    fn make_signal(
        strategy: &'static str,
        side: Side,
        edge: f64,
        confidence: f64,
        price: f64,
    ) -> Signal {
        Signal {
            strategy,
            side,
            edge,
            fair_value: price + edge,
            market_price: price,
            confidence,
            size_frac: 0.02,
            is_passive: false,
        }
    }

    fn make_passive_signal(
        strategy: &'static str,
        side: Side,
        edge: f64,
        price: f64,
    ) -> Signal {
        Signal {
            strategy,
            side,
            edge,
            fair_value: price + edge,
            market_price: price,
            confidence: 0.5,
            size_frac: 0.02,
            is_passive: true,
        }
    }

    /// When house_side is None and active signals disagree, the dominant side
    /// (by sum of edge * confidence) survives.
    #[test]
    fn test_deconfliction_picks_dominant_side() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        let mut signals = vec![
            make_signal("latency_arb", Side::Up, 0.05, 0.8, 0.50),   // score = 0.04
            make_signal("convexity_fade", Side::Down, 0.03, 0.6, 0.50), // score = 0.018
        ];
        let mut house_side = None;
        let mut flip_count = 0u32;
        let mut next_id = 1;
        let conf = ProcessConfig::live();
        let mut sink = TestSink::new();

        process_signals(
            &mut signals, &mut state, &mut risk,
            &mut house_side, &mut flip_count, &mut next_id, now, &conf, &mut sink,
        );

        // All logged signals should be Up only (Down was deconflicted away)
        assert!(
            sink.signals.iter().all(|s| s == "latency_arb"),
            "Only dominant side (Up) signals should be logged: {:?}", sink.signals
        );
    }

    /// When house_side is set, opposite-side active signals are dropped.
    #[test]
    fn test_house_side_filters_opposite() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        let mut signals = vec![
            make_signal("latency_arb", Side::Down, 0.05, 0.8, 0.50),
        ];
        let mut house_side = Some(Side::Up);
        let mut flip_count = 0u32;
        let mut next_id = 1;
        let conf = ProcessConfig::live();
        let mut sink = TestSink::new();

        process_signals(
            &mut signals, &mut state, &mut risk,
            &mut house_side, &mut flip_count, &mut next_id, now, &conf, &mut sink,
        );

        assert!(sink.signals.is_empty(), "Down signal should be filtered by house=Up");
    }

    /// Passive signals are exempt from house-side filtering.
    #[test]
    fn test_passive_exempt_from_house_side() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        let mut signals = vec![
            make_passive_signal("lp_extreme", Side::Down, 0.05, 0.10),
        ];
        let mut house_side = Some(Side::Up);
        let mut flip_count = 0u32;
        let mut next_id = 1;
        let conf = ProcessConfig::live();
        let mut sink = TestSink::new();

        process_signals(
            &mut signals, &mut state, &mut risk,
            &mut house_side, &mut flip_count, &mut next_id, now, &conf, &mut sink,
        );

        assert_eq!(sink.signals.len(), 1, "Passive signal should survive house_side filter");
        assert_eq!(sink.signals[0], "lp_extreme");
    }

    /// Signals are sorted by edge * confidence descending before processing.
    #[test]
    fn test_signals_sorted_by_score() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        let mut signals = vec![
            make_signal("convexity_fade", Side::Up, 0.02, 0.4, 0.50),  // score = 0.008
            make_signal("latency_arb", Side::Up, 0.05, 0.8, 0.50),     // score = 0.040
            make_signal("certainty_capture", Side::Up, 0.03, 0.9, 0.50), // score = 0.027
        ];
        let mut house_side = None;
        let mut flip_count = 0u32;
        let mut next_id = 1;
        let conf = ProcessConfig::live();
        let mut sink = TestSink::new();

        process_signals(
            &mut signals, &mut state, &mut risk,
            &mut house_side, &mut flip_count, &mut next_id, now, &conf, &mut sink,
        );

        // Signals logged in score order: latency_arb (0.040), certainty_capture (0.027), convexity_fade (0.008)
        assert_eq!(sink.signals[0], "latency_arb");
        assert_eq!(sink.signals[1], "certainty_capture");
        assert_eq!(sink.signals[2], "convexity_fade");
    }

    /// Only signals with confidence >= 0.7 can set house_side.
    #[test]
    fn test_house_side_set_by_high_confidence_only() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        // Only a low-confidence signal
        let mut signals = vec![
            make_signal("convexity_fade", Side::Up, 0.05, 0.4, 0.50),
        ];
        let mut house_side: Option<Side> = None;
        let mut flip_count = 0u32;
        let mut next_id = 1;
        let conf = ProcessConfig::live();
        let mut sink = TestSink::new();

        process_signals(
            &mut signals, &mut state, &mut risk,
            &mut house_side, &mut flip_count, &mut next_id, now, &conf, &mut sink,
        );

        assert!(house_side.is_none(), "Low-confidence signal should not set house_side");
    }

    /// High-confidence signal sets house_side.
    #[test]
    fn test_house_side_set_by_high_confidence() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        let mut signals = vec![
            make_signal("strike_misalign", Side::Down, 0.05, 0.9, 0.50),
        ];
        let mut house_side: Option<Side> = None;
        let mut flip_count = 0u32;
        let mut next_id = 1;
        let conf = ProcessConfig::live();
        let mut sink = TestSink::new();

        process_signals(
            &mut signals, &mut state, &mut risk,
            &mut house_side, &mut flip_count, &mut next_id, now, &conf, &mut sink,
        );

        // If the signal passed risk and filled, house_side should be set
        if !sink.orders.is_empty() {
            assert_eq!(house_side, Some(Side::Down),
                "High-confidence signal should set house_side");
        }
    }

    /// Backtest config applies +1 cent slippage to fill price.
    #[test]
    fn test_slippage_applied_in_backtest() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        let mut signals = vec![
            make_signal("latency_arb", Side::Up, 0.05, 0.8, 0.50),
        ];
        let mut house_side = None;
        let mut flip_count = 0u32;
        let mut next_id = 1;
        let conf = ProcessConfig::backtest();
        let mut sink = TestSink::new();

        process_signals(
            &mut signals, &mut state, &mut risk,
            &mut house_side, &mut flip_count, &mut next_id, now, &conf, &mut sink,
        );

        if let Some((_, _, price, _)) = sink.orders.first() {
            assert!(
                (*price - 0.51).abs() < 0.001,
                "Backtest slippage should add 0.01: got {}", price
            );
        }
    }

    /// Live config does NOT apply slippage.
    #[test]
    fn test_no_slippage_in_live() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        let mut signals = vec![
            make_signal("latency_arb", Side::Up, 0.05, 0.8, 0.50),
        ];
        let mut house_side = None;
        let mut flip_count = 0u32;
        let mut next_id = 1;
        let conf = ProcessConfig::live();
        let mut sink = TestSink::new();

        process_signals(
            &mut signals, &mut state, &mut risk,
            &mut house_side, &mut flip_count, &mut next_id, now, &conf, &mut sink,
        );

        if let Some((_, _, price, _)) = sink.orders.first() {
            assert!(
                (*price - 0.50).abs() < 0.001,
                "Live should not add slippage: got {}", price
            );
        }
    }

    /// Strategy stats are incremented for signals and orders.
    #[test]
    fn test_strategy_stats_incremented() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        let mut signals = vec![
            make_signal("latency_arb", Side::Up, 0.05, 0.8, 0.50),
        ];
        let mut house_side = None;
        let mut flip_count = 0u32;
        let mut next_id = 1;
        let conf = ProcessConfig::live();
        let mut sink = TestSink::new();

        process_signals(
            &mut signals, &mut state, &mut risk,
            &mut house_side, &mut flip_count, &mut next_id, now, &conf, &mut sink,
        );

        assert!(state.total_signals >= 1, "total_signals should be incremented");
        let stats = state.strategy_stats.get("latency_arb");
        assert!(stats.is_some(), "latency_arb should have stats");
        assert!(stats.unwrap().signals >= 1, "signal count should be incremented");
    }

    /// Empty signal buffer returns false and does nothing.
    #[test]
    fn test_empty_signals_noop() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        let mut signals: Vec<Signal> = vec![];
        let mut house_side = None;
        let mut flip_count = 0u32;
        let mut next_id = 1;
        let conf = ProcessConfig::live();
        let mut sink = TestSink::new();

        let result = process_signals(
            &mut signals, &mut state, &mut risk,
            &mut house_side, &mut flip_count, &mut next_id, now, &conf, &mut sink,
        );

        assert!(!result, "Empty signals should return false");
        assert!(sink.signals.is_empty());
        assert!(sink.orders.is_empty());
    }
}
