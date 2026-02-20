use std::time::Instant;

use crate::config::Interval;

// ─── Feed Events (produced by WS tasks, consumed by engine) ───

pub enum FeedEvent {
    BinanceTrade(BinanceTrade),
    PolymarketQuote(PolymarketQuote),
    PolymarketBook(PolymarketBook),
    CrossMarketQuote(CrossMarketQuoteEvent),
    OrderAck(OrderAck),
    Tick,
}

#[derive(Clone)]
pub struct BinanceTrade {
    pub exchange_ts_ms: i64,
    pub recv_at: Instant,
    pub price: f64,
    pub qty: f64,
    pub is_buy: bool,
}

pub struct PolymarketQuote {
    pub server_ts_ms: i64,
    pub recv_at: Instant,
    pub up_bid: Option<f64>,
    pub up_ask: Option<f64>,
    pub down_bid: Option<f64>,
    pub down_ask: Option<f64>,
}

pub struct PolymarketBook {
    pub recv_at: Instant,
    pub is_up_token: bool,
    pub bids: Vec<(f64, f64)>, // (price, size), sorted desc by price
    pub asks: Vec<(f64, f64)>, // (price, size), sorted asc by price
}

pub struct CrossMarketQuoteEvent {
    pub interval: Interval,
    pub up_bid: f64,
    pub up_ask: f64,
    pub down_bid: f64,
    pub down_ask: f64,
    pub strike: f64,
    pub end_ms: i64,
}

// ─── Market Info ───

#[derive(Clone)]
pub struct MarketInfo {
    pub slug: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub up_token_id: String,
    pub down_token_id: String,
    pub strike: f64,
    pub tick_size: f64,
    pub neg_risk: bool,
}

/// Per-market context sent to the order gateway at market start.
#[derive(Clone)]
pub struct MarketContext {
    pub up_token_id: String,
    pub down_token_id: String,
    pub tick_size: f64,
    pub neg_risk: bool,
}

// ─── Strategy Output ───

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Side {
    Up,
    Down,
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Side::Up => write!(f, "UP"),
            Side::Down => write!(f, "DOWN"),
        }
    }
}

/// Evaluation trigger: which event type a strategy wants to evaluate on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EvalTrigger {
    BinanceTrade,
    PolymarketQuote,
    Both,
    MarketOpen,
}

pub struct Signal {
    pub strategy: &'static str,
    pub side: Side,
    pub edge: f64,
    pub fair_value: f64,
    pub market_price: f64,
    pub confidence: f64,
    pub size_frac: f64,
    pub is_passive: bool,
    /// If true, post at best bid instead of crossing at ask (GTD post_only).
    /// Used by convexity_fade and strike_misalign.
    pub use_bid: bool,
}

// ─── Settlement ───

/// Recorded fill for settlement PnL computation.
pub struct Fill {
    pub order_id: u64,
    pub strategy: &'static str,
    pub side: Side,
    pub price: f64,
    pub size: f64,
}

// ─── Orders & Execution ───

/// Order type for CLOB submission.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OrderType {
    /// Good-Till-Cancelled: rests on the book until filled or cancelled.
    GTC,
    /// Fill-Or-Kill: must fill immediately and completely, or is cancelled.
    FOK,
    /// Good-Till-Date: rests on the book until filled, cancelled, or expiration.
    GTD,
}

#[derive(Clone)]
pub struct Order {
    pub id: u64,
    pub side: Side,
    pub price: f64,
    pub size: f64,
    pub strategy: &'static str,
    pub signal_edge: f64,
    pub is_passive: bool,
    pub created_at: Instant,
    /// CLOB order type: GTC for passive limit, FOK for aggressive taker, GTD for time-limited.
    pub order_type: OrderType,
    /// Post-only: reject if order would cross the spread. Valid with GTC and GTD.
    pub post_only: bool,
    /// GTD expiration timestamp in milliseconds (UTC). Only set for GTD orders.
    pub expiration_ms: Option<i64>,
    /// CLOB token ID for the outcome being bought.
    pub token_id: String,
}

pub struct OrderAck {
    pub order_id: u64,
    pub status: OrderStatus,
    pub filled_price: Option<f64>,
    pub filled_size: Option<f64>,
    pub latency_ms: f64,
    /// CLOB-assigned order ID (for tracking/cancellation).
    pub clob_order_id: Option<String>,
    /// Raw JSON response from CLOB (for recording/replay).
    pub raw_response: Option<String>,
}

#[derive(Debug)]
pub enum OrderStatus {
    Filled,
    PartialFill,
    Rejected(String),
    Timeout,
    /// Order posted to book, awaiting match (post_only GTC success).
    Live,
    /// Post-only order rejected because it would cross the spread.
    Unmatched,
}

// ─── Telemetry Events ───

pub enum TelemetryEvent {
    Signal(SignalRecord),
    Latency(LatencyRecord),
    OrderSent(OrderRecord),
    OrderResult(FillRecord),
    MarketStart(MarketStartRecord),
    MarketEnd(MarketEndRecord),
    StrategyMetrics(StrategyMetricsRecord),
    /// Raw CLOB request/response JSON for exact-environment replay.
    RawClobResponse(RawClobRecord),
    /// Local order rejection (e.g., insufficient balance). Triggers TG alert.
    OrderRejectedLocal(OrderRejectedRecord),
}

#[derive(Clone)]
pub struct OrderRejectedRecord {
    pub order_id: u64,
    pub strategy: String,
    pub reason: String,
}

pub struct SignalRecord {
    pub ts_ms: i64,
    pub strategy: String,
    pub side: Side,
    pub edge: f64,
    pub fair_value: f64,
    pub market_price: f64,
    pub confidence: f64,
    pub size_frac: f64,
    pub binance_price: f64,
    pub distance: f64,
    pub time_left_s: f64,
    pub eval_latency_us: u64,
    pub selected: bool,
    // Per-signal Greeks at evaluation time
    pub signal_delta: f64,
    pub signal_gamma: f64,
    // Portfolio-level aggregate Greeks at signal time
    pub portfolio_delta: f64,
    pub portfolio_gamma: f64,
}

pub struct LatencyRecord {
    pub ts_ms: i64,
    pub event: &'static str,
    pub latency_us: u64,
}

#[derive(Clone)]
pub struct OrderRecord {
    pub ts_ms: i64,
    pub order_id: u64,
    pub side: Side,
    pub price: f64,
    pub size: f64,
    pub strategy: String,
    pub edge_at_submit: f64,
    pub binance_price: f64,
    pub time_left_s: f64,
}

#[derive(Clone)]
pub struct FillRecord {
    pub ts_ms: i64,
    pub order_id: u64,
    pub strategy: String,
    pub side: Side,
    pub status: String,
    pub filled_price: Option<f64>,
    pub filled_size: Option<f64>,
    pub submit_to_ack_ms: f64,
    pub pnl_if_correct: Option<f64>,
}

#[derive(Clone)]
pub struct MarketStartRecord {
    pub ts_ms: i64,
    pub slug: String,
    pub strike: f64,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Clone)]
pub struct PerStrategyEnd {
    pub strategy: String,
    pub signals: u32,
    pub orders: u32,
    pub filled: u32,
    pub gross_pnl: f64,
    pub avg_edge: f64,
}

#[derive(Clone)]
pub struct MarketEndRecord {
    pub ts_ms: i64,
    pub slug: String,
    pub final_binance_price: f64,
    pub final_distance: f64,
    pub outcome: Side,
    pub total_signals: u32,
    pub total_orders: u32,
    pub total_filled: u32,
    pub gross_pnl: f64,
    pub per_strategy: Vec<PerStrategyEnd>,
}

#[derive(Clone)]
pub struct StrategyMetricsRecord {
    pub ts_ms: i64,
    pub strategy: String,
    pub fill_count: u32,
    pub fill_rate: f64,
    pub adverse_selection: f64,
    pub win_rate: f64,
    pub avg_edge: f64,
}

/// Raw CLOB request/response for recording and replay.
pub struct RawClobRecord {
    pub ts_ms: i64,
    pub order_id: u64,
    /// "submit" or "response"
    pub direction: &'static str,
    pub raw_json: String,
}
