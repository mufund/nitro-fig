use std::collections::{HashMap, VecDeque};

use crate::types::{BinanceTrade, MarketInfo, OrderAck, OrderStatus, PolymarketQuote, Side};

/// Per-strategy performance counters, accumulated during a single market.
pub struct StrategyStats {
    pub signals: u32,
    pub orders: u32,
    pub filled: u32,
    pub gross_pnl: f64,
    pub total_edge: f64, // sum of edges at order time (for avg_edge calc)
}

impl StrategyStats {
    pub fn new() -> Self {
        Self {
            signals: 0,
            orders: 0,
            filled: 0,
            gross_pnl: 0.0,
            total_edge: 0.0,
        }
    }

    pub fn avg_edge(&self) -> f64 {
        if self.orders > 0 {
            self.total_edge / self.orders as f64
        } else {
            0.0
        }
    }
}

/// Owned by the engine task — no Arc, no RwLock, no shared references.
pub struct MarketState {
    pub info: MarketInfo,
    // Binance
    pub binance_price: f64,
    pub binance_ts: i64,
    pub trade_buffer: VecDeque<BinanceTrade>,
    // Polymarket
    pub up_bid: f64,
    pub up_ask: f64,
    pub down_bid: f64,
    pub down_ask: f64,
    pub pm_last_ts: i64,
    // Position tracking
    pub position: PositionTracker,
    // Stats (aggregate)
    pub total_signals: u32,
    pub total_orders: u32,
    pub total_filled: u32,
    pub gross_pnl: f64,
    // Stats (per-strategy)
    pub strategy_stats: HashMap<&'static str, StrategyStats>,
}

impl MarketState {
    pub fn new(info: MarketInfo) -> Self {
        Self {
            info,
            binance_price: 0.0,
            binance_ts: 0,
            trade_buffer: VecDeque::with_capacity(2000),
            up_bid: 0.0,
            up_ask: 0.0,
            down_bid: 0.0,
            down_ask: 0.0,
            pm_last_ts: 0,
            position: PositionTracker::new(),
            total_signals: 0,
            total_orders: 0,
            total_filled: 0,
            gross_pnl: 0.0,
            strategy_stats: HashMap::new(),
        }
    }

    #[inline]
    pub fn on_binance_trade(&mut self, t: BinanceTrade) {
        self.binance_price = t.price;
        self.binance_ts = t.exchange_ts_ms;
        self.trade_buffer.push_back(t);
        // Evict trades older than 30s
        let cutoff = self.binance_ts - 30_000;
        while self
            .trade_buffer
            .front()
            .map_or(false, |t| t.exchange_ts_ms < cutoff)
        {
            self.trade_buffer.pop_front();
        }
    }

    #[inline]
    pub fn on_polymarket_quote(&mut self, q: PolymarketQuote) {
        if let Some(v) = q.up_bid {
            self.up_bid = v;
        }
        if let Some(v) = q.up_ask {
            self.up_ask = v;
        }
        if let Some(v) = q.down_bid {
            self.down_bid = v;
        }
        if let Some(v) = q.down_ask {
            self.down_ask = v;
        }
        self.pm_last_ts = q.server_ts_ms;
    }

    #[inline]
    pub fn time_left_s(&self, now_ms: i64) -> f64 {
        ((self.info.end_ms - now_ms).max(0)) as f64 / 1000.0
    }

    #[inline]
    pub fn distance(&self) -> f64 {
        self.binance_price - self.info.strike
    }

    /// Distance as fraction of strike. Scales across any asset price.
    /// E.g. $30 distance at $68000 BTC = 0.000441, $0.066 at $150 SOL = 0.000441.
    #[inline]
    pub fn distance_frac(&self) -> f64 {
        if self.info.strike > 0.0 {
            self.distance() / self.info.strike
        } else {
            0.0
        }
    }

    pub fn is_stale(&self, now_ms: i64) -> bool {
        (self.binance_ts > 0 && now_ms - self.binance_ts > 5000)
            || (self.pm_last_ts > 0 && now_ms - self.pm_last_ts > 5000)
    }

    pub fn has_data(&self) -> bool {
        self.binance_price > 0.0 && (self.up_ask > 0.0 || self.down_ask > 0.0)
    }
}

pub struct PositionTracker {
    pub side: Option<Side>,
    pub size: f64,
    pub avg_price: f64,
    pub pending_orders: u32,
}

impl PositionTracker {
    pub fn new() -> Self {
        Self {
            side: None,
            size: 0.0,
            avg_price: 0.0,
            pending_orders: 0,
        }
    }

    pub fn on_order_sent(&mut self) {
        self.pending_orders += 1;
    }

    pub fn on_fill(&mut self, ack: &OrderAck) {
        if self.pending_orders > 0 {
            self.pending_orders -= 1;
        }
        match ack.status {
            OrderStatus::Filled | OrderStatus::PartialFill => {
                if let (Some(price), Some(size)) = (ack.filled_price, ack.filled_size) {
                    let total = self.size + size;
                    if total > 0.0 {
                        self.avg_price = (self.avg_price * self.size + price * size) / total;
                    }
                    self.size = total;
                }
            }
            _ => {}
        }
    }
}
