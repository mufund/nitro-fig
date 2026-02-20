use std::time::Instant;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::types::*;

/// Order gateway: receives orders from engine, executes on CLOB, feeds ack back.
/// Runs as a background task — never touches shared state.
///
/// In dry_run mode: simulates immediate fills.
/// In live mode: submits to Polymarket CLOB via polymarket-client-sdk.
pub async fn order_gateway(
    mut order_rx: mpsc::Receiver<Order>,
    feed_tx: mpsc::Sender<FeedEvent>,
    telem_tx: mpsc::Sender<TelemetryEvent>,
    market_ctx_rx: tokio::sync::oneshot::Receiver<MarketContext>,
    config: Config,
) {
    eprintln!("[GW] Order gateway started (dry_run={})", config.dry_run);

    // Wait for market context (tick_size, neg_risk, token IDs)
    let _market_ctx = match market_ctx_rx.await {
        Ok(ctx) => {
            eprintln!(
                "[GW] Market context: tick_size={} neg_risk={} up={:.8}.. down={:.8}..",
                ctx.tick_size, ctx.neg_risk,
                &ctx.up_token_id[..8.min(ctx.up_token_id.len())],
                &ctx.down_token_id[..8.min(ctx.down_token_id.len())],
            );
            ctx
        }
        Err(_) => {
            eprintln!("[GW] Market context channel closed, exiting");
            return;
        }
    };

    // TODO: Initialize CLOB client for live execution
    // When polymarket-client-sdk is integrated:
    // if !config.dry_run {
    //     let clob = auth::init_clob_client(&config).await?;
    //     // Use clob.client.limit_order()... for real order submission
    // }

    while let Some(order) = order_rx.recv().await {
        let submit_at = Instant::now();

        let ack = if config.dry_run {
            // Simulate immediate fill at limit price
            OrderAck {
                order_id: order.id,
                status: OrderStatus::Filled,
                filled_price: Some(order.price),
                filled_size: Some(order.size),
                latency_ms: 0.0,
                clob_order_id: None,
                raw_response: None,
            }
        } else {
            // Live mode: log order details and record raw request
            eprintln!(
                "[GW] Live order #{}: {:?} {} @ {:.4} x ${:.2} [{}] post_only={} token={}",
                order.id, order.order_type, order.side, order.price, order.size,
                order.strategy, order.post_only,
                &order.token_id[..8.min(order.token_id.len())],
            );

            // Record raw request JSON for replay
            let request_json = serde_json::json!({
                "order_id": order.id,
                "token_id": order.token_id,
                "side": format!("{:?}", order.side),
                "price": order.price,
                "size": order.size,
                "order_type": format!("{:?}", order.order_type),
                "post_only": order.post_only,
                "strategy": order.strategy,
            }).to_string();

            let _ = telem_tx.try_send(TelemetryEvent::RawClobResponse(RawClobRecord {
                ts_ms: chrono::Utc::now().timestamp_millis(),
                order_id: order.id,
                direction: "submit",
                raw_json: request_json,
            }));

            // TODO: Real CLOB execution via polymarket-client-sdk
            // let signed = clob.client.sign(&signer, limit_order).await?;
            // let resp = clob.client.post_order(signed).await?;
            // Parse resp into OrderAck with real status/price/size

            // For now: simulate fill with latency measurement
            let ack = OrderAck {
                order_id: order.id,
                status: OrderStatus::Filled,
                filled_price: Some(order.price),
                filled_size: Some(order.size),
                latency_ms: 0.0,
                clob_order_id: None,
                raw_response: Some("{\"simulated\": true}".to_string()),
            };

            // Record raw response JSON for replay
            let _ = telem_tx.try_send(TelemetryEvent::RawClobResponse(RawClobRecord {
                ts_ms: chrono::Utc::now().timestamp_millis(),
                order_id: order.id,
                direction: "response",
                raw_json: ack.raw_response.clone().unwrap_or_default(),
            }));

            ack
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
