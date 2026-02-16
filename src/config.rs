/// Trading interval.
#[derive(Clone, Copy, Debug)]
pub enum Interval {
    M5,
    M15,
    H1,
    H4,
}

impl Interval {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "15m" => Interval::M15,
            "1h" => Interval::H1,
            "4h" => Interval::H4,
            _ => Interval::M5,
        }
    }

    /// Window duration in seconds.
    pub fn window_secs(&self) -> i64 {
        match self {
            Interval::M5 => 300,
            Interval::M15 => 900,
            Interval::H1 => 3600,
            Interval::H4 => 14400,
        }
    }

    /// Window duration in milliseconds.
    pub fn window_ms(&self) -> i64 {
        self.window_secs() * 1000
    }

    /// Human-readable label for slugs and log paths: "5m", "15m", "1h", "4h".
    pub fn label(&self) -> &'static str {
        match self {
            Interval::M5 => "5m",
            Interval::M15 => "15m",
            Interval::H1 => "1h",
            Interval::H4 => "4h",
        }
    }
}

/// Configuration loaded from environment variables.
#[derive(Clone)]
pub struct Config {
    // Asset + interval
    pub asset: String,
    pub interval: Interval,

    // WebSocket URLs
    pub binance_ws: String,
    pub binance_ws_fallback: String,
    pub polymarket_clob_ws: String,

    // Gamma API
    pub gamma_api_url: String,
    pub series_id: String,

    // Telegram
    pub tg_bot_token: Option<String>,
    pub tg_chat_id: Option<String>,

    // Risk
    pub max_position_usd: f64,
    pub max_orders_per_market: u32,
    pub cooldown_ms: i64,

    // Mode
    pub dry_run: bool,
}

impl Config {
    pub fn from_env() -> Self {
        let asset = std::env::var("ASSET")
            .unwrap_or_else(|_| "btc".into())
            .to_lowercase();
        let interval = Interval::from_str(
            &std::env::var("INTERVAL").unwrap_or_else(|_| "5m".into()),
        );

        // Auto-derive Binance WS from asset unless explicitly overridden
        let binance_ws = std::env::var("BINANCE_WS").unwrap_or_else(|_| {
            format!("wss://stream.binance.com:9443/ws/{}usdt@trade", asset)
        });
        let binance_ws_fallback = std::env::var("BINANCE_WS_FALLBACK").unwrap_or_else(|_| {
            format!("wss://stream.binance.us:9443/ws/{}usd@trade", asset)
        });

        // Auto-derive series_id from asset+interval unless explicitly set
        let series_id = std::env::var("SERIES_ID").unwrap_or_else(|_| {
            default_series_id(&asset, &interval).to_string()
        });

        Self {
            asset,
            interval,
            binance_ws,
            binance_ws_fallback,
            polymarket_clob_ws: std::env::var("PM_CLOB_WS")
                .unwrap_or_else(|_| "wss://ws-subscriptions-clob.polymarket.com/ws/market".into()),
            gamma_api_url: std::env::var("GAMMA_API_URL")
                .unwrap_or_else(|_| "https://gamma-api.polymarket.com".into()),
            series_id,
            tg_bot_token: std::env::var("TELEGRAM_BOT_TOKEN").ok(),
            tg_chat_id: std::env::var("TELEGRAM_CHAT_ID").ok(),
            max_position_usd: std::env::var("MAX_POSITION_USD")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(100.0),
            max_orders_per_market: std::env::var("MAX_ORDERS_PER_MARKET")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10),
            cooldown_ms: std::env::var("COOLDOWN_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5000),
            dry_run: std::env::var("DRY_RUN")
                .map(|v| v == "1" || v.to_lowercase() == "true")
                .unwrap_or(true),
        }
    }

    /// Uppercase asset label for display: "BTC", "ETH", "SOL", "XRP".
    pub fn asset_label(&self) -> String {
        self.asset.to_uppercase()
    }

    /// Slug prefix for market discovery: "{asset}-updown-{interval}-".
    pub fn slug_prefix(&self) -> String {
        format!("{}-updown-{}-", self.asset, self.interval.label())
    }
}

/// Known Polymarket series IDs by asset + interval.
///
/// Slug formats vary by interval:
///   5m/15m/4h: {asset}-updown-{interval}-{unix_ts}  (slug-based discovery works)
///   1h:        bitcoin-up-or-down-{month}-{day}-{hour}am/pm-et  (human-readable, series_id only)
fn default_series_id(asset: &str, interval: &Interval) -> &'static str {
    match (asset, interval) {
        ("btc", Interval::M5) => "10684",
        ("btc", Interval::M15) => "10192",
        ("btc", Interval::H1) => "10114",
        ("btc", Interval::H4) => "10331",
        ("eth", Interval::M15) => "10191",
        ("sol", Interval::M15) => "10423",
        ("xrp", Interval::M15) => "10422",
        _ => "10684",
    }
}
