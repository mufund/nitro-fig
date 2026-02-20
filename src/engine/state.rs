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
#[derive(Clone)]
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
#[derive(Clone)]
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

    /// Volume-weighted average fill price for a buy order of `target_size` units.
    /// Walks ask levels (ascending by price after defensive sort).
    /// Returns (avg_fill_price, fillable_size). None if book is empty.
    pub fn vwap_fill_ask(&self, target_size: f64) -> Option<(f64, f64)> {
        if self.asks.is_empty() || target_size <= 0.0 {
            return None;
        }
        let mut remaining = target_size;
        let mut cost = 0.0;
        let mut filled = 0.0;
        for &(price, size) in &self.asks {
            let take = remaining.min(size);
            cost += take * price;
            filled += take;
            remaining -= take;
            if remaining <= 0.0 {
                break;
            }
        }
        if filled > 0.0 {
            Some((cost / filled, filled))
        } else {
            None
        }
    }

    /// Microprice: size-weighted mid using level-1 depth.
    /// microprice = (bid_price * ask_size + ask_price * bid_size) / (bid_size + ask_size)
    /// Returns 0.0 if either side is empty.
    #[inline]
    pub fn microprice(&self) -> f64 {
        let (bp, bs) = match self.bids.first() {
            Some(&(p, s)) if p > 0.0 && s > 0.0 => (p, s),
            _ => return 0.0,
        };
        let (ap, a_s) = match self.asks.first() {
            Some(&(p, s)) if p > 0.0 && s > 0.0 => (p, s),
            _ => return 0.0,
        };
        (bp * a_s + ap * bs) / (bs + a_s)
    }

    /// Depth imbalance: bid_depth / (bid_depth + ask_depth) over N levels.
    /// Values > 0.5 = more buying interest. Values < 0.5 = more selling pressure.
    /// Returns 0.5 (neutral) if both sides empty.
    #[inline]
    pub fn depth_imbalance(&self, levels: usize) -> f64 {
        let bd = self.bid_depth(levels);
        let ad = self.ask_depth(levels);
        let total = bd + ad;
        if total <= 0.0 {
            0.5
        } else {
            bd / total
        }
    }

    pub fn apply_snapshot(&mut self, mut bids: Vec<(f64, f64)>, mut asks: Vec<(f64, f64)>) {
        // Defensive sort: bids descending by price, asks ascending by price.
        // VWAP fill and depth calculations depend on correct ordering.
        bids.sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        asks.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        self.bids = bids;
        self.asks = asks;
    }
}

/// Cross-market state for Edge 4 (cross-timeframe RV).
#[derive(Clone)]
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
#[derive(Clone)]
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
#[derive(Clone)]
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

#[derive(Clone)]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_book(bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)>) -> OrderBook {
        let mut ob = OrderBook::new();
        ob.apply_snapshot(bids, asks);
        ob
    }

    // ── microprice ──

    /// Scenario: OrderBook with equal bid and ask sizes at symmetric prices.
    /// Expected: Microprice equals the simple midpoint since depth is balanced.
    #[test]
    fn test_microprice_symmetric() {
        let ob = make_book(vec![(0.50, 100.0)], vec![(0.52, 100.0)]);
        let mp = ob.microprice();
        assert!((mp - 0.51).abs() < 1e-10, "Symmetric should be midpoint: {}", mp);
    }

    /// Scenario: OrderBook with 10x more size on the ask side than the bid side.
    /// Expected: Microprice is pulled toward the bid price (away from the heavier ask).
    #[test]
    fn test_microprice_asymmetric() {
        // Large ask size pulls microprice toward bid
        let ob = make_book(vec![(0.50, 10.0)], vec![(0.52, 100.0)]);
        let mp = ob.microprice();
        let expected = (0.50 * 100.0 + 0.52 * 10.0) / (10.0 + 100.0);
        assert!((mp - expected).abs() < 1e-10, "Expected {}, got {}", expected, mp);
        assert!(mp < 0.51, "Should be below naive mid");
    }

    /// Scenario: OrderBook missing one side (bids empty, then asks empty).
    /// Expected: Microprice returns 0.0 when either side has no levels.
    #[test]
    fn test_microprice_empty_book() {
        let ob = make_book(vec![], vec![(0.52, 100.0)]);
        assert_eq!(ob.microprice(), 0.0);
        let ob2 = make_book(vec![(0.50, 100.0)], vec![]);
        assert_eq!(ob2.microprice(), 0.0);
    }

    // ── vwap_fill_ask ──

    /// Scenario: Ask book has one level with 100 units; request a fill of 50.
    /// Expected: VWAP fill price equals the single ask price, fills exactly 50.
    #[test]
    fn test_vwap_fill_single_level() {
        let ob = make_book(vec![], vec![(0.50, 100.0)]);
        let (price, size) = ob.vwap_fill_ask(50.0).unwrap();
        assert!((price - 0.50).abs() < 1e-10);
        assert!((size - 50.0).abs() < 1e-10);
    }

    /// Scenario: Three ask levels with increasing prices; request 50 units (requires first two levels).
    /// Expected: VWAP fill blends prices from the two consumed levels weighted by size taken at each.
    #[test]
    fn test_vwap_fill_walks_levels() {
        let ob = make_book(vec![], vec![(0.50, 20.0), (0.52, 30.0), (0.55, 50.0)]);
        let (price, size) = ob.vwap_fill_ask(50.0).unwrap();
        // Takes 20 @ 0.50 + 30 @ 0.52 = cost 10.0 + 15.6 = 25.6 / 50.0
        let expected = (20.0 * 0.50 + 30.0 * 0.52) / 50.0;
        assert!((price - expected).abs() < 1e-10, "VWAP fill = {}, expected {}", price, expected);
        assert!((size - 50.0).abs() < 1e-10);
    }

    /// Scenario: Ask book has only 20 units but 100 are requested.
    /// Expected: Partial fill of 20 at the single available price; fillable_size < target_size.
    #[test]
    fn test_vwap_fill_partial_depth() {
        let ob = make_book(vec![], vec![(0.50, 20.0)]);
        let (price, size) = ob.vwap_fill_ask(100.0).unwrap();
        assert!((price - 0.50).abs() < 1e-10);
        assert!((size - 20.0).abs() < 1e-10); // only 20 available
    }

    /// Scenario: Completely empty ask book with a 50-unit fill request.
    /// Expected: Returns None because there is no liquidity to fill against.
    #[test]
    fn test_vwap_fill_empty_book() {
        let ob = make_book(vec![], vec![]);
        assert!(ob.vwap_fill_ask(50.0).is_none());
    }

    // ── depth_imbalance ──

    /// Scenario: Equal bid and ask depth (100 each) at level 1.
    /// Expected: Depth imbalance is exactly 0.5 (neutral).
    #[test]
    fn test_depth_imbalance_balanced() {
        let ob = make_book(vec![(0.50, 100.0)], vec![(0.52, 100.0)]);
        let imb = ob.depth_imbalance(1);
        assert!((imb - 0.5).abs() < 1e-10, "Balanced should be 0.5: {}", imb);
    }

    /// Scenario: Bid depth is 25, ask depth is 75 at level 1.
    /// Expected: Imbalance is 0.25, indicating heavier selling pressure.
    #[test]
    fn test_depth_imbalance_skewed() {
        let ob = make_book(vec![(0.50, 25.0)], vec![(0.52, 75.0)]);
        let imb = ob.depth_imbalance(1);
        assert!((imb - 0.25).abs() < 1e-10, "Should be 0.25: {}", imb);
    }

    /// Scenario: Both bid and ask sides are empty.
    /// Expected: Imbalance defaults to 0.5 (neutral) to avoid division by zero.
    #[test]
    fn test_depth_imbalance_empty() {
        let ob = make_book(vec![], vec![]);
        assert!((ob.depth_imbalance(5) - 0.5).abs() < 1e-10, "Empty should be neutral 0.5");
    }

    // ── apply_snapshot sorting ──

    /// Scenario: Bids and asks passed in unsorted order to apply_snapshot().
    /// Expected: Bids are sorted descending (best bid first), asks ascending (best ask first).
    #[test]
    fn test_apply_snapshot_sorts_correctly() {
        let mut ob = OrderBook::new();
        ob.apply_snapshot(
            vec![(0.48, 10.0), (0.50, 20.0)], // bids out of order
            vec![(0.54, 10.0), (0.52, 20.0)], // asks out of order
        );
        assert_eq!(ob.bids[0].0, 0.50, "Best bid should be highest");
        assert_eq!(ob.asks[0].0, 0.52, "Best ask should be lowest");
    }

    // ── mid ──

    /// Scenario: Normal book with bid at 0.48 and ask at 0.52.
    /// Expected: Mid price is the arithmetic average: 0.50.
    #[test]
    fn test_mid_normal() {
        let ob = make_book(vec![(0.48, 50.0)], vec![(0.52, 50.0)]);
        assert!((ob.mid() - 0.50).abs() < 1e-10);
    }

    /// Scenario: Only asks present, no bids in the book.
    /// Expected: Mid returns 0.0 because a two-sided mid requires both sides.
    #[test]
    fn test_mid_empty_bid() {
        let ob = make_book(vec![], vec![(0.52, 50.0)]);
        assert_eq!(ob.mid(), 0.0);
    }

    /// Scenario: Completely empty book (no bids or asks).
    /// Expected: Mid returns 0.0.
    #[test]
    fn test_mid_empty_both() {
        let ob = make_book(vec![], vec![]);
        assert_eq!(ob.mid(), 0.0);
    }

    // ── spread ──

    /// Scenario: Normal book with bid at 0.48 and ask at 0.52.
    /// Expected: Spread is ask minus bid = 0.04.
    #[test]
    fn test_spread_normal() {
        let ob = make_book(vec![(0.48, 50.0)], vec![(0.52, 50.0)]);
        assert!((ob.spread() - 0.04).abs() < 1e-10);
    }

    /// Scenario: Book missing the bid side, then both sides empty.
    /// Expected: Spread returns 0.0 when either side is absent.
    #[test]
    fn test_spread_empty() {
        assert_eq!(make_book(vec![], vec![(0.52, 50.0)]).spread(), 0.0);
        assert_eq!(make_book(vec![], vec![]).spread(), 0.0);
    }

    // ── bid_depth / ask_depth ──

    /// Scenario: Three bid levels with sizes 100, 75, 50; query top 2 and top 5 levels.
    /// Expected: Top 2 sums to 175; requesting more levels than exist sums the entire book (225).
    #[test]
    fn test_bid_depth_multi_level() {
        let ob = make_book(
            vec![(0.50, 100.0), (0.49, 75.0), (0.48, 50.0)],
            vec![(0.52, 50.0)],
        );
        assert!((ob.bid_depth(2) - 175.0).abs() < 1e-10);
        assert!((ob.bid_depth(5) - 225.0).abs() < 1e-10); // levels > book len
    }

    /// Scenario: Three ask levels with sizes 30, 40, 50; query top 2 and top 3.
    /// Expected: Top 2 sums to 70; top 3 sums to 120.
    #[test]
    fn test_ask_depth_multi_level() {
        let ob = make_book(
            vec![(0.50, 50.0)],
            vec![(0.52, 30.0), (0.53, 40.0), (0.54, 50.0)],
        );
        assert!((ob.ask_depth(2) - 70.0).abs() < 1e-10);
        assert!((ob.ask_depth(3) - 120.0).abs() < 1e-10);
    }

    /// Scenario: Empty book queried for depth, and a populated book queried for 0 levels.
    /// Expected: Both return 0.0 -- empty book has nothing, 0 levels means no sum.
    #[test]
    fn test_depth_empty_and_zero_levels() {
        let ob = make_book(vec![], vec![]);
        assert_eq!(ob.bid_depth(5), 0.0);
        assert_eq!(ob.ask_depth(5), 0.0);
        let ob2 = make_book(vec![(0.50, 100.0)], vec![(0.52, 50.0)]);
        assert_eq!(ob2.bid_depth(0), 0.0);
        assert_eq!(ob2.ask_depth(0), 0.0);
    }

    // ── best_bid / best_ask ──

    /// Scenario: Completely empty book queried for best_bid and best_ask.
    /// Expected: Both return 0.0 as the fallback when no levels exist.
    #[test]
    fn test_best_bid_ask_empty() {
        let ob = make_book(vec![], vec![]);
        assert_eq!(ob.best_bid(), 0.0);
        assert_eq!(ob.best_ask(), 0.0);
    }

    // ── vwap_fill_ask edge cases ──

    /// Scenario: Non-empty ask book but target_size is 0.0.
    /// Expected: Returns None because a zero-size fill is meaningless.
    #[test]
    fn test_vwap_fill_zero_target() {
        let ob = make_book(vec![], vec![(0.50, 100.0)]);
        assert!(ob.vwap_fill_ask(0.0).is_none());
    }

    /// Scenario: Non-empty ask book but target_size is negative (-10).
    /// Expected: Returns None because negative fill sizes are rejected.
    #[test]
    fn test_vwap_fill_negative_target() {
        let ob = make_book(vec![], vec![(0.50, 100.0)]);
        assert!(ob.vwap_fill_ask(-10.0).is_none());
    }

    // ── StrategyStats ──

    /// Scenario: Freshly constructed StrategyStats with zero orders.
    /// Expected: avg_edge() returns 0.0 to avoid division by zero.
    #[test]
    fn test_strategy_stats_avg_edge_zero_orders() {
        let ss = StrategyStats::new();
        assert_eq!(ss.avg_edge(), 0.0);
        assert_eq!(ss.orders, 0);
    }

    /// Scenario: StrategyStats with 4 orders and total_edge = 0.20.
    /// Expected: avg_edge() returns 0.05 (0.20 / 4).
    #[test]
    fn test_strategy_stats_avg_edge() {
        let mut ss = StrategyStats::new();
        ss.orders = 4;
        ss.total_edge = 0.20;
        assert!((ss.avg_edge() - 0.05).abs() < 1e-10);
    }

    // ── MarketState methods ──

    fn make_test_state(strike: f64, binance_price: f64) -> MarketState {
        let info = MarketInfo {
            slug: "test".to_string(),
            start_ms: 0,
            end_ms: 300_000,
            up_token_id: "up".to_string(),
            down_token_id: "down".to_string(),
            strike,
            tick_size: 0.01,
            neg_risk: false,
        };
        let oracle = OracleBasis::new(0.0, 2.0);
        let mut bn = BinanceState::new(0.94, 5, 0.30, 30_000, 60_000);
        bn.binance_price = binance_price;
        MarketState::new(info, bn, oracle)
    }

    /// Scenario: Binance price (96k) above strike (95k).
    /// Expected: distance() returns +1000 (price - strike).
    #[test]
    fn test_distance_positive() {
        let state = make_test_state(95_000.0, 96_000.0);
        assert!((state.distance() - 1000.0).abs() < 1e-10);
    }

    /// Scenario: Binance price (94k) below strike (95k).
    /// Expected: distance() returns -1000 (price - strike).
    #[test]
    fn test_distance_negative() {
        let state = make_test_state(95_000.0, 94_000.0);
        assert!((state.distance() - (-1000.0)).abs() < 1e-10);
    }

    /// Scenario: Binance price is 1% above the 100k strike.
    /// Expected: distance_frac() returns 0.01 (distance / strike).
    #[test]
    fn test_distance_frac() {
        let state = make_test_state(100_000.0, 101_000.0);
        assert!((state.distance_frac() - 0.01).abs() < 1e-10);
    }

    /// Scenario: Strike is zero (degenerate market info).
    /// Expected: distance_frac() returns 0.0 to avoid division by zero.
    #[test]
    fn test_distance_frac_zero_strike() {
        let state = make_test_state(0.0, 100_000.0);
        assert_eq!(state.distance_frac(), 0.0);
    }

    /// Scenario: Market ends at 300s; check at 200s and at 400s (past expiry).
    /// Expected: 100s remaining mid-market, 0s after expiry (clamped to non-negative).
    #[test]
    fn test_time_left_s() {
        let state = make_test_state(95_000.0, 96_000.0);
        assert!((state.time_left_s(200_000) - 100.0).abs() < 1e-10);
        assert_eq!(state.time_left_s(400_000), 0.0); // past expiry
    }

    /// Scenario: Oracle has a +15 basis offset; Binance price is 96000.
    /// Expected: s_est() returns 96015 (binance_price + oracle beta).
    #[test]
    fn test_s_est_with_beta() {
        let info = MarketInfo {
            slug: "test".to_string(),
            start_ms: 0,
            end_ms: 300_000,
            up_token_id: "up".to_string(),
            down_token_id: "down".to_string(),
            strike: 95_000.0,
            tick_size: 0.01,
            neg_risk: false,
        };
        let oracle = OracleBasis::new(15.0, 2.0);
        let mut bn = BinanceState::new(0.94, 5, 0.30, 30_000, 60_000);
        bn.binance_price = 96_000.0;
        let state = MarketState::new(info, bn, oracle);
        assert_eq!(state.s_est(), 96_015.0);
    }

    /// Scenario: Oracle uncertainty pad is 3s; 100s remain until expiry.
    /// Expected: tau_eff_s() returns 103s (time_left + oracle uncertainty pad).
    #[test]
    fn test_tau_eff_s() {
        let info = MarketInfo {
            slug: "test".to_string(),
            start_ms: 0,
            end_ms: 300_000,
            up_token_id: "up".to_string(),
            down_token_id: "down".to_string(),
            strike: 95_000.0,
            tick_size: 0.01,
            neg_risk: false,
        };
        let oracle = OracleBasis::new(0.0, 3.0);
        let bn = BinanceState::new(0.94, 5, 0.30, 30_000, 60_000);
        let state = MarketState::new(info, bn, oracle);
        // time_left_s(200_000) = 100.0, tau_eff = 100.0 + 3.0 = 103.0
        assert!((state.tau_eff_s(200_000) - 103.0).abs() < 1e-10);
    }

    /// Scenario: Binance timestamp is fresh but Polymarket is 6s old (>5s threshold).
    /// Expected: is_stale() returns true because PM feed exceeds the staleness limit.
    #[test]
    fn test_is_stale_polymarket_path() {
        let mut state = make_test_state(95_000.0, 96_000.0);
        state.bn.binance_ts = 100_000;
        state.pm_last_ts = 94_000; // PM is 6s old at now=100_000
        assert!(state.is_stale(100_000));
    }

    /// Scenario: Both timestamps are 0 (no data received yet).
    /// Expected: is_stale() returns false because staleness requires ts > 0.
    #[test]
    fn test_is_stale_no_data_not_stale() {
        let state = make_test_state(95_000.0, 96_000.0);
        // Both timestamps are 0 → not stale (condition requires ts > 0)
        assert!(!state.is_stale(100_000));
    }

    /// Scenario: Binance price is set but no PM quotes yet, then up_ask is set.
    /// Expected: has_data() is false with BN only, true once any PM ask appears.
    #[test]
    fn test_has_data_needs_both() {
        let mut state = make_test_state(95_000.0, 96_000.0);
        assert!(!state.has_data(), "No PM data yet");
        state.up_ask = 0.55;
        assert!(state.has_data(), "BN price + up_ask = has data");
    }

    /// Scenario: Binance price set plus only down_ask (no up token data).
    /// Expected: has_data() returns true because down-side ask alone satisfies the requirement.
    #[test]
    fn test_has_data_down_only() {
        let mut state = make_test_state(95_000.0, 96_000.0);
        state.down_ask = 0.45;
        assert!(state.has_data(), "Down-only data counts");
    }

    // ── PositionTracker ──

    /// Scenario: Two sequential fills at prices 0.50 and 0.60 for 10 units each.
    /// Expected: Total size is 20, avg_price is 0.55 (weighted average), pending_orders resets to 0.
    #[test]
    fn test_position_tracker_fill_averaging() {
        let mut pt = PositionTracker::new();
        pt.on_order_sent();
        pt.on_fill(&OrderAck {
            order_id: 1,
            status: OrderStatus::Filled,
            filled_price: Some(0.50),
            filled_size: Some(10.0),
            latency_ms: 50.0,
            clob_order_id: None,
            raw_response: None,
        });
        pt.on_order_sent();
        pt.on_fill(&OrderAck {
            order_id: 2,
            status: OrderStatus::Filled,
            filled_price: Some(0.60),
            filled_size: Some(10.0),
            latency_ms: 50.0,
            clob_order_id: None,
            raw_response: None,
        });
        assert!((pt.size - 20.0).abs() < 1e-10);
        assert!((pt.avg_price - 0.55).abs() < 1e-10);
        assert_eq!(pt.pending_orders, 0);
    }

    /// Scenario: on_fill() called without a preceding on_order_sent().
    /// Expected: pending_orders stays at 0 (underflow guard prevents wrapping).
    #[test]
    fn test_position_tracker_pending_underflow_guard() {
        let mut pt = PositionTracker::new();
        // on_fill without on_order_sent → pending stays 0
        pt.on_fill(&OrderAck {
            order_id: 1,
            status: OrderStatus::Filled,
            filled_price: Some(0.55),
            filled_size: Some(10.0),
            latency_ms: 50.0,
            clob_order_id: None,
            raw_response: None,
        });
        assert_eq!(pt.pending_orders, 0);
    }
}
