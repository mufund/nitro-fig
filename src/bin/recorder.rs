//! Feed recorder: records market data (Binance WS + Polymarket CLOB WS).
//! Outputs per market: logs/{interval}/{slug}/binance.csv, polymarket.csv, book.csv, market_info.txt
//! Supports any asset/interval via ASSET and INTERVAL env vars.
//! Usage: recorder [--cycles N]  (default: infinite)

use std::fs::{self, File};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use polymarket_crypto::config::Config;
use polymarket_crypto::market::discovery::discover_next_market;

#[tokio::main]
async fn main() {
    let config = Config::from_env();
    let client = reqwest::Client::new();

    // Parse --cycles N from CLI args
    let args: Vec<String> = std::env::args().collect();
    let max_cycles: Option<u32> = args
        .windows(2)
        .find(|w| w[0] == "--cycles")
        .and_then(|w| w[1].parse().ok());

    eprintln!("╔══════════════════════════════════════════════════╗");
    eprintln!(
        "║  Polymarket {} {} Feed Recorder (WS)",
        config.asset_label(),
        config.interval.label()
    );
    eprintln!("║  Series: {}", config.series_id);
    if let Some(n) = max_cycles {
        eprintln!("║  Cycles: {}", n);
    }
    eprintln!("╚══════════════════════════════════════════════════╝");

    let mut completed = 0u32;

    loop {
        if let Some(max) = max_cycles {
            if completed >= max {
                eprintln!("[REC] Completed {} cycles, exiting", completed);
                break;
            }
        }

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
            "[REC] Next: {} | starts in {:.0}s | cycle {}/{}",
            market.slug,
            wait_ms as f64 / 1000.0,
            completed + 1,
            max_cycles.map_or("inf".to_string(), |n| n.to_string()),
        );

        if wait_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms as u64)).await;
        }

        // Set up output directory
        let dir = format!("logs/{}/{}", config.interval.label(), market.slug);
        fs::create_dir_all(&dir).ok();

        // Fetch strike from Binance spot before market starts
        let strike = fetch_binance_price(&client, &config).await;
        eprintln!("[REC] Strike (pre-open Binance): ${:.2}", strike);

        // Write market info (including strike)
        {
            let mut f = File::create(format!("{}/market_info.txt", dir)).unwrap();
            writeln!(f, "slug={}", market.slug).ok();
            writeln!(f, "start_ms={}", market.start_ms).ok();
            writeln!(f, "end_ms={}", market.end_ms).ok();
            writeln!(f, "up_token_id={}", market.up_token_id).ok();
            writeln!(f, "down_token_id={}", market.down_token_id).ok();
            writeln!(f, "strike={:.2}", strike).ok();
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

        // Spawn Polymarket WS recorder (quotes + full book depth)
        let pm_stop = stop.clone();
        let pm_dir = dir.clone();
        let pm_ws = config.polymarket_clob_ws.clone();
        let pm_up = market.up_token_id.clone();
        let pm_down = market.down_token_id.clone();
        let pm_handle = tokio::spawn(async move {
            record_polymarket_ws(&pm_ws, &pm_dir, &pm_up, &pm_down, pm_stop).await;
        });

        // Wait until market end + 30s buffer
        let end_wait = (market.end_ms + 30_000 - chrono::Utc::now().timestamp_millis()).max(0);
        tokio::time::sleep(tokio::time::Duration::from_millis(end_wait as u64)).await;

        stop.store(true, Ordering::Relaxed);
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        bn_handle.abort();
        pm_handle.abort();

        completed += 1;
        eprintln!(
            "[REC] Market {} recording complete ({}/{})\n",
            market.slug,
            completed,
            max_cycles.map_or("inf".to_string(), |n| n.to_string()),
        );
    }
}

/// Fetch current BTC spot price from Binance REST for strike.
async fn fetch_binance_price(client: &reqwest::Client, config: &Config) -> f64 {
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
            eprintln!("[REC] Failed to fetch Binance price for strike: {}", e);
            0.0
        }
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
                )
                .ok();
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

/// Record Polymarket via CLOB WebSocket — captures both quotes and full book depth.
/// Writes two files:
///   polymarket.csv — best bid/ask per token (same schema as before, for backward compat)
///   book.csv       — full orderbook snapshots (all levels, both tokens)
async fn record_polymarket_ws(
    ws_url: &str,
    dir: &str,
    up_token: &str,
    down_token: &str,
    stop: Arc<AtomicBool>,
) {
    let mut quote_file = File::create(format!("{}/polymarket.csv", dir)).unwrap();
    writeln!(
        quote_file,
        "recv_time,recv_ts_ms,up_price,down_price,up_bid,up_ask,down_bid,down_ask"
    )
    .ok();

    let mut book_file = File::create(format!("{}/book.csv", dir)).unwrap();
    // book.csv: one row per level per snapshot
    // token=up|down, side=bid|ask, level=0..N, price, size
    writeln!(
        book_file,
        "recv_time,recv_ts_ms,token,side,level,price,size"
    )
    .ok();

    let mut quote_count = 0u64;
    let mut book_count = 0u64;

    // Track latest best bid/ask for each token (for polymarket.csv rows)
    let mut up_bid = 0.0_f64;
    let mut up_ask = 0.0_f64;
    let mut down_bid = 0.0_f64;
    let mut down_ask = 0.0_f64;

    let ws = match connect_async(ws_url).await {
        Ok((ws, _)) => ws,
        Err(e) => {
            eprintln!("[PM-REC] WS connect failed: {}", e);
            return;
        }
    };
    eprintln!("[PM-REC] WS connected");
    let (mut write, mut read) = ws.split();

    // Subscribe to both token IDs
    let sub = serde_json::json!({
        "assets_ids": [up_token, down_token],
        "type": "market",
        "custom_feature_enabled": true
    });
    if let Err(e) = write.send(Message::Text(sub.to_string())).await {
        eprintln!("[PM-REC] Subscribe failed: {}", e);
        return;
    }
    eprintln!(
        "[PM-REC] Subscribed to UP={}... DOWN={}...",
        &up_token[..8.min(up_token.len())],
        &down_token[..8.min(down_token.len())]
    );

    let mut ping_interval = tokio::time::interval(tokio::time::Duration::from_secs(10));

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        tokio::select! {
            msg = read.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => {
                        eprintln!("[PM-REC] WS error: {}", e);
                        break;
                    }
                    None => {
                        eprintln!("[PM-REC] Stream ended");
                        break;
                    }
                };

                if let Message::Text(text) = msg {
                    let now = chrono::Utc::now();
                    let now_str = now.format("%Y-%m-%dT%H:%M:%S%.3fZ");
                    let now_ms = now.timestamp_millis();

                    let v: serde_json::Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    let events = if v.is_array() {
                        match v.as_array() {
                            Some(a) => a.clone(),
                            None => continue,
                        }
                    } else if v.is_object() {
                        vec![v]
                    } else {
                        continue
                    };

                    let mut wrote_quote = false;

                    for event in &events {
                        let event_type = event.get("event_type")
                            .and_then(|e| e.as_str())
                            .unwrap_or("");
                        let asset_id = event.get("asset_id")
                            .and_then(|a| a.as_str())
                            .unwrap_or("");

                        let is_up = asset_id == up_token;
                        let is_down = asset_id == down_token;
                        if !is_up && !is_down {
                            continue;
                        }

                        // Handle quote updates (best_bid_ask, price_change)
                        if event_type == "best_bid_ask" || event_type == "price_change" {
                            let bid = parse_json_f64(event.get("best_bid"))
                                .or_else(|| parse_json_f64(event.get("price")));
                            let ask = parse_json_f64(event.get("best_ask"));

                            if is_up {
                                if let Some(b) = bid { up_bid = b; }
                                if let Some(a) = ask { up_ask = a; }
                            } else {
                                if let Some(b) = bid { down_bid = b; }
                                if let Some(a) = ask { down_ask = a; }
                            }
                            wrote_quote = true;
                        }

                        // Handle book events (full depth)
                        if event_type == "book" {
                            let token_label = if is_up { "up" } else { "down" };

                            // Extract best bid/ask from book for quote tracking
                            if let Some(bids) = event.get("bids").and_then(|v| v.as_array()) {
                                for (i, level) in bids.iter().enumerate() {
                                    let price = parse_json_f64(level.get("price")).unwrap_or(0.0);
                                    let size = parse_json_f64(level.get("size")).unwrap_or(0.0);
                                    if price > 0.0 && size > 0.0 {
                                        writeln!(
                                            book_file,
                                            "{},{},{},bid,{},{:.4},{:.2}",
                                            now_str, now_ms, token_label, i, price, size,
                                        ).ok();
                                    }
                                    if i == 0 && price > 0.0 {
                                        if is_up { up_bid = price; } else { down_bid = price; }
                                    }
                                }
                            }
                            if let Some(asks) = event.get("asks").and_then(|v| v.as_array()) {
                                for (i, level) in asks.iter().enumerate() {
                                    let price = parse_json_f64(level.get("price")).unwrap_or(0.0);
                                    let size = parse_json_f64(level.get("size")).unwrap_or(0.0);
                                    if price > 0.0 && size > 0.0 {
                                        writeln!(
                                            book_file,
                                            "{},{},{},ask,{},{:.4},{:.2}",
                                            now_str, now_ms, token_label, i, price, size,
                                        ).ok();
                                    }
                                    if i == 0 && price > 0.0 {
                                        if is_up { up_ask = price; } else { down_ask = price; }
                                    }
                                }
                            }
                            book_count += 1;
                            wrote_quote = true;
                        }
                    }

                    // Write a quote row whenever we got any update
                    if wrote_quote {
                        // up_price/down_price = midpoints (backward compat)
                        let up_mid = if up_bid > 0.0 && up_ask > 0.0 {
                            (up_bid + up_ask) / 2.0
                        } else {
                            up_bid.max(up_ask)
                        };
                        let down_mid = if down_bid > 0.0 && down_ask > 0.0 {
                            (down_bid + down_ask) / 2.0
                        } else {
                            down_bid.max(down_ask)
                        };

                        writeln!(
                            quote_file,
                            "{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}",
                            now_str, now_ms, up_mid, down_mid,
                            up_bid, up_ask, down_bid, down_ask,
                        ).ok();
                        quote_count += 1;
                    }

                    if quote_count % 100 == 0 && quote_count > 0 {
                        eprintln!("[PM-REC] {} quotes, {} book snapshots", quote_count, book_count);
                        quote_file.flush().ok();
                        book_file.flush().ok();
                    }
                }
            }
            _ = ping_interval.tick() => {
                let _ = write.send(Message::Ping(vec![])).await;
            }
        }
    }

    quote_file.flush().ok();
    book_file.flush().ok();
    eprintln!(
        "[PM-REC] Done: {} quotes, {} book snapshots",
        quote_count, book_count
    );
}

fn parse_json_f64(val: Option<&serde_json::Value>) -> Option<f64> {
    val.and_then(|v| {
        v.as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| v.as_f64())
    })
}
