use std::collections::HashMap;
use std::time::Instant;

use crate::config::Config;
use crate::engine::state::MarketState;
use crate::types::{Fill, Order, OrderAck, Side, Signal};

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
                max_total_frac: 0.02,       // $20 total (1 order)
                cooldown_ms: 15_000,        // 15s — only fires once in 15s window
                max_orders_per_market: 1,
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

        Some(Order {
            id: order_id,
            side: signal.side,
            price: signal.market_price,
            size,
            strategy: signal.strategy,
            signal_edge: signal.edge,
            is_passive: signal.is_passive,
            created_at: Instant::now(),
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
