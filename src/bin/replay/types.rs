use std::collections::VecDeque;
use std::time::Instant;

use polymarket_crypto::config::Config;
use polymarket_crypto::engine::risk::StrategyRiskManager;
use polymarket_crypto::engine::state::MarketState;
use polymarket_crypto::types::Side;

// ─── CSV row types (each bin owns its own) ───

pub struct BinanceCsvRow {
    pub ts_ms: i64,
    pub price: f64,
    pub qty: f64,
    pub is_buy: bool,
}

pub struct PmCsvRow {
    pub ts_ms: i64,
    pub up_bid: f64,
    pub up_ask: f64,
    pub down_bid: f64,
    pub down_ask: f64,
}

pub struct BookSnapshot {
    pub ts_ms: i64,
    pub is_up: bool,
    pub bids: Vec<(f64, f64)>,
    pub asks: Vec<(f64, f64)>,
}

// ─── Replay event (merged timeline) ───

#[derive(Clone)]
pub enum ReplayEvent {
    Binance {
        ts_ms: i64,
        price: f64,
        qty: f64,
        is_buy: bool,
    },
    Polymarket {
        ts_ms: i64,
        up_bid: f64,
        up_ask: f64,
        down_bid: f64,
        down_ask: f64,
    },
    Book {
        ts_ms: i64,
        is_up: bool,
        bids: Vec<(f64, f64)>,
        asks: Vec<(f64, f64)>,
    },
}

impl ReplayEvent {
    pub fn ts_ms(&self) -> i64 {
        match self {
            Self::Binance { ts_ms, .. }
            | Self::Polymarket { ts_ms, .. }
            | Self::Book { ts_ms, .. } => *ts_ms,
        }
    }

    pub fn type_label(&self) -> &'static str {
        match self {
            Self::Binance { .. } => "BN Trade",
            Self::Polymarket { .. } => "PM Quote",
            Self::Book { .. } => "PM Book",
        }
    }
}

// ─── Market info loaded from disk ───

pub struct LoadedMarketInfo {
    pub slug: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub strike: f64,
}

// ─── Signal & order log entries ───

#[derive(Clone)]
pub struct SignalEntry {
    pub event_idx: usize,
    pub btc_price: f64,
    pub strategy: String,
    pub side: String,
    pub edge: f64,
    pub fair_value: f64,
    pub market_price: f64,
    pub time_left_s: f64,
    pub is_passive: bool,
}

#[derive(Clone)]
pub struct OrderEntry {
    pub event_idx: usize,
    pub btc_price: f64,
    pub id: u64,
    pub strategy: String,
    pub side: String,
    pub price: f64,
    pub size: f64,
    pub edge: f64,
    pub time_left_s: f64,
    pub is_passive: bool,
}

// ─── App state ───

pub struct App {
    pub events: Vec<ReplayEvent>,
    pub cursor: usize,
    pub state: MarketState,
    pub market_info: LoadedMarketInfo,
    pub data_dir: String,
    pub playing: bool,
    pub speed: usize,

    // Snapshots for O(1000) backward navigation
    pub snapshots: Vec<(usize, MarketState)>,
    pub snapshot_interval: usize,

    // Chart histories: (event_index, value)
    pub price_history: VecDeque<(f64, f64)>,
    pub vwap_history: VecDeque<(f64, f64)>,
    pub buy_vol_history: VecDeque<u64>,
    pub sell_vol_history: VecDeque<u64>,
    pub up_bid_chart: VecDeque<(f64, f64)>,
    pub up_ask_chart: VecDeque<(f64, f64)>,
    pub down_bid_chart: VecDeque<(f64, f64)>,
    pub down_ask_chart: VecDeque<(f64, f64)>,

    // Signal & order logs
    pub signal_log: Vec<SignalEntry>,
    pub order_log: Vec<OrderEntry>,
    pub risk: StrategyRiskManager,
    pub house_side: Option<Side>,
    pub next_order_id: u64,

    pub fake_instant: Instant,
    pub status_msg: Option<(String, Instant)>,
}

// ─── Replay config (no env vars needed) ───

pub fn replay_config() -> Config {
    Config {
        asset: "btc".to_string(),
        interval: polymarket_crypto::config::Interval::M5,
        binance_ws: String::new(),
        binance_ws_fallback: String::new(),
        polymarket_clob_ws: String::new(),
        gamma_api_url: String::new(),
        series_id: String::new(),
        tg_bot_token: None,
        tg_chat_id: None,
        max_position_usd: 100.0,
        max_orders_per_market: 10,
        cooldown_ms: 5000,
        bankroll: 1000.0,
        max_total_exposure_frac: 0.15,
        daily_loss_halt_frac: -0.03,
        weekly_loss_halt_frac: -0.08,
        oracle_beta: 0.0,
        oracle_delta_s: 2.0,
        ewma_lambda: 0.94,
        sigma_floor_annual: 0.30,
        max_portfolio_delta: 0.0,
        max_portfolio_gamma_neg: 0.0,
        strategy_latency_arb: true,
        strategy_certainty_capture: true,
        strategy_convexity_fade: true,
        strategy_strike_misalign: true,
        strategy_lp_extreme: true,
        strategy_cross_timeframe: false,
        dry_run: true,
        polymarket_private_key: None,
        polymarket_funder_address: None,
        polymarket_signature_type: 0,
    }
}
