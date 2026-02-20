use std::str::FromStr;
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
    let market_ctx = match market_ctx_rx.await {
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

    // ── Initialize CLOB client for live execution ──
    use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
    use polymarket_client_sdk::clob::types::{
        Side as ClobSide, OrderType as ClobOrderType, SignatureType, OrderStatusType,
    };
    use polymarket_client_sdk::types::{Decimal, U256};
    use polymarket_client_sdk::auth::{LocalSigner, Signer};
    use polymarket_client_sdk::POLYGON;

    // Compute tick_size decimal places for price rounding
    let tick_decimals: usize = if market_ctx.tick_size >= 0.1 {
        1
    } else if market_ctx.tick_size >= 0.01 {
        2
    } else if market_ctx.tick_size >= 0.001 {
        3
    } else {
        4
    };

    // We store the authenticated client + signer as Option so dry_run compiles without credentials.
    let clob = if !config.dry_run {
        let pk = config
            .polymarket_private_key
            .as_ref()
            .expect("[GW] POLYMARKET_PRIVATE_KEY required when DRY_RUN=false");
        let signer = LocalSigner::from_str(pk)
            .expect("[GW] Invalid POLYMARKET_PRIVATE_KEY")
            .with_chain_id(Some(POLYGON));

        let sig_type = match config.polymarket_signature_type {
            1 => SignatureType::Proxy,
            2 => SignatureType::GnosisSafe,
            _ => SignatureType::Eoa,
        };

        let mut auth_builder = ClobClient::new("https://clob.polymarket.com", ClobConfig::default())
            .expect("[GW] Failed to create CLOB client")
            .authentication_builder(&signer)
            .signature_type(sig_type);

        if let Some(ref funder) = config.polymarket_funder_address {
            auth_builder = auth_builder.funder(
                funder.parse().expect("[GW] Invalid POLYMARKET_FUNDER_ADDRESS"),
            );
        }

        let client = auth_builder
            .authenticate()
            .await
            .expect("[GW] CLOB authentication failed");

        eprintln!("[GW] CLOB client authenticated, address={}", client.address());
        Some((client, signer))
    } else {
        None
    };

    let _ = &market_ctx; // used in live path

    // ── Order processing loop ──
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
            // ── Live CLOB execution ──
            let (ref client, ref signer) = *clob.as_ref().unwrap();

            eprintln!(
                "[GW] LIVE #{}: {:?} {:?} @ {:.tick$} x ${:.2} [{}] post_only={} token={:.8}..",
                order.id, order.order_type, order.side, order.price, order.size,
                order.strategy, order.post_only,
                &order.token_id[..8.min(order.token_id.len())],
                tick = tick_decimals,
            );

            // Convert price to Decimal with tick_size precision
            let price_str = format!("{:.prec$}", order.price, prec = tick_decimals);
            let price_dec = match Decimal::from_str(&price_str) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("[GW] Invalid price '{}': {}", price_str, e);
                    send_rejected_ack(&feed_tx, &telem_tx, &order, submit_at, format!("bad price: {}", e)).await;
                    continue;
                }
            };

            // Convert size: our size is USDC notional, SDK expects shares (outcome tokens)
            // For BUY: you spend (shares * price) USDC to get (shares) tokens
            // So shares = usdc_notional / price
            let shares = order.size / order.price;
            let size_str = format!("{:.2}", shares);
            let size_dec = match Decimal::from_str(&size_str) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("[GW] Invalid size '{}': {}", size_str, e);
                    send_rejected_ack(&feed_tx, &telem_tx, &order, submit_at, format!("bad size: {}", e)).await;
                    continue;
                }
            };

            // Parse token_id to U256
            let token_id = match U256::from_str(&order.token_id) {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("[GW] Invalid token_id '{}': {}", order.token_id, e);
                    send_rejected_ack(&feed_tx, &telem_tx, &order, submit_at, format!("bad token_id: {}", e)).await;
                    continue;
                }
            };

            // Map our order type → SDK order type
            let clob_order_type = match order.order_type {
                OrderType::GTC => ClobOrderType::GTC,
                OrderType::FOK => ClobOrderType::FOK,
            };

            // Record raw request for replay
            let request_json = serde_json::json!({
                "order_id": order.id,
                "token_id": order.token_id,
                "side": format!("{:?}", order.side),
                "price": price_str,
                "size_shares": size_str,
                "size_usdc": order.size,
                "order_type": format!("{:?}", order.order_type),
                "post_only": order.post_only,
                "strategy": order.strategy,
            })
            .to_string();

            let _ = telem_tx.try_send(TelemetryEvent::RawClobResponse(RawClobRecord {
                ts_ms: chrono::Utc::now().timestamp_millis(),
                order_id: order.id,
                direction: "submit",
                raw_json: request_json,
            }));

            // Build → Sign → Post
            let result: Result<_, String> = async {
                let signable = client
                    .limit_order()
                    .token_id(token_id)
                    .price(price_dec)
                    .size(size_dec)
                    .side(ClobSide::Buy)
                    .order_type(clob_order_type)
                    .post_only(order.post_only)
                    .build()
                    .await
                    .map_err(|e| format!("build: {}", e))?;

                let signed = client
                    .sign(signer, signable)
                    .await
                    .map_err(|e| format!("sign: {}", e))?;

                let resp = client
                    .post_order(signed)
                    .await
                    .map_err(|e| format!("post: {}", e))?;

                Ok(resp)
            }
            .await;

            match result {
                Ok(resp) => {
                    // PostOrderResponse doesn't impl Serialize, build JSON manually
                    let raw = serde_json::json!({
                        "success": resp.success,
                        "order_id": resp.order_id,
                        "status": format!("{:?}", resp.status),
                        "error_msg": resp.error_msg,
                        "making_amount": resp.making_amount.to_string(),
                        "taking_amount": resp.taking_amount.to_string(),
                        "trade_ids": resp.trade_ids,
                    }).to_string();

                    let _ = telem_tx.try_send(TelemetryEvent::RawClobResponse(RawClobRecord {
                        ts_ms: chrono::Utc::now().timestamp_millis(),
                        order_id: order.id,
                        direction: "response",
                        raw_json: raw.clone(),
                    }));

                    let latency = submit_at.elapsed().as_secs_f64() * 1000.0;

                    // Map CLOB status → our OrderStatus
                    let status = if resp.success {
                        match resp.status {
                            OrderStatusType::Matched => {
                                eprintln!(
                                    "[GW] #{} MATCHED lat={:.1}ms clob_id={}",
                                    order.id, latency, resp.order_id
                                );
                                OrderStatus::Filled
                            }
                            OrderStatusType::Live => {
                                eprintln!(
                                    "[GW] #{} LIVE (resting) lat={:.1}ms clob_id={}",
                                    order.id, latency, resp.order_id
                                );
                                OrderStatus::Live
                            }
                            OrderStatusType::Delayed => {
                                eprintln!(
                                    "[GW] #{} DELAYED lat={:.1}ms clob_id={}",
                                    order.id, latency, resp.order_id
                                );
                                OrderStatus::Live
                            }
                            OrderStatusType::Unmatched => {
                                eprintln!(
                                    "[GW] #{} UNMATCHED (post_only crossed) lat={:.1}ms",
                                    order.id, latency
                                );
                                OrderStatus::Unmatched
                            }
                            _ => {
                                eprintln!(
                                    "[GW] #{} status={:?} lat={:.1}ms",
                                    order.id, resp.status, latency
                                );
                                OrderStatus::Live
                            }
                        }
                    } else {
                        let msg = resp.error_msg.unwrap_or_else(|| "unknown error".to_string());
                        eprintln!("[GW] #{} REJECTED: {} lat={:.1}ms", order.id, msg, latency);
                        OrderStatus::Rejected(msg)
                    };

                    // For Matched orders, filled_size is in USDC (our convention)
                    let filled_size = match &status {
                        OrderStatus::Filled => Some(order.size),
                        _ => None,
                    };

                    OrderAck {
                        order_id: order.id,
                        status,
                        filled_price: Some(order.price),
                        filled_size,
                        latency_ms: latency,
                        clob_order_id: Some(resp.order_id),
                        raw_response: Some(raw),
                    }
                }
                Err(e) => {
                    let latency = submit_at.elapsed().as_secs_f64() * 1000.0;
                    eprintln!("[GW] #{} ERROR: {} lat={:.1}ms", order.id, e, latency);

                    let _ = telem_tx.try_send(TelemetryEvent::RawClobResponse(RawClobRecord {
                        ts_ms: chrono::Utc::now().timestamp_millis(),
                        order_id: order.id,
                        direction: "error",
                        raw_json: e.clone(),
                    }));

                    OrderAck {
                        order_id: order.id,
                        status: OrderStatus::Rejected(e),
                        filled_price: None,
                        filled_size: None,
                        latency_ms: latency,
                        clob_order_id: None,
                        raw_response: None,
                    }
                }
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

/// Helper: send a Rejected ack back for validation errors before reaching the CLOB.
async fn send_rejected_ack(
    feed_tx: &mpsc::Sender<FeedEvent>,
    telem_tx: &mpsc::Sender<TelemetryEvent>,
    order: &Order,
    submit_at: Instant,
    reason: String,
) {
    eprintln!("[GW] #{} REJECTED (local): {}", order.id, reason);

    let _ = telem_tx.try_send(TelemetryEvent::RawClobResponse(RawClobRecord {
        ts_ms: chrono::Utc::now().timestamp_millis(),
        order_id: order.id,
        direction: "error",
        raw_json: reason.clone(),
    }));

    let ack = OrderAck {
        order_id: order.id,
        status: OrderStatus::Rejected(reason),
        filled_price: None,
        filled_size: None,
        latency_ms: submit_at.elapsed().as_secs_f64() * 1000.0,
        clob_order_id: None,
        raw_response: None,
    };

    let _ = feed_tx.send(FeedEvent::OrderAck(ack)).await;
}
