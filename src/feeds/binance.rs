use std::time::Instant;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::types::{BinanceTrade, FeedEvent};

/// Pure producer: connects to Binance WS, parses trades, sends FeedEvents.
/// Owns no shared state — only holds a channel sender.
pub async fn binance_feed(
    feed_tx: mpsc::Sender<FeedEvent>,
    ws_url: String,
    ws_fallback: String,
) {
    let mut backoff_ms: u64 = 1000;

    loop {
        let url = &ws_url;
        eprintln!("[BINANCE] Connecting to {}", url);

        let connect_result = connect_async(url).await;
        let ws = match connect_result {
            Ok((ws, _)) => {
                eprintln!("[BINANCE] Connected");
                backoff_ms = 1000;
                ws
            }
            Err(e) => {
                eprintln!("[BINANCE] Primary failed: {}, trying fallback", e);
                match connect_async(&ws_fallback).await {
                    Ok((ws, _)) => {
                        eprintln!("[BINANCE] Connected via fallback");
                        backoff_ms = 1000;
                        ws
                    }
                    Err(e2) => {
                        eprintln!("[BINANCE] Fallback failed: {}, retrying in {}ms", e2, backoff_ms);
                        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                        backoff_ms = (backoff_ms * 2).min(10_000);
                        continue;
                    }
                }
            }
        };

        let (mut _write, mut read) = ws.split();

        while let Some(msg) = read.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("[BINANCE] WS error: {}, reconnecting", e);
                    break;
                }
            };

            if let Message::Text(text) = msg {
                let recv_at = Instant::now();
                if let Some(trade) = parse_trade(&text, recv_at) {
                    if feed_tx.send(FeedEvent::BinanceTrade(trade)).await.is_err() {
                        eprintln!("[BINANCE] Channel closed, exiting");
                        return;
                    }
                }
            }
        }

        eprintln!("[BINANCE] Disconnected, reconnecting in {}ms", backoff_ms);
        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(10_000);
    }
}

fn parse_trade(text: &str, recv_at: Instant) -> Option<BinanceTrade> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let price: f64 = v["p"].as_str()?.parse().ok()?;
    let qty: f64 = v["q"].as_str()?.parse().ok()?;
    let ts_ms = v["T"].as_i64()?;
    let is_buy = !v["m"].as_bool()?; // m=true means seller is maker, so buyer is taker

    Some(BinanceTrade {
        exchange_ts_ms: ts_ms,
        recv_at,
        price,
        qty,
        is_buy,
    })
}
