use std::time::Instant;

// ─── Feed Events (produced by WS tasks, consumed by engine) ───

pub enum FeedEvent {
    BinanceTrade(BinanceTrade),
    PolymarketQuote(PolymarketQuote),
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

// ─── Market Info ───

#[derive(Clone)]
pub struct MarketInfo {
    pub slug: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub up_token_id: String,
    pub down_token_id: String,
    pub strike: f64,
}

// ─── Strategy Output ───

#[derive(Clone, Copy, Debug)]
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

pub struct Signal {
    pub strategy: &'static str,
    pub side: Side,
    pub edge: f64,
    pub fair_value: f64,
    pub market_price: f64,
    pub confidence: f64,
    pub size_frac: f64,
}

// ─── Orders & Execution ───

pub struct Order {
    pub id: u64,
    pub side: Side,
    pub price: f64,
    pub size: f64,
    pub strategy: &'static str,
    pub signal_edge: f64,
    pub created_at: Instant,
}

pub struct OrderAck {
    pub order_id: u64,
    pub status: OrderStatus,
    pub filled_price: Option<f64>,
    pub filled_size: Option<f64>,
    pub latency_ms: f64,
}

#[derive(Debug)]
pub enum OrderStatus {
    Filled,
    PartialFill,
    Rejected(String),
    Timeout,
}

// ─── Telemetry Events ───

pub enum TelemetryEvent {
    Signal(SignalRecord),
    Latency(LatencyRecord),
    OrderSent(OrderRecord),
    OrderResult(FillRecord),
    MarketStart(MarketStartRecord),
    MarketEnd(MarketEndRecord),
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
    pub selected: bool, // was this signal chosen by select_best()?
}

pub struct LatencyRecord {
    pub ts_ms: i64,
    pub event: &'static str,
    pub latency_us: u64,
}

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

pub struct FillRecord {
    pub ts_ms: i64,
    pub order_id: u64,
    pub strategy: String, // which strategy triggered this order
    pub status: String,
    pub filled_price: Option<f64>,
    pub filled_size: Option<f64>,
    pub submit_to_ack_ms: f64,
    pub pnl_if_correct: Option<f64>,
}

pub struct MarketStartRecord {
    pub ts_ms: i64,
    pub slug: String,
    pub strike: f64,
    pub start_ms: i64,
    pub end_ms: i64,
}

pub struct PerStrategyEnd {
    pub strategy: String,
    pub signals: u32,
    pub orders: u32,
    pub filled: u32,
    pub gross_pnl: f64,
    pub avg_edge: f64,
}

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
