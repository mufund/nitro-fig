use std::time::Instant;

use crate::config::Config;
use crate::engine::state::MarketState;
use crate::types::{Order, OrderAck, OrderStatus, Signal};

/// Risk manager: enforces position limits, max orders, cooldowns.
pub struct RiskManager {
    pub max_position_usd: f64,
    pub max_orders_per_market: u32,
    pub cooldown_ms: i64,
    pub last_order_ms: i64,
    pub orders_this_market: u32,
    pub filled_this_market: u32,
}

impl RiskManager {
    pub fn new(config: &Config) -> Self {
        Self {
            max_position_usd: config.max_position_usd,
            max_orders_per_market: config.max_orders_per_market,
            cooldown_ms: config.cooldown_ms,
            last_order_ms: 0,
            orders_this_market: 0,
            filled_this_market: 0,
        }
    }

    pub fn check(
        &self,
        signal: &Signal,
        state: &MarketState,
        order_id: u64,
        now_ms: i64,
    ) -> Option<Order> {
        // Cooldown
        if self.last_order_ms > 0 && now_ms - self.last_order_ms < self.cooldown_ms {
            return None;
        }

        // Max orders per market
        if self.orders_this_market >= self.max_orders_per_market {
            return None;
        }

        // Position limit
        let current_exposure = state.position.size;
        let max_add = (self.max_position_usd - current_exposure).max(0.0);
        if max_add < 1.0 {
            return None;
        }

        // Size: Kelly fraction of bankroll, capped by remaining room
        let size = (signal.size_frac * self.max_position_usd).min(max_add);
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
            created_at: Instant::now(),
        })
    }

    pub fn on_order_sent(&mut self, now_ms: i64) {
        self.last_order_ms = now_ms;
        self.orders_this_market += 1;
    }

    pub fn on_fill(&mut self, ack: &OrderAck) {
        match ack.status {
            OrderStatus::Filled | OrderStatus::PartialFill => {
                self.filled_this_market += 1;
            }
            _ => {}
        }
    }
}
