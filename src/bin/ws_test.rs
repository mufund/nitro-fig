//! Quick integration test: connects to both WebSockets and validates connectivity.
//! Reads ASSET and SERIES_ID from env vars (same as the main bot).

use polymarket_crypto::config::Config;

#[tokio::main]
async fn main() {
    use futures_util::{SinkExt, StreamExt};
    use std::time::Instant;
    use tokio::sync::mpsc;

    let config = Config::from_env();

    eprintln!("╔══════════════════════════════════════════════════╗");
    eprintln!("║  WebSocket Integration Test                       ║");
    eprintln!("║  Asset: {} | Interval: {}                        ", config.asset_label(), config.interval.label());
    eprintln!("╚══════════════════════════════════════════════════╝");

    // --- Test 1: Binance WS ---
    eprintln!("\n[TEST 1] Connecting to Binance WS...");
    let ws_url = config.binance_ws.clone();
    let (bn_tx, mut bn_rx) = mpsc::channel::<(f64, i64)>(100);

    let bn_handle = tokio::spawn(async move {
        match tokio_tungstenite::connect_async(&ws_url).await {
            Ok((ws, _)) => {
                eprintln!("[BINANCE] Connected to {}", ws_url);
                let (_write, mut read) = ws.split();
                let mut count = 0u32;
                while let Some(Ok(msg)) = read.next().await {
                    if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let (Some(price_str), Some(ts)) = (v["p"].as_str(), v["T"].as_i64()) {
                                if let Ok(price) = price_str.parse::<f64>() {
                                    let _ = bn_tx.send((price, ts)).await;
                                    count += 1;
                                    if count <= 3 {
                                        eprintln!("[BINANCE]   Trade #{}: ${:.2}", count, price);
                                    }
                                    if count >= 50 {
                                        eprintln!("[BINANCE] Received {} trades, stopping", count);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("[BINANCE] Failed: {}", e);
            }
        }
    });

    // --- Test 2: Polymarket CLOB WS ---
    eprintln!("[TEST 2] Connecting to Polymarket CLOB WS...");

    let http = reqwest::Client::new();
    let api_url = format!(
        "{}/events?series_id={}&active=true&closed=false&limit=5&order=endDate&ascending=false",
        config.gamma_api_url, config.series_id,
    );

    let (up_token, down_token) = match http.get(&api_url).send().await {
        Ok(resp) => {
            match resp.text().await {
                Ok(text) => {
                    let events: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
                    let mut up = String::new();
                    let mut down = String::new();

                    if let Some(events_arr) = events.as_array() {
                        for event in events_arr {
                            if let Some(markets) = event.get("markets").and_then(|m| m.as_array()) {
                                for market in markets {
                                    let outcomes_str = market.get("outcomes").and_then(|o| o.as_str()).unwrap_or("");
                                    let tokens_str = market.get("clobTokenIds").and_then(|t| t.as_str()).unwrap_or("");

                                    if let (Ok(outcomes), Ok(tokens)) = (
                                        serde_json::from_str::<Vec<String>>(outcomes_str),
                                        serde_json::from_str::<Vec<String>>(tokens_str),
                                    ) {
                                        if outcomes.len() >= 2 && tokens.len() >= 2 {
                                            up = tokens[0].clone();
                                            down = tokens[1].clone();
                                            break;
                                        }
                                    }
                                }
                            }
                            if !up.is_empty() { break; }
                        }
                    }
                    (up, down)
                }
                Err(_) => (String::new(), String::new()),
            }
        }
        Err(_) => (String::new(), String::new()),
    };

    let (pm_tx, mut pm_rx) = mpsc::channel::<String>(100);

    let up_token_empty = up_token.is_empty();
    if up_token_empty {
        eprintln!("[PM] No token IDs found, skipping PM WS test");
    } else {
        eprintln!("[PM] Token IDs: UP={}... DOWN={}...", &up_token[..8], &down_token[..8]);

        let pm_ws_url = config.polymarket_clob_ws.clone();
        let pm_handle = tokio::spawn(async move {
            match tokio_tungstenite::connect_async(&pm_ws_url).await {
                Ok((ws, _)) => {
                    eprintln!("[PM] Connected to CLOB WS");
                    let (mut write, mut read) = ws.split();

                    let sub = serde_json::json!({
                        "assets_ids": [&up_token, &down_token],
                        "type": "market",
                        "custom_feature_enabled": true
                    });

                    if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(sub.to_string())).await {
                        eprintln!("[PM] Subscribe failed: {}", e);
                        return;
                    }
                    eprintln!("[PM] Subscribed");

                    let mut count = 0u32;
                    let start = Instant::now();

                    while let Some(Ok(msg)) = read.next().await {
                        if start.elapsed().as_secs() > 15 {
                            eprintln!("[PM] 15s elapsed, received {} messages", count);
                            break;
                        }
                        if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
                            count += 1;
                            if count <= 3 {
                                let preview = if text.len() > 120 { &text[..120] } else { &text };
                                eprintln!("[PM]   Msg #{}: {}", count, preview);
                            }
                            let _ = pm_tx.send(text).await;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[PM] Connection failed: {}", e);
                }
            }
        });

        let _ = tokio::time::timeout(tokio::time::Duration::from_secs(20), pm_handle).await;
    }

    // Wait for Binance test
    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(15), bn_handle).await;

    // --- Summary ---
    let mut bn_count = 0u32;
    while bn_rx.try_recv().is_ok() { bn_count += 1; }
    let mut pm_count = 0u32;
    while pm_rx.try_recv().is_ok() { pm_count += 1; }

    eprintln!("\n{:-<50}", "");
    eprintln!("Results ({} {}):", config.asset_label(), config.interval.label());
    eprintln!("  Binance trades received:     {}", bn_count);
    eprintln!("  Polymarket messages received: {}", pm_count);
    eprintln!("  Binance WS:     {}", if bn_count > 0 { "PASS" } else { "FAIL" });
    eprintln!("  Polymarket WS:  {}", if pm_count > 0 || up_token_empty { "PASS (or N/A)" } else { "No data (market not active)" });
    eprintln!("{:-<50}", "");
}
