pub mod latency_arb;
pub mod certainty_capture;
pub mod convexity_fade;
pub mod cross_timeframe;
pub mod strike_misalign;
pub mod lp_extreme;

#[cfg(test)]
pub(crate) mod test_helpers;
#[cfg(test)]
mod bench_latency;

use crate::engine::state::MarketState;
use crate::types::{EvalTrigger, Signal};

/// Strategy trait: stateless pure function of market state.
/// Same code runs in live engine and backtester.
pub trait Strategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn trigger(&self) -> EvalTrigger {
        EvalTrigger::PolymarketQuote
    }
    fn evaluate(&self, state: &MarketState, now_ms: i64) -> Option<Signal>;
}

/// Evaluate a filtered subset of strategies, filling pre-allocated buffer.
#[inline]
pub fn evaluate_filtered(
    strategies: &[&dyn Strategy],
    state: &MarketState,
    now_ms: i64,
    buf: &mut Vec<Signal>,
) {
    buf.clear();
    for s in strategies {
        if let Some(sig) = s.evaluate(state, now_ms) {
            buf.push(sig);
        }
    }
}

/// Half-Kelly position sizing.
pub fn kelly(edge: f64, price: f64) -> f64 {
    if price >= 1.0 || edge <= 0.0 {
        return 0.0;
    }
    ((edge / (1.0 - price)) * 0.5).clamp(0.0, 0.15)
}

/// Time left as fraction of total window (1.0 at start, 0.0 at end).
pub fn time_left_fraction(state: &MarketState, now_ms: i64) -> f64 {
    let total = (state.info.end_ms - state.info.start_ms).max(1) as f64;
    let left = (state.info.end_ms - now_ms).max(0) as f64;
    left / total
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: Positive edge (5%) at 50-cent price.
    /// Expected: Half-Kelly produces a positive position size.
    #[test]
    fn test_kelly_positive_edge() {
        let f = kelly(0.05, 0.50);
        assert!(f > 0.0, "Positive edge should produce positive sizing: {}", f);
    }

    /// Scenario: Zero edge at 50-cent price.
    /// Expected: Kelly returns 0 -- no position without edge.
    #[test]
    fn test_kelly_zero_edge() {
        assert_eq!(kelly(0.0, 0.50), 0.0);
    }

    /// Scenario: Price equals 1.0 (certain outcome, no payout upside).
    /// Expected: Kelly returns 0 -- denominator is zero, no sizing.
    #[test]
    fn test_kelly_price_at_one() {
        assert_eq!(kelly(0.05, 1.0), 0.0);
    }

    // ── Kelly edge cases ──

    /// Scenario: Negative edge (-5%) at 50-cent price.
    /// Expected: Kelly returns 0 -- never size into a negative-EV trade.
    #[test]
    fn test_kelly_negative_edge() {
        assert_eq!(kelly(-0.05, 0.50), 0.0);
    }

    /// Scenario: Price above 1.0 (impossible for valid binary, but edge case).
    /// Expected: Kelly returns 0 -- invalid price triggers early return.
    #[test]
    fn test_kelly_price_above_one() {
        assert_eq!(kelly(0.10, 1.5), 0.0);
    }

    /// Scenario: Huge edge (90%) at near-zero price (1 cent) producing raw Kelly ~0.45.
    /// Expected: Result is clamped to the 15% max position size cap.
    #[test]
    fn test_kelly_clamp_at_015() {
        // edge=0.90, price=0.01 → raw = (0.90 / 0.99) * 0.5 ≈ 0.4545 → clamped to 0.15
        let f = kelly(0.90, 0.01);
        assert!((f - 0.15).abs() < 1e-10, "Should clamp to 0.15: {}", f);
    }

    /// Scenario: 10% edge at 40-cent price, within normal sizing range.
    /// Expected: Result matches exact half-Kelly formula (0.10/0.60)*0.5 = 0.0833.
    #[test]
    fn test_kelly_exact_formula() {
        // edge=0.10, price=0.40 → kelly = (0.10 / 0.60) * 0.5 = 0.0833
        let f = kelly(0.10, 0.40);
        let expected = (0.10 / 0.60) * 0.5;
        assert!((f - expected).abs() < 1e-10, "kelly = {}, expected = {}", f, expected);
    }

    /// Scenario: 5% edge at price 0.99, denominator near zero producing raw Kelly ~2.5.
    /// Expected: Result is clamped to the 15% max position size cap.
    #[test]
    fn test_kelly_price_near_one() {
        // price=0.99 → denominator = 0.01 → kelly = (0.05 / 0.01) * 0.5 = 2.5 → clamped 0.15
        let f = kelly(0.05, 0.99);
        assert!((f - 0.15).abs() < 1e-10, "Should clamp: {}", f);
    }

    // ── time_left_fraction tests ──

    /// Scenario: Evaluate time_left_fraction at exactly the market start time.
    /// Expected: Returns 1.0 -- full window remaining.
    #[test]
    fn test_time_left_fraction_at_start() {
        let (state, _) = test_helpers::make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        let frac = time_left_fraction(&state, state.info.start_ms);
        assert!((frac - 1.0).abs() < 1e-10, "At start: frac = {}", frac);
    }

    /// Scenario: Evaluate time_left_fraction at exactly the market end time.
    /// Expected: Returns 0.0 -- no time remaining.
    #[test]
    fn test_time_left_fraction_at_end() {
        let (state, _) = test_helpers::make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        let frac = time_left_fraction(&state, state.info.end_ms);
        assert!((frac - 0.0).abs() < 1e-10, "At end: frac = {}", frac);
    }

    /// Scenario: Evaluate time_left_fraction at the midpoint between start and end.
    /// Expected: Returns ~0.5 -- half the window remains.
    #[test]
    fn test_time_left_fraction_midway() {
        let (state, _) = test_helpers::make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        let mid = (state.info.start_ms + state.info.end_ms) / 2;
        let frac = time_left_fraction(&state, mid);
        assert!((frac - 0.5).abs() < 0.01, "Midway: frac = {}", frac);
    }

    /// Scenario: Evaluate time_left_fraction 10 seconds past market end.
    /// Expected: Returns 0.0 -- clamped at zero, never goes negative.
    #[test]
    fn test_time_left_fraction_past_end() {
        let (state, _) = test_helpers::make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        let frac = time_left_fraction(&state, state.info.end_ms + 10_000);
        assert_eq!(frac, 0.0, "Past end should be 0: {}", frac);
    }

    /// Scenario: Evaluate time_left_fraction 5 seconds before market start.
    /// Expected: Returns > 1.0 -- more than the full window remains.
    #[test]
    fn test_time_left_fraction_before_start() {
        let (state, _) = test_helpers::make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        let frac = time_left_fraction(&state, state.info.start_ms - 5_000);
        assert!(frac > 1.0, "Before start should be > 1.0: {}", frac);
    }

    // ── evaluate_filtered tests ──

    /// Scenario: Call evaluate_filtered with an empty strategies list.
    /// Expected: Output buffer is empty -- no strategies means no signals.
    #[test]
    fn test_evaluate_filtered_empty_strategies() {
        let strategies: Vec<&dyn Strategy> = vec![];
        let (state, now) = test_helpers::make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        let mut buf = Vec::new();
        evaluate_filtered(&strategies, &state, now, &mut buf);
        assert!(buf.is_empty());
    }

    /// Scenario: Call evaluate_filtered with a pre-populated buffer and no strategies.
    /// Expected: Buffer is cleared on entry, even when no strategies run.
    #[test]
    fn test_evaluate_filtered_clears_buffer() {
        use crate::types::Side;
        let strategies: Vec<&dyn Strategy> = vec![];
        let (state, now) = test_helpers::make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        let mut buf = vec![Signal {
            strategy: "dummy",
            side: Side::Up,
            edge: 0.1,
            fair_value: 0.5,
            market_price: 0.4,
            confidence: 0.5,
            size_frac: 0.01,
            is_passive: false,
            use_bid: false,
        }];
        evaluate_filtered(&strategies, &state, now, &mut buf);
        assert!(buf.is_empty(), "Buffer should be cleared even with no strategies");
    }

    /// Scenario: Run evaluate_filtered with real LatencyArb and CertaintyCapture strategies on default state.
    /// Expected: Pipeline runs without panics; at most 2 signals collected (likely 0 with no book/edge).
    #[test]
    fn test_evaluate_filtered_collects_signals() {
        // Use real strategies — most will return None but tests the pipeline
        let latency = latency_arb::LatencyArb;
        let certainty = certainty_capture::CertaintyCapture;
        let strategies: Vec<&dyn Strategy> = vec![&latency, &certainty];
        let (state, now) = test_helpers::make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        let mut buf = Vec::new();
        evaluate_filtered(&strategies, &state, now, &mut buf);
        // Both will likely return None (no book data, z too low) — that's fine
        // Key: no panics, buffer properly populated
        assert!(buf.len() <= 2);
    }
}
