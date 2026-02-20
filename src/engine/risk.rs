use std::collections::HashMap;
use std::time::Instant;

use crate::config::Config;
use crate::engine::state::MarketState;
use crate::math::pricing::{delta_bin, gamma_bin};
use crate::types::{Fill, Order, OrderAck, OrderType, Side, Signal};

#[derive(Clone)]
pub struct StrategyLimits {
    pub max_per_trade_frac: f64,
    pub max_total_frac: f64,
    pub cooldown_ms: i64,
    pub max_orders_per_market: u32,
}

struct StrategyRiskState {
    exposure: f64,
    orders_this_market: u32,
    last_order_ms: i64,
}

impl StrategyRiskState {
    fn new() -> Self {
        Self {
            exposure: 0.0,
            orders_this_market: 0,
            last_order_ms: 0,
        }
    }
}

// ─── Portfolio Greeks ─────────────────────────────────────────────────────────

/// Per-fill record for Greeks recomputation as market conditions change.
#[derive(Clone)]
struct FillGreeksRecord {
    side: Side,
    size: f64,
}

/// Aggregate portfolio Greeks snapshot.
#[derive(Clone, Copy, Debug, Default)]
pub struct PortfolioGreeks {
    pub delta: f64,
    pub gamma: f64,
    pub n_positions: u32,
}

/// Tracks fills and recomputes aggregate Greeks on demand.
///
/// All fills in a market share the same (S, K, sigma, tau) since it's one binary
/// option. `delta_bin` and `gamma_bin` are computed once per recompute, then scaled
/// by `sign * size` per fill. Cost: 2 function calls + N multiply-accumulates.
pub struct GreeksTracker {
    positions: Vec<FillGreeksRecord>,
    /// Cached snapshot — recomputed on `recompute()`.
    pub snapshot: PortfolioGreeks,
}

impl GreeksTracker {
    pub fn new() -> Self {
        Self {
            positions: Vec::with_capacity(16),
            snapshot: PortfolioGreeks::default(),
        }
    }

    /// Record a new fill. Call `recompute()` after to update the snapshot.
    pub fn on_fill(&mut self, side: Side, size: f64) {
        self.positions.push(FillGreeksRecord { side, size });
        self.snapshot.n_positions = self.positions.len() as u32;
    }

    /// Recompute aggregate Greeks at current market conditions.
    /// Called on every Binance trade (when positions exist) and after every fill.
    pub fn recompute(&mut self, s: f64, k: f64, sigma: f64, tau: f64) {
        let unit_delta = delta_bin(s, k, sigma, tau);
        let unit_gamma = gamma_bin(s, k, sigma, tau);

        let mut total_delta = 0.0;
        let mut total_gamma = 0.0;

        for pos in &self.positions {
            let sign = match pos.side {
                Side::Up => 1.0,
                Side::Down => -1.0,
            };
            total_delta += sign * pos.size * unit_delta;
            total_gamma += sign * pos.size * unit_gamma;
        }

        self.snapshot = PortfolioGreeks {
            delta: total_delta,
            gamma: total_gamma,
            n_positions: self.positions.len() as u32,
        };
    }

    /// Reset at market end.
    pub fn reset(&mut self) {
        self.positions.clear();
        self.snapshot = PortfolioGreeks::default();
    }
}

/// Two-tier risk manager: per-strategy limits + portfolio-level caps.
/// Each strategy operates independently — one hitting its cap does not block others.
pub struct StrategyRiskManager {
    bankroll: f64,
    limits: HashMap<&'static str, StrategyLimits>,
    state: HashMap<&'static str, StrategyRiskState>,

    // Portfolio-level
    pub total_exposure: f64,
    max_total_exposure_frac: f64,
    pub daily_pnl: f64,
    daily_loss_halt_frac: f64,
    pub weekly_pnl: f64,
    weekly_loss_halt_frac: f64,
    pub halted_until_ms: i64,

    // Portfolio Greeks
    pub greeks: GreeksTracker,
    max_portfolio_delta: f64,
    max_portfolio_gamma_neg: f64,
}

impl StrategyRiskManager {
    pub fn new(config: &Config) -> Self {
        let mut limits = HashMap::new();

        // Cooldowns tuned for 300s (5-min) markets.
        // Target: 4-6 total orders per market. Each strategy gets 1-2 shots.
        // Portfolio cap (15% = $150) binds before individual caps sum.

        limits.insert(
            "latency_arb",
            StrategyLimits {
                max_per_trade_frac: 0.02,   // $20 per trade
                max_total_frac: 0.04,       // $40 total (2 orders)
                cooldown_ms: 60_000,        // 60s between orders
                max_orders_per_market: 2,
            },
        );
        limits.insert(
            "certainty_capture",
            StrategyLimits {
                max_per_trade_frac: 0.03,   // $30 per trade
                max_total_frac: 0.03,       // $30 total (1 order)
                cooldown_ms: 120_000,       // 120s — fires once, late in market
                max_orders_per_market: 1,
            },
        );
        limits.insert(
            "convexity_fade",
            StrategyLimits {
                max_per_trade_frac: 0.01,   // $10 per trade
                max_total_frac: 0.02,       // $20 total (2 orders)
                cooldown_ms: 60_000,        // 60s between orders
                max_orders_per_market: 2,
            },
        );
        limits.insert(
            "cross_timeframe",
            StrategyLimits {
                max_per_trade_frac: 0.005,
                max_total_frac: 0.02,
                cooldown_ms: 120_000,
                max_orders_per_market: 1,
            },
        );
        limits.insert(
            "strike_misalign",
            StrategyLimits {
                max_per_trade_frac: 0.02,   // $20 per trade
                max_total_frac: 0.04,       // $40 total (2 orders)
                cooldown_ms: 30_000,        // 30s — allows re-entry if edge persists
                max_orders_per_market: 2,
            },
        );
        limits.insert(
            "lp_extreme",
            StrategyLimits {
                max_per_trade_frac: 0.02,   // $20 per trade
                max_total_frac: 0.02,       // $20 total (1 order)
                cooldown_ms: 120_000,       // 120s — one tail risk shot
                max_orders_per_market: 1,
            },
        );

        let mut state = HashMap::new();
        for &name in limits.keys() {
            state.insert(name, StrategyRiskState::new());
        }

        Self {
            bankroll: config.bankroll,
            limits,
            state,
            total_exposure: 0.0,
            max_total_exposure_frac: config.max_total_exposure_frac,
            daily_pnl: 0.0,
            daily_loss_halt_frac: config.daily_loss_halt_frac,
            weekly_pnl: 0.0,
            weekly_loss_halt_frac: config.weekly_loss_halt_frac,
            halted_until_ms: 0,
            greeks: GreeksTracker::new(),
            max_portfolio_delta: config.max_portfolio_delta,
            max_portfolio_gamma_neg: config.max_portfolio_gamma_neg,
        }
    }

    /// Check if a strategy signal passes all risk gates and produce an Order.
    pub fn check_strategy(
        &self,
        signal: &Signal,
        state: &MarketState,
        order_id: u64,
        now_ms: i64,
    ) -> Option<Order> {
        // 1. Portfolio halt check
        if now_ms < self.halted_until_ms {
            return None;
        }

        // 2. Kill switch: daily loss
        if self.daily_pnl < self.daily_loss_halt_frac * self.bankroll {
            return None;
        }

        // 3. Kill switch: weekly loss
        if self.weekly_pnl < self.weekly_loss_halt_frac * self.bankroll {
            return None;
        }

        // 4. Kill switch: stale feeds
        if state.is_stale(now_ms) {
            return None;
        }

        // 5. Portfolio-level exposure check
        let max_portfolio = self.max_total_exposure_frac * self.bankroll;
        if self.total_exposure >= max_portfolio {
            return None;
        }

        // 5b. Portfolio delta limit (0.0 = disabled)
        if self.max_portfolio_delta > 0.0
            && self.greeks.snapshot.delta.abs() > self.max_portfolio_delta
        {
            return None;
        }

        // 5c. Portfolio negative gamma limit (0.0 = disabled)
        if self.max_portfolio_gamma_neg > 0.0
            && self.greeks.snapshot.gamma < -self.max_portfolio_gamma_neg
        {
            return None;
        }

        // 6. Per-strategy checks
        let limits = self.limits.get(signal.strategy)?;
        let strat_state = self.state.get(signal.strategy)?;

        // Per-strategy cooldown
        if strat_state.last_order_ms > 0
            && now_ms - strat_state.last_order_ms < limits.cooldown_ms
        {
            return None;
        }

        // Per-strategy max orders
        if strat_state.orders_this_market >= limits.max_orders_per_market {
            return None;
        }

        // Per-strategy exposure limit
        let max_strat_exposure = limits.max_total_frac * self.bankroll;
        if strat_state.exposure >= max_strat_exposure {
            return None;
        }

        // 7. Compute size
        let kelly_size = signal.size_frac * self.bankroll;
        let per_trade_cap = limits.max_per_trade_frac * self.bankroll;
        let strat_room = max_strat_exposure - strat_state.exposure;
        let portfolio_room = max_portfolio - self.total_exposure;

        let size = kelly_size
            .min(per_trade_cap)
            .min(strat_room)
            .min(portfolio_room);

        if size < 1.0 {
            return None;
        }

        // Determine order type and execution parameters:
        // - lp_extreme (is_passive): GTC post_only (unchanged)
        // - convexity_fade, strike_misalign (use_bid): GTD at bid, post_only, 10s TTL
        // - latency_arb: FOK (latency race, needs instant fill-or-kill)
        // - certainty_capture, cross_timeframe (others): GTD at ask, 10s TTL
        let (order_type, post_only, expiration_ms) = if signal.is_passive {
            (OrderType::GTC, true, None)
        } else if signal.use_bid {
            (OrderType::GTD, true, Some(now_ms + 10_000))
        } else if signal.strategy == "latency_arb" {
            (OrderType::FOK, false, None)
        } else {
            (OrderType::GTD, false, Some(now_ms + 10_000))
        };

        Some(Order {
            id: order_id,
            side: signal.side,
            price: signal.market_price,
            size,
            strategy: signal.strategy,
            signal_edge: signal.edge,
            is_passive: signal.is_passive,
            created_at: Instant::now(),
            order_type,
            post_only,
            expiration_ms,
            token_id: String::new(), // set by LiveSink::on_order from MarketInfo
        })
    }

    pub fn on_order_sent(&mut self, strategy: &'static str, now_ms: i64, size: f64) {
        if let Some(s) = self.state.get_mut(strategy) {
            s.last_order_ms = now_ms;
            s.orders_this_market += 1;
            s.exposure += size;
        }
        self.total_exposure += size;
    }

    pub fn on_fill(&mut self, _strategy: &str, _ack: &OrderAck) {
        // PnL is computed at settlement — not at fill time.
        // Fills are tracked in runner.rs and settled with settle_market().
    }

    /// Settle PnL at market end. Called once per market with the known outcome.
    pub fn settle_market(&mut self, outcome: Side, fills: &[Fill]) {
        let mut market_pnl = 0.0;
        for fill in fills {
            let pnl = if fill.side == outcome {
                (1.0 - fill.price) * fill.size
            } else {
                -(fill.price * fill.size)
            };
            market_pnl += pnl;
        }
        self.daily_pnl += market_pnl;
        self.weekly_pnl += market_pnl;
        // Reset per-market exposure for next market
        self.total_exposure = 0.0;
        self.greeks.reset();
        for s in self.state.values_mut() {
            *s = StrategyRiskState::new();
        }
    }

    pub fn trigger_halt(&mut self, now_ms: i64, duration_ms: i64) {
        self.halted_until_ms = now_ms + duration_ms;
        eprintln!(
            "[RISK] HALT triggered until +{}ms (total_exp=${:.0}, daily_pnl=${:.2})",
            duration_ms, self.total_exposure, self.daily_pnl
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::test_helpers::*;

    fn make_signal(strategy: &'static str, edge: f64, price: f64, size_frac: f64) -> Signal {
        Signal {
            strategy,
            side: Side::Up,
            edge,
            fair_value: price + edge,
            market_price: price,
            confidence: 0.8,
            size_frac,
            is_passive: false,
            use_bid: false,
        }
    }

    /// Scenario: Valid latency_arb signal with fresh feeds, no prior orders, within all limits.
    /// Expected: Order is approved with size between $1 floor and $20 per-trade cap.
    #[test]
    fn test_order_approved_happy_path() {
        let config = make_config();
        let risk = StrategyRiskManager::new(&config);
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        let order = risk.check_strategy(&signal, &state, 1, now);
        assert!(order.is_some(), "Valid signal should produce an order");
        let order = order.unwrap();
        assert!(order.size >= 1.0, "Order size should be at least $1: {}", order.size);
        assert!(order.size <= 20.0, "Order size capped at per_trade_frac * bankroll = $20: {}", order.size);
    }

    /// Scenario: Portfolio halt triggered for 60s; signal arrives 1s later.
    /// Expected: Order rejected because halt has not yet expired (gate 1).
    #[test]
    fn test_halt_blocks_order() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        risk.trigger_halt(now, 60_000);
        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        assert!(risk.check_strategy(&signal, &state, 1, now + 1000).is_none());
    }

    /// Scenario: 1s halt triggered; check at +500ms (still halted) and +2000ms (expired).
    /// Expected: Blocked while halted, approved after halt expires.
    #[test]
    fn test_halt_expires() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        risk.trigger_halt(now, 1000);
        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        // Still halted at now + 500
        assert!(risk.check_strategy(&signal, &state, 1, now + 500).is_none());
        // Halt expired at now + 2000 — update feed timestamps so they're not stale
        state.bn.binance_ts = now + 2000;
        state.pm_last_ts = now + 2000;
        assert!(risk.check_strategy(&signal, &state, 2, now + 2000).is_some());
    }

    /// Scenario: daily_pnl set to -$50, exceeding the -$30 daily loss threshold.
    /// Expected: Order rejected by the daily loss kill switch (gate 2).
    #[test]
    fn test_daily_loss_blocks() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        // daily_loss_halt_frac = -0.03, bankroll = 1000 → threshold = -30
        risk.daily_pnl = -50.0;
        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        assert!(risk.check_strategy(&signal, &state, 1, now).is_none());
    }

    /// Scenario: weekly_pnl set to -$100, exceeding the -$80 weekly loss threshold.
    /// Expected: Order rejected by the weekly loss kill switch (gate 3).
    #[test]
    fn test_weekly_loss_blocks() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        // weekly_loss_halt_frac = -0.08, bankroll = 1000 → threshold = -80
        risk.weekly_pnl = -100.0;
        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        assert!(risk.check_strategy(&signal, &state, 1, now).is_none());
    }

    /// Scenario: Binance timestamp set to 6s behind now (>1s staleness threshold).
    /// Expected: Order rejected by the stale feed kill switch (gate 4).
    #[test]
    fn test_stale_feed_blocks() {
        let config = make_config();
        let risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        // Make binance data stale (> 1000ms old)
        state.bn.binance_ts = now - 6000;
        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        assert!(risk.check_strategy(&signal, &state, 1, now).is_none());
    }

    /// Scenario: total_exposure already at $150 (= 15% cap of $1000 bankroll).
    /// Expected: Order rejected by portfolio-level exposure cap (gate 5).
    #[test]
    fn test_portfolio_cap_blocks() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        // max_total_exposure_frac = 0.15, bankroll = 1000 → cap = 150
        risk.total_exposure = 150.0;
        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        assert!(risk.check_strategy(&signal, &state, 1, now).is_none());
    }

    /// Scenario: Signal references "bogus_strategy" which has no registered limits.
    /// Expected: Order rejected because unknown strategies have no limits entry (gate 6).
    #[test]
    fn test_unknown_strategy_blocks() {
        let config = make_config();
        let risk = StrategyRiskManager::new(&config);
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        let signal = make_signal("bogus_strategy", 0.05, 0.50, 0.01);
        assert!(risk.check_strategy(&signal, &state, 1, now).is_none());
    }

    /// Scenario: latency_arb order sent at now; second signal arrives 1s later (cooldown is 60s).
    /// Expected: Order rejected because strategy is still in cooldown (gate 7).
    #[test]
    fn test_cooldown_blocks() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        risk.on_order_sent("latency_arb", now, 10.0);
        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        // cooldown for latency_arb = 60_000ms, check at now + 1000 → still cooling
        assert!(risk.check_strategy(&signal, &state, 2, now + 1000).is_none());
    }

    /// Scenario: Two fills -- one winning (Up bet, Up outcome) and one losing (Down bet, Up outcome).
    /// Expected: PnL nets to $0 (+$4 win - $4 loss); exposure resets after settlement.
    #[test]
    fn test_settle_market_pnl() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        risk.on_order_sent("latency_arb", 0, 10.0);
        risk.on_order_sent("certainty_capture", 0, 10.0);

        let fills = vec![
            Fill {
                order_id: 1,
                strategy: "latency_arb",
                side: Side::Up,
                price: 0.60,
                size: 10.0,
            },
            Fill {
                order_id: 2,
                strategy: "certainty_capture",
                side: Side::Down,
                price: 0.40,
                size: 10.0,
            },
        ];

        // Outcome is Up
        risk.settle_market(Side::Up, &fills);

        // Fill 1: side=Up, outcome=Up → pnl = (1 - 0.60) * 10 = 4.0
        // Fill 2: side=Down, outcome=Up → pnl = -(0.40 * 10) = -4.0
        // Total = 0.0
        assert!((risk.daily_pnl - 0.0).abs() < 1e-10, "Daily PnL: {}", risk.daily_pnl);
        assert!((risk.weekly_pnl - 0.0).abs() < 1e-10, "Weekly PnL: {}", risk.weekly_pnl);
        assert_eq!(risk.total_exposure, 0.0, "Exposure should be reset after settle");
    }

    // ── Max orders per market gate ──

    /// Scenario: Two latency_arb orders already sent (max_orders_per_market = 2); third attempted.
    /// Expected: Third order rejected by the per-strategy max orders gate (gate 8).
    #[test]
    fn test_max_orders_per_market_blocks() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        // latency_arb allows max 2 orders per market
        risk.on_order_sent("latency_arb", now, 5.0);
        risk.on_order_sent("latency_arb", now + 61_000, 5.0); // past cooldown

        // Update feed timestamps so staleness doesn't mask the real gate
        let check_time = now + 122_000;
        state.bn.binance_ts = check_time;
        state.pm_last_ts = check_time;

        // 3rd order should be blocked (max_orders_per_market = 2)
        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        assert!(risk.check_strategy(&signal, &state, 3, check_time).is_none(),
            "3rd order should be blocked by max_orders_per_market");
    }

    // ── Per-strategy exposure cap ──

    /// Scenario: latency_arb already at $40 exposure (= 4% of $1000 bankroll cap).
    /// Expected: Next order rejected by per-strategy exposure limit (gate 9).
    #[test]
    fn test_per_strategy_exposure_cap_blocks() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        // latency_arb max_total_frac = 0.04 * bankroll(1000) = $40
        risk.on_order_sent("latency_arb", now, 40.0);

        // Update feed timestamps past cooldown
        let check_time = now + 61_000;
        state.bn.binance_ts = check_time;
        state.pm_last_ts = check_time;

        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        // Past cooldown but at exposure cap
        assert!(risk.check_strategy(&signal, &state, 2, check_time).is_none(),
            "Should be blocked by per-strategy exposure cap");
    }

    // ── Size floor ($1 minimum) ──

    /// Scenario: Signal with size_frac = 0.0001 producing a $0.10 order (below $1 floor).
    /// Expected: Order rejected by the minimum size floor (gate 10).
    #[test]
    fn test_size_floor_blocks_tiny_order() {
        let config = make_config();
        let risk = StrategyRiskManager::new(&config);
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        // Very small size_frac → kelly_size < $1 → None
        let signal = make_signal("latency_arb", 0.001, 0.50, 0.0001);
        // size_frac * bankroll = 0.0001 * 1000 = $0.10 < $1
        assert!(risk.check_strategy(&signal, &state, 1, now).is_none(),
            "Sub-dollar order should be rejected");
    }

    // ── Settle with empty fills ──

    /// Scenario: Market settled with an empty fills slice after accumulating $10 exposure.
    /// Expected: PnL stays at $0, exposure resets to $0, strategy state cleared.
    #[test]
    fn test_settle_empty_fills() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        risk.on_order_sent("latency_arb", 0, 10.0);
        assert_eq!(risk.total_exposure, 10.0);

        risk.settle_market(Side::Up, &[]);
        assert_eq!(risk.daily_pnl, 0.0, "Empty fills → zero PnL");
        assert_eq!(risk.total_exposure, 0.0, "Exposure should reset");
    }

    // ── Settle PnL accumulation across multiple markets ──

    /// Scenario: Two sequential markets -- first a +$6 win, then a -$6 loss.
    /// Expected: daily_pnl accumulates across markets: $6 after win, $0 after loss.
    #[test]
    fn test_settle_pnl_accumulates() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);

        // Market 1: Win
        let fills1 = vec![Fill {
            order_id: 1,
            strategy: "latency_arb",
            side: Side::Up,
            price: 0.40,
            size: 10.0,
        }];
        risk.settle_market(Side::Up, &fills1);
        // PnL = (1 - 0.40) * 10 = 6.0
        assert!((risk.daily_pnl - 6.0).abs() < 1e-10, "After win: {}", risk.daily_pnl);

        // Market 2: Lose
        let fills2 = vec![Fill {
            order_id: 2,
            strategy: "latency_arb",
            side: Side::Up,
            price: 0.60,
            size: 10.0,
        }];
        risk.settle_market(Side::Down, &fills2);
        // PnL = -(0.60 * 10) = -6.0 → cumulative = 6.0 + (-6.0) = 0.0
        assert!((risk.daily_pnl - 0.0).abs() < 1e-10, "After loss: {}", risk.daily_pnl);
    }

    // ── Settle all winning fills ──

    /// Scenario: Two fills both on the winning side (Up bets, Up outcome).
    /// Expected: Both contribute positive PnL; daily and weekly totals equal $19.
    #[test]
    fn test_settle_all_winning() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);

        let fills = vec![
            Fill { order_id: 1, strategy: "latency_arb", side: Side::Up, price: 0.30, size: 20.0 },
            Fill { order_id: 2, strategy: "certainty_capture", side: Side::Up, price: 0.50, size: 10.0 },
        ];
        risk.settle_market(Side::Up, &fills);

        // Fill 1: (1 - 0.30) * 20 = 14.0
        // Fill 2: (1 - 0.50) * 10 = 5.0
        let expected = 14.0 + 5.0;
        assert!((risk.daily_pnl - expected).abs() < 1e-10, "All winning PnL: {}", risk.daily_pnl);
        assert!((risk.weekly_pnl - expected).abs() < 1e-10);
    }

    // ── Settle all losing fills ──

    /// Scenario: Two fills both on the losing side (Up bets, Down outcome).
    /// Expected: Both contribute negative PnL; daily total equals -$13.
    #[test]
    fn test_settle_all_losing() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);

        let fills = vec![
            Fill { order_id: 1, strategy: "latency_arb", side: Side::Up, price: 0.60, size: 15.0 },
            Fill { order_id: 2, strategy: "certainty_capture", side: Side::Up, price: 0.40, size: 10.0 },
        ];
        risk.settle_market(Side::Down, &fills);

        // Fill 1: -(0.60 * 15) = -9.0
        // Fill 2: -(0.40 * 10) = -4.0
        let expected = -9.0 + -4.0;
        assert!((risk.daily_pnl - expected).abs() < 1e-10, "All losing PnL: {}", risk.daily_pnl);
    }

    // ── Size capping by room ──

    /// Scenario: Portfolio exposure at $145 with $150 cap; signal wants $20.
    /// Expected: Order approved but size clamped to $5 (remaining portfolio room).
    #[test]
    fn test_order_size_capped_by_portfolio_room() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        // Portfolio cap = 0.15 * 1000 = 150. Set exposure to 145 → only $5 room
        risk.total_exposure = 145.0;

        let signal = make_signal("latency_arb", 0.05, 0.50, 0.02); // wants 0.02 * 1000 = $20
        let order = risk.check_strategy(&signal, &state, 1, now);
        assert!(order.is_some(), "Should still approve with room");
        let order = order.unwrap();
        assert!(order.size <= 5.0, "Size capped by portfolio room: {}", order.size);
    }

    /// Scenario: latency_arb at $35 exposure with $40 strategy cap; signal wants $20.
    /// Expected: Order approved but size clamped to $5 (remaining strategy room).
    #[test]
    fn test_order_size_capped_by_strategy_room() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        // latency_arb max_total_frac = 0.04 → $40. Spend $35 → $5 room
        risk.on_order_sent("latency_arb", now - 120_000, 35.0); // past cooldown

        let signal = make_signal("latency_arb", 0.05, 0.50, 0.02); // wants $20
        let order = risk.check_strategy(&signal, &state, 2, now);
        assert!(order.is_some(), "Should approve with strategy room");
        let order = order.unwrap();
        assert!(order.size <= 5.0, "Size capped by strategy room: {}", order.size);
    }

    // ── Cooldown expires ──

    /// Scenario: latency_arb order sent at now; second signal checked at now + 61s (cooldown is 60s).
    /// Expected: Order approved because the cooldown period has expired.
    #[test]
    fn test_cooldown_expires() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
        risk.on_order_sent("latency_arb", now, 10.0);

        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        // Advance feed timestamps so they're not stale at now + 61_000
        state.bn.binance_ts = now + 61_000;
        state.pm_last_ts = now + 61_000;
        // latency_arb cooldown = 60_000ms. At now + 61_000 → cooldown expired
        assert!(risk.check_strategy(&signal, &state, 2, now + 61_000).is_some(),
            "Order should be approved after cooldown expires");
    }

    // ── on_order_sent updates both per-strategy and portfolio ──

    /// Scenario: Two orders sent on different strategies ($15 + $25).
    /// Expected: Portfolio total_exposure accumulates to $40 across both strategies.
    #[test]
    fn test_on_order_sent_updates_exposure() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);

        risk.on_order_sent("latency_arb", 1000, 15.0);
        assert_eq!(risk.total_exposure, 15.0);

        risk.on_order_sent("certainty_capture", 2000, 25.0);
        assert_eq!(risk.total_exposure, 40.0);
    }

    // ── Independent strategy limits ──

    /// Scenario: latency_arb filled to its $40 exposure cap; certainty_capture signal arrives.
    /// Expected: latency_arb is blocked but certainty_capture passes (independent strategy limits).
    #[test]
    fn test_one_strategy_cap_doesnt_block_another() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        // Fill up latency_arb (max_total_frac=0.04 → $40)
        risk.on_order_sent("latency_arb", now, 40.0);

        // latency_arb should be blocked (at now — not stale, exposure cap hit)
        let signal_la = make_signal("latency_arb", 0.05, 0.50, 0.01);
        assert!(risk.check_strategy(&signal_la, &state, 2, now).is_none());

        // certainty_capture should still work (independent limits)
        let signal_cc = make_signal("certainty_capture", 0.05, 0.50, 0.01);
        assert!(risk.check_strategy(&signal_cc, &state, 3, now).is_some(),
            "Certainty capture should not be blocked by latency_arb cap");
    }

    // ── Portfolio Greeks tests ──

    /// Scenario: Fresh GreeksTracker with no fills.
    /// Expected: Snapshot is all zeros.
    #[test]
    fn test_greeks_tracker_empty() {
        let tracker = GreeksTracker::new();
        assert_eq!(tracker.snapshot.delta, 0.0);
        assert_eq!(tracker.snapshot.gamma, 0.0);
        assert_eq!(tracker.snapshot.n_positions, 0);
    }

    /// Scenario: One UP fill of $10, recomputed at ATM (S=K=100000).
    /// Expected: Portfolio delta is positive (long the binary call).
    #[test]
    fn test_greeks_tracker_single_up_fill() {
        let mut tracker = GreeksTracker::new();
        tracker.on_fill(Side::Up, 10.0);
        // ATM with sigma=0.001/s, tau=300s
        tracker.recompute(100_000.0, 100_000.0, 0.001, 300.0);
        assert!(tracker.snapshot.delta > 0.0,
            "UP fill should produce positive delta: {}", tracker.snapshot.delta);
        assert_eq!(tracker.snapshot.n_positions, 1);
    }

    /// Scenario: Equal-size UP and DOWN fills ($10 each), recomputed at ATM.
    /// Expected: Delta and gamma cancel to near-zero (opposing positions).
    #[test]
    fn test_greeks_tracker_opposing_fills_cancel() {
        let mut tracker = GreeksTracker::new();
        tracker.on_fill(Side::Up, 10.0);
        tracker.on_fill(Side::Down, 10.0);
        tracker.recompute(100_000.0, 100_000.0, 0.001, 300.0);
        assert!(tracker.snapshot.delta.abs() < 1e-12,
            "Opposing fills should cancel delta: {}", tracker.snapshot.delta);
        assert!(tracker.snapshot.gamma.abs() < 1e-12,
            "Opposing fills should cancel gamma: {}", tracker.snapshot.gamma);
        assert_eq!(tracker.snapshot.n_positions, 2);
    }

    /// Scenario: One UP fill, recompute at S=100000 then at S=100500.
    /// Expected: Delta changes because delta_bin is a function of spot price.
    #[test]
    fn test_greeks_tracker_recompute_varies_with_s() {
        let mut tracker = GreeksTracker::new();
        tracker.on_fill(Side::Up, 10.0);

        tracker.recompute(100_000.0, 100_000.0, 0.001, 300.0);
        let delta_at_atm = tracker.snapshot.delta;

        tracker.recompute(100_500.0, 100_000.0, 0.001, 300.0);
        let delta_itm = tracker.snapshot.delta;

        assert!((delta_at_atm - delta_itm).abs() > 1e-10,
            "Delta should change with S: atm={} itm={}", delta_at_atm, delta_itm);
    }

    /// Scenario: Add a fill, recompute, then reset.
    /// Expected: After reset, snapshot is all zeros and positions vec is empty.
    #[test]
    fn test_greeks_tracker_reset_clears() {
        let mut tracker = GreeksTracker::new();
        tracker.on_fill(Side::Up, 10.0);
        tracker.recompute(100_000.0, 100_000.0, 0.001, 300.0);
        assert!(tracker.snapshot.delta != 0.0, "Should have nonzero delta before reset");

        tracker.reset();
        assert_eq!(tracker.snapshot.delta, 0.0);
        assert_eq!(tracker.snapshot.gamma, 0.0);
        assert_eq!(tracker.snapshot.n_positions, 0);
    }

    /// Scenario: max_portfolio_delta set to a tiny value; delta pushed past it via greeks.recompute.
    /// Expected: check_strategy returns None (gate 5b blocks).
    #[test]
    fn test_delta_limit_blocks_order() {
        let mut config = make_config();
        config.max_portfolio_delta = 0.0001; // very tight limit
        let mut risk = StrategyRiskManager::new(&config);
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        // Simulate a fill that pushes delta past the limit
        risk.greeks.on_fill(Side::Up, 100.0);
        risk.greeks.recompute(95_500.0, 95_000.0, 0.001, 120.0);
        assert!(risk.greeks.snapshot.delta.abs() > 0.0001,
            "Delta should exceed limit: {}", risk.greeks.snapshot.delta);

        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        assert!(risk.check_strategy(&signal, &state, 1, now).is_none(),
            "Should be blocked by portfolio delta limit");
    }

    /// Scenario: max_portfolio_gamma_neg set to a small value; gamma made sufficiently negative.
    /// Expected: check_strategy returns None (gate 5c blocks).
    #[test]
    fn test_gamma_limit_blocks_order() {
        let mut config = make_config();
        config.max_portfolio_gamma_neg = 1e-12; // very tight limit
        let mut risk = StrategyRiskManager::new(&config);
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        // Deep ITM: gamma_bin is negative. UP fill with positive sign * negative gamma = negative.
        // Actually, for S > K (ITM), gamma_bin is negative.
        // UP fill: sign=+1, so total_gamma = +1 * size * gamma_bin (negative) = negative.
        risk.greeks.on_fill(Side::Up, 100.0);
        risk.greeks.recompute(100_000.0, 95_000.0, 0.001, 120.0);
        assert!(risk.greeks.snapshot.gamma < 0.0,
            "ITM UP fill should produce negative portfolio gamma: {}", risk.greeks.snapshot.gamma);

        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        assert!(risk.check_strategy(&signal, &state, 1, now).is_none(),
            "Should be blocked by portfolio gamma limit");
    }

    /// Scenario: Greeks limits at default (0.0 = disabled), with active fills.
    /// Expected: check_strategy still approves (limits disabled by default).
    #[test]
    fn test_greeks_limits_disabled_by_default() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

        // Add a fill with nonzero Greeks
        risk.greeks.on_fill(Side::Up, 100.0);
        risk.greeks.recompute(95_500.0, 95_000.0, 0.001, 120.0);
        assert!(risk.greeks.snapshot.delta.abs() > 0.0, "Should have nonzero delta");

        // Default limits are 0.0 = disabled, so order should pass
        let signal = make_signal("latency_arb", 0.05, 0.50, 0.01);
        assert!(risk.check_strategy(&signal, &state, 1, now).is_some(),
            "Order should pass when Greeks limits are disabled (0.0)");
    }

    /// Scenario: settle_market resets Greeks tracker.
    /// Expected: After settlement, greeks snapshot is zeroed out.
    #[test]
    fn test_settle_resets_greeks() {
        let config = make_config();
        let mut risk = StrategyRiskManager::new(&config);

        risk.greeks.on_fill(Side::Up, 10.0);
        risk.greeks.recompute(100_000.0, 100_000.0, 0.001, 300.0);
        assert!(risk.greeks.snapshot.n_positions > 0);

        risk.settle_market(Side::Up, &[]);
        assert_eq!(risk.greeks.snapshot.n_positions, 0, "Greeks should reset after settle");
        assert_eq!(risk.greeks.snapshot.delta, 0.0);
        assert_eq!(risk.greeks.snapshot.gamma, 0.0);
    }
}
