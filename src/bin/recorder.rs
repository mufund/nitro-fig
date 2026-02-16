//! Feed recorder: continuously records market data (Binance WS + Polymarket REST).
//! Outputs: logs/{interval}/{slug}/binance.csv, polymarket.csv, market_info.txt
//! Supports any asset/interval via ASSET and INTERVAL env vars.

use std::fs::{self, File};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::StreamExt;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use polymarket_crypto::config::Config;
use polymarket_crypto::market::discovery::discover_next_market;

#[tokio::main]
async fn main() {
    let config = Config::from_env();
    let client = reqwest::Client::new();

    eprintln!("╔══════════════════════════════════════════════════╗");
    eprintln!("║  Polymarket {} {} Feed Recorder", config.asset_label(), config.interval.label());
    eprintln!("║  Series: {}", config.series_id);
    eprintln!("╚══════════════════════════════════════════════════╝");

    loop {
        // Discover next market
        let market = match discover_next_market(&client, &config).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[REC] Discovery failed: {}. Retrying in 10s...", e);
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                continue;
            }
        };

        let now_ms = chrono::Utc::now().timestamp_millis();
        let wait_ms = (market.start_ms - 5_000 - now_ms).max(0);
        eprintln!(
            "[REC] Next: {} | starts in {:.0}s",
            market.slug,
            wait_ms as f64 / 1000.0,
        );

        if wait_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms as u64)).await;
        }

        // Set up output directory
        let dir = format!("logs/{}/{}", config.interval.label(), market.slug);
        fs::create_dir_all(&dir).ok();

        // Write market info
        {
            let mut f = File::create(format!("{}/market_info.txt", dir)).unwrap();
            writeln!(f, "slug={}", market.slug).ok();
            writeln!(f, "start_ms={}", market.start_ms).ok();
            writeln!(f, "end_ms={}", market.end_ms).ok();
            writeln!(f, "up_token_id={}", market.up_token_id).ok();
            writeln!(f, "down_token_id={}", market.down_token_id).ok();
        }

        let stop = Arc::new(AtomicBool::new(false));

        // Spawn Binance recorder
        let bn_stop = stop.clone();
        let bn_dir = dir.clone();
        let bn_ws = config.binance_ws.clone();
        let bn_ws_fallback = config.binance_ws_fallback.clone();
        let bn_handle = tokio::spawn(async move {
            record_binance(&bn_ws, &bn_ws_fallback, &bn_dir, bn_stop).await;
        });

        // Spawn Polymarket poller
        let pm_stop = stop.clone();
        let pm_dir = dir.clone();
        let pm_up = market.up_token_id.clone();
        let pm_down = market.down_token_id.clone();
        let pm_client = client.clone();
        let pm_handle = tokio::spawn(async move {
            record_polymarket(&pm_client, &pm_dir, &pm_up, &pm_down, pm_stop).await;
        });

        // Wait until market end + 30s buffer
        let end_wait = (market.end_ms + 30_000 - chrono::Utc::now().timestamp_millis()).max(0);
        tokio::time::sleep(tokio::time::Duration::from_millis(end_wait as u64)).await;

        stop.store(true, Ordering::Relaxed);
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        bn_handle.abort();
        pm_handle.abort();

        eprintln!("[REC] Market {} recording complete\n", market.slug);
    }
}

async fn record_binance(ws_url: &str, ws_fallback: &str, dir: &str, stop: Arc<AtomicBool>) {
    let mut file = File::create(format!("{}/binance.csv", dir)).unwrap();
    writeln!(file, "recv_time,recv_ts_ms,trade_ts_ms,price,qty,side").ok();

    let ws = match connect_async(ws_url).await {
        Ok((ws, _)) => ws,
        Err(e) => {
            eprintln!("[BN-REC] Primary failed: {}, trying fallback", e);
            match connect_async(ws_fallback).await {
                Ok((ws, _)) => ws,
                Err(e2) => {
                    eprintln!("[BN-REC] Fallback failed: {}", e2);
                    return;
                }
            }
        }
    };

    eprintln!("[BN-REC] Connected");
    let (_, mut read) = ws.split();
    let mut count = 0u64;

    while let Some(msg) = read.next().await {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if let Ok(Message::Text(text)) = msg {
            let now = chrono::Utc::now();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                let price = v["p"].as_str().unwrap_or("0");
                let qty = v["q"].as_str().unwrap_or("0");
                let ts = v["T"].as_i64().unwrap_or(0);
                let side = if v["m"].as_bool().unwrap_or(false) {
                    "sell"
                } else {
                    "buy"
                };
                writeln!(
                    file,
                    "{},{},{},{},{},{}",
                    now.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                    now.timestamp_millis(),
                    ts,
                    price,
                    qty,
                    side,
                ).ok();
                count += 1;
                if count % 1000 == 0 {
                    eprintln!("[BN-REC] {} trades", count);
                    file.flush().ok();
                }
            }
        }
    }
    file.flush().ok();
    eprintln!("[BN-REC] Done: {} trades", count);
}

async fn record_polymarket(
    client: &reqwest::Client,
    dir: &str,
    up_token: &str,
    down_token: &str,
    stop: Arc<AtomicBool>,
) {
    let mut file = File::create(format!("{}/polymarket.csv", dir)).unwrap();
    writeln!(file, "recv_time,recv_ts_ms,up_price,down_price,up_bid,up_ask,down_bid,down_ask").ok();

    let mut count = 0u64;
    let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(300));

    while !stop.load(Ordering::Relaxed) {
        interval.tick().await;

        let now = chrono::Utc::now();

        let up_url = format!(
            "https://clob.polymarket.com/price?token_id={}&side=buy",
            up_token
        );
        let down_url = format!(
            "https://clob.polymarket.com/price?token_id={}&side=buy",
            down_token
        );

        let (up_resp, down_resp) = tokio::join!(
            client.get(&up_url).send(),
            client.get(&down_url).send(),
        );

        let up_price = parse_clob_price(up_resp.ok()).await;
        let down_price = parse_clob_price(down_resp.ok()).await;

        let up_book_url = format!(
            "https://clob.polymarket.com/book?token_id={}",
            up_token
        );
        let down_book_url = format!(
            "https://clob.polymarket.com/book?token_id={}",
            down_token
        );

        let (up_book_resp, down_book_resp) = tokio::join!(
            client.get(&up_book_url).send(),
            client.get(&down_book_url).send(),
        );

        let (up_bid, up_ask) = parse_book(up_book_resp.ok()).await;
        let (down_bid, down_ask) = parse_book(down_book_resp.ok()).await;

        writeln!(
            file,
            "{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}",
            now.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
            now.timestamp_millis(),
            up_price,
            down_price,
            up_bid,
            up_ask,
            down_bid,
            down_ask,
        ).ok();

        count += 1;
        if count % 50 == 0 {
            eprintln!("[PM-REC] {} polls", count);
            file.flush().ok();
        }
    }
    file.flush().ok();
    eprintln!("[PM-REC] Done: {} polls", count);
}

async fn parse_clob_price(resp: Option<reqwest::Response>) -> f64 {
    let resp = match resp {
        Some(r) => r,
        None => return 0.0,
    };
    let text = resp.text().await.unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    v["price"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v["price"].as_f64())
        .unwrap_or(0.0)
}

async fn parse_book(resp: Option<reqwest::Response>) -> (f64, f64) {
    let resp = match resp {
        Some(r) => r,
        None => return (0.0, 0.0),
    };
    let text = resp.text().await.unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();

    let bid = v["bids"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|b| {
            b["price"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| b["price"].as_f64())
        })
        .unwrap_or(0.0);

    let ask = v["asks"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|b| {
            b["price"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| b["price"].as_f64())
        })
        .unwrap_or(0.0);

    (bid, ask)
}
