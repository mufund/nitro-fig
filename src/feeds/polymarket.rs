use std::time::Instant;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::types::{FeedEvent, PolymarketQuote};

/// Pure producer: connects to Polymarket CLOB WS, parses best_bid_ask updates.
/// Owns no shared state — only holds a channel sender.
pub async fn polymarket_feed(
    feed_tx: mpsc::Sender<FeedEvent>,
    ws_url: String,
    up_token_id: String,
    down_token_id: String,
) {
    let mut backoff_ms: u64 = 1000;

    loop {
        eprintln!("[PM] Connecting to {}", ws_url);

        let connect_result = connect_async(&ws_url).await;
        let ws = match connect_result {
            Ok((ws, _)) => {
                eprintln!("[PM] Connected");
                backoff_ms = 1000;
                ws
            }
            Err(e) => {
                eprintln!("[PM] Connection failed: {}, retrying in {}ms", e, backoff_ms);
                tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(10_000);
                continue;
            }
        };

        let (mut write, mut read) = ws.split();

        // Subscribe to both token IDs
        let sub = serde_json::json!({
            "assets_ids": [&up_token_id, &down_token_id],
            "type": "market",
            "custom_feature_enabled": true
        });

        if let Err(e) = write.send(Message::Text(sub.to_string())).await {
            eprintln!("[PM] Subscribe failed: {}, reconnecting", e);
            continue;
        }
        eprintln!("[PM] Subscribed to UP={} DOWN={}", &up_token_id[..8.min(up_token_id.len())], &down_token_id[..8.min(down_token_id.len())]);

        // Ping keepalive
        let ping_write = feed_tx.clone(); // not used, just to keep task alive
        let mut ping_interval = tokio::time::interval(tokio::time::Duration::from_secs(10));

        loop {
            tokio::select! {
                msg = read.next() => {
                    let msg = match msg {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => {
                            eprintln!("[PM] WS error: {}, reconnecting", e);
                            break;
                        }
                        None => {
                            eprintln!("[PM] Stream ended, reconnecting");
                            break;
                        }
                    };

                    if let Message::Text(text) = msg {
                        let recv_at = Instant::now();
                        if let Some(quote) = parse_clob_message(&text, recv_at, &up_token_id, &down_token_id) {
                            if feed_tx.send(FeedEvent::PolymarketQuote(quote)).await.is_err() {
                                eprintln!("[PM] Channel closed, exiting");
                                return;
                            }
                        }
                    }
                }
                _ = ping_interval.tick() => {
                    // Send ping to keep connection alive
                    let _ = write.send(Message::Ping(vec![])).await;
                }
            }
        }

        let _ = ping_write; // suppress unused warning
        eprintln!("[PM] Disconnected, reconnecting in {}ms", backoff_ms);
        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(10_000);
    }
}

fn parse_clob_message(
    text: &str,
    recv_at: Instant,
    up_token_id: &str,
    down_token_id: &str,
) -> Option<PolymarketQuote> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    // Handle array of events
    let events = if v.is_array() {
        v.as_array()?.clone()
    } else if v.is_object() {
        vec![v]
    } else {
        return None;
    };

    let mut up_bid: Option<f64> = None;
    let mut up_ask: Option<f64> = None;
    let mut down_bid: Option<f64> = None;
    let mut down_ask: Option<f64> = None;
    let mut ts_ms: i64 = chrono::Utc::now().timestamp_millis();
    let mut found_data = false;

    for event in &events {
        let event_type = event.get("event_type").and_then(|e| e.as_str()).unwrap_or("");

        // Handle best_bid_ask and price_change events
        if event_type == "best_bid_ask" || event_type == "price_change" || event_type == "book" {
            let asset_id = event.get("asset_id").and_then(|a| a.as_str()).unwrap_or("");

            let bid = event.get("best_bid").or_else(|| event.get("price"))
                .and_then(|v| v.as_str().or_else(|| v.as_f64().map(|_| "")).and_then(|s| {
                    if s.is_empty() { v.as_f64() } else { s.parse().ok() }
                }));
            let ask = event.get("best_ask")
                .and_then(|v| v.as_str().or_else(|| v.as_f64().map(|_| "")).and_then(|s| {
                    if s.is_empty() { v.as_f64() } else { s.parse().ok() }
                }));

            if let Some(t) = event.get("timestamp").and_then(|t| t.as_i64()) {
                ts_ms = t;
            }

            if asset_id == up_token_id {
                up_bid = bid.or(up_bid);
                up_ask = ask.or(up_ask);
                found_data = true;
            } else if asset_id == down_token_id {
                down_bid = bid.or(down_bid);
                down_ask = ask.or(down_ask);
                found_data = true;
            }
        }
    }

    if !found_data {
        return None;
    }

    Some(PolymarketQuote {
        server_ts_ms: ts_ms,
        recv_at,
        up_bid,
        up_ask,
        down_bid,
        down_ask,
    })
}
