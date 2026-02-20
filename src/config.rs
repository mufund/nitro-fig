/// Trading interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

    /// Binance klines API interval string: "5m", "15m", "1h", "4h".
    pub fn binance_kline_label(&self) -> &'static str {
        match self {
            Interval::M5 => "5m",
            Interval::M15 => "15m",
            Interval::H1 => "1h",
            Interval::H4 => "4h",
        }
    }

    /// How many seconds before market start to wake up.
    /// Short intervals wake early (10s), long intervals wake earlier (30s for 1h, 60s for 4h).
    pub fn pre_wake_secs(&self) -> i64 {
        match self {
            Interval::M5 => 10,
            Interval::M15 => 15,
            Interval::H1 => 30,
            Interval::H4 => 60,
        }
    }

    /// How many seconds to wait after market end before settling.
    /// Longer intervals get more buffer for Binance candle close to finalize.
    pub fn post_end_buffer_secs(&self) -> i64 {
        match self {
            Interval::M5 => 10,
            Interval::M15 => 10,
            Interval::H1 => 15,
            Interval::H4 => 30,
        }
    }

    /// Open strategies (strike_misalign) window in milliseconds.
    /// Scales with interval: 15s for 5m, 30s for 15m, 120s for 1h, 300s for 4h.
    pub fn open_window_ms(&self) -> i64 {
        match self {
            Interval::M5 => 15_000,
            Interval::M15 => 30_000,
            Interval::H1 => 120_000,
            Interval::H4 => 300_000,
        }
    }

    /// Compute the candle boundary (start) timestamp in milliseconds for the
    /// candle that contains `now_ms`. E.g. for H1, if now_ms is 2:03:15,
    /// returns 2:00:00 in ms.
    pub fn candle_boundary_ms(&self, now_ms: i64) -> i64 {
        let ws_ms = self.window_ms();
        (now_ms / ws_ms) * ws_ms
    }

    /// Recorder: how many seconds after market end to keep recording.
    pub fn recorder_post_end_secs(&self) -> i64 {
        match self {
            Interval::M5 => 30,
            Interval::M15 => 30,
            Interval::H1 => 60,
            Interval::H4 => 120,
        }
    }

    /// Recorder: how many seconds before market start to wake up and start recording.
    pub fn recorder_pre_wake_secs(&self) -> i64 {
        match self {
            Interval::M5 => 5,
            Interval::M15 => 10,
            Interval::H1 => 20,
            Interval::H4 => 30,
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

    // Risk (legacy)
    pub max_position_usd: f64,
    pub max_orders_per_market: u32,
    pub cooldown_ms: i64,

    // Bankroll & portfolio risk
    pub bankroll: f64,
    pub max_total_exposure_frac: f64,
    pub daily_loss_halt_frac: f64,
    pub weekly_loss_halt_frac: f64,

    // Oracle model
    pub oracle_beta: f64,
    pub oracle_delta_s: f64,

    // EWMA
    pub ewma_lambda: f64,
    /// Minimum annualized vol (e.g. 0.30 = 30%). Converted to per-second floor.
    /// Prevents the model from becoming overconfident during low-vol periods.
    pub sigma_floor_annual: f64,

    // Strategy toggles — set to false to disable individual strategies
    pub strategy_latency_arb: bool,
    pub strategy_certainty_capture: bool,
    pub strategy_convexity_fade: bool,
    pub strategy_strike_misalign: bool,
    pub strategy_lp_extreme: bool,
    pub strategy_cross_timeframe: bool,

    // Mode
    pub dry_run: bool,

    // Polymarket CLOB credentials (live execution only)
    pub polymarket_private_key: Option<String>,
    pub polymarket_funder_address: Option<String>,
    /// Signature type: 0=EOA, 1=Poly Proxy, 2=Gnosis Safe
    pub polymarket_signature_type: u8,
}

impl Config {
    pub fn from_env() -> Self {
        let asset = std::env::var("ASSET")
            .unwrap_or_else(|_| "btc".into())
            .to_lowercase();
        let interval = Interval::from_str(
            &std::env::var("INTERVAL").unwrap_or_else(|_| "5m".into()),
        );

        let binance_ws = std::env::var("BINANCE_WS").unwrap_or_else(|_| {
            format!("wss://stream.binance.com:9443/ws/{}usdt@trade", asset)
        });
        let binance_ws_fallback = std::env::var("BINANCE_WS_FALLBACK").unwrap_or_else(|_| {
            format!("wss://stream.binance.us:9443/ws/{}usd@trade", asset)
        });

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
            bankroll: std::env::var("BANKROLL")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1000.0),
            max_total_exposure_frac: std::env::var("MAX_EXPOSURE_FRAC")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.15),
            daily_loss_halt_frac: std::env::var("DAILY_LOSS_HALT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(-0.03),
            weekly_loss_halt_frac: std::env::var("WEEKLY_LOSS_HALT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(-0.08),
            oracle_beta: std::env::var("ORACLE_BETA")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0),
            oracle_delta_s: std::env::var("ORACLE_DELTA_S")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(2.0),
            ewma_lambda: std::env::var("EWMA_LAMBDA")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.94),
            sigma_floor_annual: std::env::var("SIGMA_FLOOR_ANNUAL")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.30),
            strategy_latency_arb: std::env::var("STRAT_LATENCY_ARB")
                .map(|v| v != "0" && v.to_lowercase() != "false")
                .unwrap_or(true),
            strategy_certainty_capture: std::env::var("STRAT_CERTAINTY_CAPTURE")
                .map(|v| v != "0" && v.to_lowercase() != "false")
                .unwrap_or(true),
            strategy_convexity_fade: std::env::var("STRAT_CONVEXITY_FADE")
                .map(|v| v != "0" && v.to_lowercase() != "false")
                .unwrap_or(true),
            strategy_strike_misalign: std::env::var("STRAT_STRIKE_MISALIGN")
                .map(|v| v != "0" && v.to_lowercase() != "false")
                .unwrap_or(true),
            strategy_lp_extreme: std::env::var("STRAT_LP_EXTREME")
                .map(|v| v != "0" && v.to_lowercase() != "false")
                .unwrap_or(true),
            strategy_cross_timeframe: std::env::var("STRAT_CROSS_TF")
                .map(|v| v == "1" || v.to_lowercase() == "true")
                .unwrap_or(false),
            dry_run: std::env::var("DRY_RUN")
                .map(|v| v == "1" || v.to_lowercase() == "true")
                .unwrap_or(true),
            polymarket_private_key: std::env::var("POLYMARKET_PRIVATE_KEY").ok(),
            polymarket_funder_address: std::env::var("POLYMARKET_FUNDER_ADDRESS").ok(),
            polymarket_signature_type: std::env::var("POLYMARKET_SIG_TYPE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: make_config() helper creates a Config with all 5 active strategies enabled.
    /// Expected: All active strategies default to true, cross_timeframe to false.
    #[test]
    fn test_strategy_toggles_default_true() {
        let config = crate::strategies::test_helpers::make_config();
        assert!(config.strategy_latency_arb, "latency_arb should default to enabled");
        assert!(config.strategy_certainty_capture, "certainty_capture should default to enabled");
        assert!(config.strategy_convexity_fade, "convexity_fade should default to enabled");
        assert!(config.strategy_strike_misalign, "strike_misalign should default to enabled");
        assert!(config.strategy_lp_extreme, "lp_extreme should default to enabled");
    }

    /// Scenario: cross_timeframe is disabled by default since no cross-market feed exists.
    /// Expected: strategy_cross_timeframe is false in the default config.
    #[test]
    fn test_cross_timeframe_default_false() {
        let config = crate::strategies::test_helpers::make_config();
        assert!(!config.strategy_cross_timeframe, "cross_timeframe should default to disabled");
    }

    /// Scenario: A strategy toggle can be set to false to disable it.
    /// Expected: Only the disabled strategy is off, others remain enabled.
    #[test]
    fn test_disable_single_strategy() {
        let mut config = crate::strategies::test_helpers::make_config();
        config.strategy_latency_arb = false;
        assert!(!config.strategy_latency_arb, "latency_arb should be disabled");
        assert!(config.strategy_certainty_capture, "other strategies should stay enabled");
        assert!(config.strategy_convexity_fade, "other strategies should stay enabled");
    }

    /// Scenario: cross_timeframe toggle set to true to enable the disabled strategy.
    /// Expected: strategy_cross_timeframe becomes true.
    #[test]
    fn test_enable_cross_timeframe() {
        let mut config = crate::strategies::test_helpers::make_config();
        config.strategy_cross_timeframe = true;
        assert!(config.strategy_cross_timeframe, "cross_timeframe should be enabled when set to true");
    }

    /// Scenario: Interval parsing with known and unknown interval strings.
    /// Expected: Known strings map correctly, unknown falls back to M5.
    #[test]
    fn test_interval_from_str() {
        assert_eq!(Interval::from_str("5m"), Interval::M5);
        assert_eq!(Interval::from_str("15m"), Interval::M15);
        assert_eq!(Interval::from_str("1h"), Interval::H1);
        assert_eq!(Interval::from_str("4h"), Interval::H4);
        assert_eq!(Interval::from_str("unknown"), Interval::M5);
    }

    /// Scenario: Each interval has correct window duration in seconds and milliseconds.
    /// Expected: M5=300s, M15=900s, H1=3600s, H4=14400s (and ms = s * 1000).
    #[test]
    fn test_interval_window_durations() {
        assert_eq!(Interval::M5.window_secs(), 300);
        assert_eq!(Interval::M5.window_ms(), 300_000);
        assert_eq!(Interval::H4.window_secs(), 14400);
        assert_eq!(Interval::H4.window_ms(), 14_400_000);
    }

    /// Scenario: Known asset+interval combos return specific series IDs.
    /// Expected: btc/5m → "10684", eth/15m → "10191", unknown → "10684" fallback.
    #[test]
    fn test_default_series_id() {
        assert_eq!(default_series_id("btc", &Interval::M5), "10684");
        assert_eq!(default_series_id("eth", &Interval::M15), "10191");
        assert_eq!(default_series_id("doge", &Interval::M5), "10684");
    }
}
