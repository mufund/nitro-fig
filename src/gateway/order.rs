use std::time::Instant;
use tokio::sync::mpsc;

use crate::types::*;

/// Order gateway: receives orders from engine, executes, feeds ack back.
/// Runs as a background task — never touches shared state.
pub async fn order_gateway(
    mut order_rx: mpsc::Receiver<Order>,
    feed_tx: mpsc::Sender<FeedEvent>,
    telem_tx: mpsc::Sender<TelemetryEvent>,
    dry_run: bool,
) {
    let _client = reqwest::Client::new();

    eprintln!("[GW] Order gateway started (dry_run={})", dry_run);

    while let Some(order) = order_rx.recv().await {
        let submit_at = Instant::now();

        let ack = if dry_run {
            // Simulate immediate fill at limit price
            OrderAck {
                order_id: order.id,
                status: OrderStatus::Filled,
                filled_price: Some(order.price),
                filled_size: Some(order.size),
                latency_ms: 0.0,
            }
        } else {
            // TODO: Real CLOB execution via POST /order
            // For now, still simulate
            eprintln!("[GW] Real execution not implemented, simulating fill");
            OrderAck {
                order_id: order.id,
                status: OrderStatus::Filled,
                filled_price: Some(order.price),
                filled_size: Some(order.size),
                latency_ms: 0.0,
            }
        };

        let exec_us = submit_at.elapsed().as_micros() as u64;

        // Record execution latency
        let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
            ts_ms: chrono::Utc::now().timestamp_millis(),
            event: "order_exec",
            latency_us: exec_us,
        }));

        // Feed ack back to engine via the feed channel
        let final_ack = OrderAck {
            latency_ms: submit_at.elapsed().as_secs_f64() * 1000.0,
            ..ack
        };

        if feed_tx.send(FeedEvent::OrderAck(final_ack)).await.is_err() {
            eprintln!("[GW] Feed channel closed, exiting");
            return;
        }
    }

    eprintln!("[GW] Order gateway stopped");
}
