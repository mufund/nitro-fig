mod config;
mod engine;
mod feeds;
mod gateway;
mod market;
mod math;
mod strategies;
mod telemetry;
mod types;

use tokio::sync::{mpsc, watch};

use config::Config;
use engine::runner::run_engine;
use engine::state::BinanceState;
use feeds::binance::binance_feed;
use feeds::polymarket::polymarket_feed;
use gateway::order::order_gateway;
use market::discovery::discover_next_market;
use telemetry::writer::telemetry_writer;
use types::*;

#[tokio::main]
async fn main() {
    let config = Config::from_env();
    let http = reqwest::Client::new();

    eprintln!("╔══════════════════════════════════════════════════╗");
    eprintln!("║  Polymarket {} {} Trading System", config.asset_label(), config.interval.label());
    eprintln!("║  Series: {} | Dry run: {}", config.series_id, config.dry_run);
    eprintln!("║  Bankroll: ${:.0} | Max exposure: {:.0}%", config.bankroll, config.max_total_exposure_frac * 100.0);
    eprintln!("║  Oracle: β={:.2} δ={:.1}s | EWMA λ={:.2}", config.oracle_beta, config.oracle_delta_s, config.ewma_lambda);
    let secs_per_year: f64 = 365.25 * 24.0 * 3600.0;
    let sigma_floor_ps = config.sigma_floor_annual / secs_per_year.sqrt();
    eprintln!("║  Vol floor: {:.0}% annual → σ_floor={:.6}/s", config.sigma_floor_annual * 100.0, sigma_floor_ps);
    eprintln!("╚══════════════════════════════════════════════════╝");

    // ── Persistent Binance feed (lives across all markets) ──
    let (feed_swap_tx, feed_swap_rx) = watch::channel::<Option<mpsc::Sender<FeedEvent>>>(None);
    let (price_tx, mut price_rx) = watch::channel::<f64>(0.0);

    let bn_url = config.binance_ws.clone();
    let bn_fallback = config.binance_ws_fallback.clone();
    let _binance_handle = tokio::spawn(async move {
        binance_feed(feed_swap_rx, price_tx, bn_url, bn_fallback).await;
    });

    // Wait for first Binance price (only once, at startup)
    eprintln!("[MAIN] Waiting for first Binance price...");
    while *price_rx.borrow() == 0.0 {
        if price_rx.changed().await.is_err() {
            eprintln!("[MAIN] Binance feed died before first price");
            return;
        }
    }
    eprintln!("[MAIN] Binance online: ${:.2}", *price_rx.borrow());

    // Persistent Binance state — created once, threaded through every market
    let mut binance_state = BinanceState::new(
        config.ewma_lambda,
        10,                          // min_samples: 10 one-second samples
        config.sigma_floor_annual,
        60_000,                      // VWAP window: 60s
        30_000,                      // Regime window: 30s
    );

    loop {
        // 1. Discover next market
        let market = match discover_next_market(&http, &config).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[MAIN] Discovery failed: {}. Retrying in 10s...", e);
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                continue;
            }
        };

        let now_ms = chrono::Utc::now().timestamp_millis();
        let pre_wake_ms = config.interval.pre_wake_secs() * 1000;
        let wait_ms = (market.start_ms - pre_wake_ms - now_ms).max(0);
        eprintln!(
            "[MAIN] Next market: {} | starts in {:.0}s | pre_wake={}s | UP={} DOWN={}",
            market.slug,
            wait_ms as f64 / 1000.0,
            config.interval.pre_wake_secs(),
            &market.up_token_id[..8.min(market.up_token_id.len())],
            &market.down_token_id[..8.min(market.down_token_id.len())],
        );

        // 2. Wait until pre_wake_secs before market start
        if wait_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms as u64)).await;
        }

        // 3. Set strike from Binance candle open (klines API for all intervals).
        //    The candle open is the correct reference price per Polymarket resolution rules.
        //    For short intervals (5m) this is ~identical to a spot snapshot.
        //    For long intervals (1h, 4h) this is the candle open at the boundary.
        let mut market = market;
        market.strike = fetch_binance_candle_open(&http, &config).await;
        eprintln!("[MAIN] Strike set (candle open): ${:.2}", market.strike);

        // 4. Create per-market channels
        let (feed_tx, feed_rx) = mpsc::channel::<FeedEvent>(4096);
        let (order_tx, order_rx) = mpsc::channel::<Order>(64);
        let (telem_tx, telem_rx) = mpsc::channel::<TelemetryEvent>(4096);

        // 5. Activate Binance → this market's feed channel
        let _ = feed_swap_tx.send(Some(feed_tx.clone()));

        // 6. Spawn Polymarket CLOB WS feed (per-market, new token IDs)
        let pm_feed_tx = feed_tx.clone();
        let pm_url = config.polymarket_clob_ws.clone();
        let up_tok = market.up_token_id.clone();
        let down_tok = market.down_token_id.clone();
        let pm_handle = tokio::spawn(async move {
            polymarket_feed(pm_feed_tx, pm_url, up_tok, down_tok).await;
        });

        // 7. Spawn heartbeat (100ms tick events)
        let tick_tx = feed_tx.clone();
        let tick_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(100));
            loop {
                interval.tick().await;
                if tick_tx.send(FeedEvent::Tick).await.is_err() {
                    break;
                }
            }
        });

        // 8. Spawn order gateway
        let gw_feed_tx = feed_tx.clone();
        let gw_telem_tx = telem_tx.clone();
        let dry_run = config.dry_run;
        let gw_handle = tokio::spawn(async move {
            order_gateway(order_rx, gw_feed_tx, gw_telem_tx, dry_run).await;
        });

        // 9. Spawn telemetry writer
        let telem_config = config.clone();
        let telem_slug = market.slug.clone();
        let telem_handle = tokio::spawn(async move {
            telemetry_writer(telem_rx, telem_config, telem_slug).await;
        });

        // Drop our copy of feed_tx so engine's feed_rx closes when all producers stop
        drop(feed_tx);

        // 10. Run core engine (blocks until market ends), returns BinanceState
        binance_state = run_engine(market.clone(), binance_state, feed_rx, order_tx, telem_tx, &config).await;

        // 11. Pause Binance delivery (trades dropped between markets)
        let _ = feed_swap_tx.send(None);

        // 12. Cleanup per-market tasks (NOT Binance — it persists)
        pm_handle.abort();
        tick_handle.abort();

        // Let telemetry flush
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        gw_handle.abort();
        telem_handle.abort();

        eprintln!("[MAIN] Market {} completed. Discovering next...\n", market.slug);
    }
}

/// Fetch the Binance candle OPEN price for the current interval.
///
/// Uses the klines REST API: GET /api/v3/klines?symbol=BTCUSDT&interval=1h&limit=1
/// The open price of the most recent candle is the correct strike reference
/// per Polymarket resolution rules (all intervals).
///
/// Fallback: if klines fails, uses spot ticker price (acceptable for 5m/15m
/// where candle open ≈ spot, but logged as a warning for longer intervals).
async fn fetch_binance_candle_open(client: &reqwest::Client, config: &Config) -> f64 {
    let symbol = format!("{}USDT", config.asset_label().to_uppercase());
    let kline_interval = config.interval.binance_kline_label();
    let url = format!(
        "https://api.binance.com/api/v3/klines?symbol={}&interval={}&limit=1",
        symbol, kline_interval
    );

    match client.get(&url).send().await {
        Ok(resp) => {
            let text = resp.text().await.unwrap_or_default();
            // klines response: [[open_time, open, high, low, close, volume, ...], ...]
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(candle) = v.as_array().and_then(|a| a.first()) {
                    if let Some(open_str) = candle.get(1).and_then(|o| o.as_str()) {
                        if let Ok(open_price) = open_str.parse::<f64>() {
                            eprintln!(
                                "[MAIN] Binance kline {} open=${:.2} (interval={})",
                                symbol, open_price, kline_interval
                            );
                            return open_price;
                        }
                    }
                }
            }
            eprintln!("[MAIN] Failed to parse klines response, falling back to spot");
        }
        Err(e) => {
            eprintln!("[MAIN] Klines fetch failed: {}, falling back to spot", e);
        }
    }

    // Fallback: spot ticker
    if config.interval.window_secs() > 900 {
        eprintln!("[WARN] Using spot price as strike for {}+ interval — candle open preferred", config.interval.label());
    }
    fetch_binance_spot(client, config).await
}

/// Fallback: fetch current BTC spot price from Binance REST.
async fn fetch_binance_spot(client: &reqwest::Client, config: &Config) -> f64 {
    let symbol = format!("{}USDT", config.asset_label().to_uppercase());
    let url = format!(
        "https://api.binance.com/api/v3/ticker/price?symbol={}",
        symbol
    );
    match client.get(&url).send().await {
        Ok(resp) => {
            let text = resp.text().await.unwrap_or_default();
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            v["price"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0)
        }
        Err(e) => {
            eprintln!("[MAIN] Spot ticker fallback failed: {}", e);
            0.0
        }
    }
}
