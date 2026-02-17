use std::collections::{HashMap, VecDeque};

use crate::config::Interval;
use crate::math::ewma::SampledEwmaVol;
use crate::math::oracle::OracleBasis;
use crate::math::regime::RegimeClassifier;
use crate::math::vwap::VwapTracker;
use crate::types::{
    BinanceTrade, CrossMarketQuoteEvent, MarketInfo, OrderAck, OrderStatus, PolymarketBook,
    PolymarketQuote, Side,
};

/// Per-strategy performance counters, accumulated during a single market.
pub struct StrategyStats {
    pub signals: u32,
    pub orders: u32,
    pub filled: u32,
    pub gross_pnl: f64,
    pub total_edge: f64,
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

/// Sorted orderbook (one side: bids or asks).
pub struct OrderBook {
    pub bids: Vec<(f64, f64)>,
    pub asks: Vec<(f64, f64)>,
}

impl OrderBook {
    pub fn new() -> Self {
        Self {
            bids: Vec::with_capacity(20),
            asks: Vec::with_capacity(20),
        }
    }

    #[inline]
    pub fn best_bid(&self) -> f64 {
        self.bids.first().map_or(0.0, |(p, _)| *p)
    }

    #[inline]
    pub fn best_ask(&self) -> f64 {
        self.asks.first().map_or(0.0, |(p, _)| *p)
    }

    #[inline]
    pub fn mid(&self) -> f64 {
        let b = self.best_bid();
        let a = self.best_ask();
        if b > 0.0 && a > 0.0 {
            (b + a) / 2.0
        } else {
            0.0
        }
    }

    #[inline]
    pub fn spread(&self) -> f64 {
        let a = self.best_ask();
        let b = self.best_bid();
        if a > 0.0 && b > 0.0 {
            a - b
        } else {
            0.0
        }
    }

    pub fn bid_depth(&self, levels: usize) -> f64 {
        self.bids.iter().take(levels).map(|(_, s)| s).sum()
    }

    pub fn ask_depth(&self, levels: usize) -> f64 {
        self.asks.iter().take(levels).map(|(_, s)| s).sum()
    }

    pub fn apply_snapshot(&mut self, bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)>) {
        self.bids = bids;
        self.asks = asks;
    }
}

/// Cross-market state for Edge 4 (cross-timeframe RV).
pub struct CrossMarketState {
    pub interval: Interval,
    pub up_bid: f64,
    pub up_ask: f64,
    pub down_bid: f64,
    pub down_ask: f64,
    pub strike: f64,
    pub end_ms: i64,
}

/// Persistent Binance-derived state that survives across markets.
/// Created once at startup, threaded through each market cycle.
/// Market 1 warms in ~10s. Market 2+ starts instantly.
pub struct BinanceState {
    pub ewma_vol: SampledEwmaVol,
    pub trade_buffer: VecDeque<BinanceTrade>,
    pub binance_price: f64,
    pub binance_ts: i64,
    pub prev_binance_price: f64,
    pub vwap_tracker: VwapTracker,
    pub regime: RegimeClassifier,
    /// Cached sigma_real (updated once per second when EWMA samples).
    pub sigma_real_cached: f64,
    /// Per-second sigma floor.
    sigma_floor_per_sec: f64,
}

impl BinanceState {
    pub fn new(
        ewma_lambda: f64,
        ewma_min_samples: u32,
        sigma_floor_annual: f64,
        vwap_window_ms: i64,
        regime_window_ms: i64,
    ) -> Self {
        let secs_per_year: f64 = 365.25 * 24.0 * 3600.0;
        let sigma_floor_per_sec = sigma_floor_annual / secs_per_year.sqrt();
        Self {
            ewma_vol: SampledEwmaVol::new(ewma_lambda, ewma_min_samples),
            trade_buffer: VecDeque::with_capacity(2000),
            binance_price: 0.0,
            binance_ts: 0,
            prev_binance_price: 0.0,
            vwap_tracker: VwapTracker::new(vwap_window_ms),
            regime: RegimeClassifier::new(regime_window_ms),
            sigma_real_cached: 0.0,
            sigma_floor_per_sec,
        }
    }
}

/// Owned by the engine task — no Arc, no RwLock, no shared references.
pub struct MarketState {
    pub info: MarketInfo,
    // ── Binance-derived (persistent across markets) ──
    pub bn: BinanceState,
    // ── Polymarket (per-market, fresh each cycle) ──
    pub up_bid: f64,
    pub up_ask: f64,
    pub down_bid: f64,
    pub down_ask: f64,
    pub pm_last_ts: i64,
    pub up_book: OrderBook,
    pub down_book: OrderBook,
    // Quantitative
    pub oracle: OracleBasis,
    // Cross-timeframe markets (Edge 4)
    pub cross_markets: HashMap<Interval, CrossMarketState>,
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
    /// Create a new MarketState, consuming a BinanceState (which persists across markets).
    /// PM fields start fresh; Binance fields carry over.
    pub fn new(info: MarketInfo, bn: BinanceState, oracle: OracleBasis) -> Self {
        Self {
            info,
            bn,
            up_bid: 0.0,
            up_ask: 0.0,
            down_bid: 0.0,
            down_ask: 0.0,
            pm_last_ts: 0,
            up_book: OrderBook::new(),
            down_book: OrderBook::new(),
            oracle,
            cross_markets: HashMap::new(),
            position: PositionTracker::new(),
            total_signals: 0,
            total_orders: 0,
            total_filled: 0,
            gross_pnl: 0.0,
            strategy_stats: HashMap::new(),
        }
    }

    /// Extract BinanceState at market end to carry into next market.
    pub fn take_binance_state(self) -> BinanceState {
        self.bn
    }

    #[inline]
    pub fn on_binance_trade(&mut self, t: BinanceTrade) {
        let bn = &mut self.bn;
        bn.binance_price = t.price;
        bn.binance_ts = t.exchange_ts_ms;

        // Update 1-second sampled EWMA vol; recache sigma if sampled
        if bn.ewma_vol.update(t.price, t.exchange_ts_ms) {
            let raw = bn.ewma_vol.sigma();
            bn.sigma_real_cached = if bn.ewma_vol.is_valid() {
                raw.max(bn.sigma_floor_per_sec)
            } else {
                0.0
            };
        }

        // Update VWAP tracker
        bn.vwap_tracker.update(t.exchange_ts_ms, t.price, t.qty);

        // Update regime classifier (tick direction)
        // Only count actual price changes — identical prices are noise, not direction
        if bn.prev_binance_price > 0.0 && t.price != bn.prev_binance_price {
            bn.regime
                .update(t.exchange_ts_ms, t.price > bn.prev_binance_price);
        }
        bn.prev_binance_price = t.price;

        // Store trade in buffer
        bn.trade_buffer.push_back(t);

        // Evict trades older than 30s
        let cutoff = bn.binance_ts - 30_000;
        while bn
            .trade_buffer
            .front()
            .map_or(false, |t| t.exchange_ts_ms < cutoff)
        {
            bn.trade_buffer.pop_front();
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
    pub fn on_book_update(&mut self, book: PolymarketBook) {
        if book.is_up_token {
            self.up_book.apply_snapshot(book.bids, book.asks);
            self.up_bid = self.up_book.best_bid();
            self.up_ask = self.up_book.best_ask();
        } else {
            self.down_book.apply_snapshot(book.bids, book.asks);
            self.down_bid = self.down_book.best_bid();
            self.down_ask = self.down_book.best_ask();
        }
    }

    pub fn on_cross_market_quote(&mut self, e: CrossMarketQuoteEvent) {
        self.cross_markets
            .entry(e.interval)
            .and_modify(|cm| {
                cm.up_bid = e.up_bid;
                cm.up_ask = e.up_ask;
                cm.down_bid = e.down_bid;
                cm.down_ask = e.down_ask;
                cm.strike = e.strike;
                cm.end_ms = e.end_ms;
            })
            .or_insert(CrossMarketState {
                interval: e.interval,
                up_bid: e.up_bid,
                up_ask: e.up_ask,
                down_bid: e.down_bid,
                down_ask: e.down_ask,
                strike: e.strike,
                end_ms: e.end_ms,
            });
    }

    #[inline]
    pub fn time_left_s(&self, now_ms: i64) -> f64 {
        ((self.info.end_ms - now_ms).max(0)) as f64 / 1000.0
    }

    #[inline]
    pub fn distance(&self) -> f64 {
        self.bn.binance_price - self.info.strike
    }

    #[inline]
    pub fn distance_frac(&self) -> f64 {
        if self.info.strike > 0.0 {
            self.distance() / self.info.strike
        } else {
            0.0
        }
    }

    /// Oracle-adjusted price estimate.
    #[inline]
    pub fn s_est(&self) -> f64 {
        self.oracle.s_est(self.bn.binance_price)
    }

    /// Effective time to expiry (seconds) with oracle uncertainty.
    #[inline]
    pub fn tau_eff_s(&self, now_ms: i64) -> f64 {
        self.oracle.tau_eff(self.time_left_s(now_ms))
    }

    /// Per-second realized vol. Cached — updated once per second when EWMA samples.
    /// Zero cost on the hot path (field read, no sqrt).
    #[inline]
    pub fn sigma_real(&self) -> f64 {
        self.bn.sigma_real_cached
    }

    pub fn is_stale(&self, now_ms: i64) -> bool {
        (self.bn.binance_ts > 0 && now_ms - self.bn.binance_ts > 5000)
            || (self.pm_last_ts > 0 && now_ms - self.pm_last_ts > 5000)
    }

    pub fn has_data(&self) -> bool {
        self.bn.binance_price > 0.0 && (self.up_ask > 0.0 || self.down_ask > 0.0)
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
