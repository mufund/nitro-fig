mod config;
mod engine;
mod feeds;
mod gateway;
mod market;
mod strategies;
mod telemetry;
mod types;

use tokio::sync::mpsc;

use config::Config;
use engine::runner::run_engine;
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
    eprintln!("║  Max position: ${:.0} | Max orders: {}", config.max_position_usd, config.max_orders_per_market);
    eprintln!("╚══════════════════════════════════════════════════╝");

    loop {
        // 1. Discover next 5m market
        let market = match discover_next_market(&http, &config).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[MAIN] Discovery failed: {}. Retrying in 10s...", e);
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                continue;
            }
        };

        let now_ms = chrono::Utc::now().timestamp_millis();
        let wait_ms = (market.start_ms - 10_000 - now_ms).max(0);
        eprintln!(
            "[MAIN] Next market: {} | starts in {:.0}s | UP={} DOWN={}",
            market.slug,
            wait_ms as f64 / 1000.0,
            &market.up_token_id[..8.min(market.up_token_id.len())],
            &market.down_token_id[..8.min(market.down_token_id.len())],
        );

        // 2. Wait until 10s before market start (WS warmup)
        if wait_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms as u64)).await;
        }

        // 3. Create channels
        let (feed_tx, mut feed_rx) = mpsc::channel::<FeedEvent>(4096);
        let (order_tx, order_rx) = mpsc::channel::<Order>(64);
        let (telem_tx, telem_rx) = mpsc::channel::<TelemetryEvent>(4096);

        // 4. Spawn Binance WS feed producer
        let binance_feed_tx = feed_tx.clone();
        let bn_url = config.binance_ws.clone();
        let bn_fallback = config.binance_ws_fallback.clone();
        let binance_handle = tokio::spawn(async move {
            binance_feed(binance_feed_tx, bn_url, bn_fallback).await;
        });

        // 5. Wait for first Binance price → use as strike
        eprintln!("[MAIN] Waiting for first Binance price...");
        let mut strike = 0.0_f64;
        let mut buffered_events: Vec<FeedEvent> = Vec::new();

        loop {
            if let Some(event) = feed_rx.recv().await {
                match &event {
                    FeedEvent::BinanceTrade(t) => {
                        if strike == 0.0 {
                            strike = t.price;
                            eprintln!("[MAIN] Strike set: ${:.2}", strike);
                            buffered_events.push(event);
                            break;
                        }
                    }
                    _ => {
                        buffered_events.push(event);
                    }
                }
            }
        }

        let mut market = market;
        market.strike = strike;

        // 6. Spawn Polymarket CLOB WS feed producer
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

        // Drop our copy of feed_tx so engine's feed_rx will close when all producers stop
        drop(feed_tx);

        // 10. Re-inject buffered events into feed_rx
        //     Since we dropped our feed_tx, we need a new approach.
        //     We'll create a merged receiver that first yields buffered events.
        //     Simpler: just process buffered events inline before the engine loop.
        //     Actually, let's create a wrapper channel.
        let (merged_tx, merged_rx) = mpsc::channel::<FeedEvent>(4096);

        // Send buffered events
        for evt in buffered_events {
            let _ = merged_tx.send(evt).await;
        }

        // Spawn a relay task: feed_rx → merged_tx
        let relay_handle = tokio::spawn(async move {
            while let Some(evt) = feed_rx.recv().await {
                if merged_tx.send(evt).await.is_err() {
                    break;
                }
            }
        });

        // 11. Run core engine (this blocks until market ends)
        run_engine(market.clone(), merged_rx, order_tx, telem_tx, &config).await;

        // 12. Cleanup
        binance_handle.abort();
        pm_handle.abort();
        tick_handle.abort();
        relay_handle.abort();
        // gw_handle and telem_handle drain naturally

        // Wait a bit for telemetry to flush
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        gw_handle.abort();
        telem_handle.abort();

        eprintln!("[MAIN] Market {} completed. Discovering next...\n", market.slug);
    }
}
