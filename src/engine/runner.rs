use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::engine::risk::RiskManager;
use crate::engine::state::{MarketState, StrategyStats};
use crate::strategies::{evaluate_all, select_best};
use crate::strategies::distance_fade::DistanceFade;
use crate::strategies::momentum::Momentum;
use crate::strategies::settlement_sniper::SettlementSniper;
use crate::types::*;

/// Core engine event loop. Single task, owns all state.
/// No shared mutable state — race conditions impossible by construction.
pub async fn run_engine(
    market: MarketInfo,
    mut feed_rx: mpsc::Receiver<FeedEvent>,
    order_tx: mpsc::Sender<Order>,
    telem_tx: mpsc::Sender<TelemetryEvent>,
    config: &Config,
) {
    let mut state = MarketState::new(market);
    let mut risk = RiskManager::new(config);
    let strategies: Vec<Box<dyn crate::strategies::Strategy>> = vec![
        Box::new(DistanceFade),
        Box::new(Momentum),
        Box::new(SettlementSniper),
    ];
    let mut signals_buf: Vec<Signal> = Vec::with_capacity(8);
    let mut next_order_id: u64 = 1;

    // Map order_id → strategy name (for attributing fills to strategies)
    let mut order_strategies: HashMap<u64, &'static str> = HashMap::new();

    // Log market start
    let _ = telem_tx.try_send(TelemetryEvent::MarketStart(MarketStartRecord {
        ts_ms: chrono::Utc::now().timestamp_millis(),
        slug: state.info.slug.clone(),
        strike: state.info.strike,
        start_ms: state.info.start_ms,
        end_ms: state.info.end_ms,
    }));

    eprintln!(
        "[ENGINE] Running market {} | strike=${:.0} | window={}s",
        state.info.slug,
        state.info.strike,
        (state.info.end_ms - state.info.start_ms) / 1000
    );

    while let Some(event) = feed_rx.recv().await {
        let now_ms = chrono::Utc::now().timestamp_millis();

        match event {
            FeedEvent::BinanceTrade(t) => {
                let recv_latency_us = t.recv_at.elapsed().as_micros() as u64;
                state.on_binance_trade(t);
                let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                    ts_ms: now_ms,
                    event: "binance_recv",
                    latency_us: recv_latency_us,
                }));
            }

            FeedEvent::PolymarketQuote(q) => {
                let recv_at = q.recv_at;
                let pm_recv_us = recv_at.elapsed().as_micros() as u64;
                state.on_polymarket_quote(q);

                let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                    ts_ms: now_ms,
                    event: "pm_recv",
                    latency_us: pm_recv_us,
                }));

                // Only evaluate if we have both feeds
                if !state.has_data() {
                    continue;
                }

                // ── Strategy evaluation (hot path) ──
                let eval_start = Instant::now();
                evaluate_all(&strategies, &state, now_ms, &mut signals_buf);
                let eval_us = eval_start.elapsed().as_micros() as u64;

                let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                    ts_ms: now_ms,
                    event: "eval",
                    latency_us: eval_us,
                }));

                // Skip if no signals fired
                if signals_buf.is_empty() {
                    continue;
                }

                let best = select_best(&signals_buf);
                let best_strategy = best.map(|b| b.strategy);
                let time_left_s = state.time_left_s(now_ms);
                let distance = state.distance();

                // Log ALL signals (not just the winner) with selected flag
                for sig in &signals_buf {
                    let is_selected = best_strategy == Some(sig.strategy);
                    let _ = telem_tx.try_send(TelemetryEvent::Signal(SignalRecord {
                        ts_ms: now_ms,
                        strategy: sig.strategy.to_string(),
                        side: sig.side,
                        edge: sig.edge,
                        fair_value: sig.fair_value,
                        market_price: sig.market_price,
                        confidence: sig.confidence,
                        size_frac: sig.size_frac,
                        binance_price: state.binance_price,
                        distance,
                        time_left_s,
                        eval_latency_us: eval_us,
                        selected: is_selected,
                    }));
                }

                if let Some(best) = best {
                    state.total_signals += 1;

                    // Per-strategy signal counter
                    state.strategy_stats
                        .entry(best.strategy)
                        .or_insert_with(StrategyStats::new)
                        .signals += 1;

                    // ── Risk check ──
                    if let Some(order) = risk.check(best, &state, next_order_id, now_ms) {
                        state.total_orders += 1;

                        // Per-strategy order counter + edge accumulator
                        let strat_stats = state.strategy_stats
                            .entry(best.strategy)
                            .or_insert_with(StrategyStats::new);
                        strat_stats.orders += 1;
                        strat_stats.total_edge += best.edge;

                        // Track order_id → strategy for fill attribution
                        order_strategies.insert(order.id, best.strategy);

                        // Record order attempt
                        let _ = telem_tx.try_send(TelemetryEvent::OrderSent(OrderRecord {
                            ts_ms: now_ms,
                            order_id: order.id,
                            side: order.side,
                            price: order.price,
                            size: order.size,
                            strategy: order.strategy.to_string(),
                            edge_at_submit: best.edge,
                            binance_price: state.binance_price,
                            time_left_s,
                        }));

                        // Dispatch order (non-blocking)
                        let _ = order_tx.try_send(order);
                        next_order_id += 1;

                        state.position.on_order_sent();
                        risk.on_order_sent(now_ms);
                    }

                    // End-to-end latency
                    let e2e_us = recv_at.elapsed().as_micros() as u64;
                    let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                        ts_ms: now_ms,
                        event: "e2e",
                        latency_us: e2e_us,
                    }));

                    eprintln!(
                        "[SIG] {} {:?} edge={:.3} fair={:.3} mkt={:.3} dist={:.0} t-{:.0}s e2e={}μs",
                        best.strategy, best.side, best.edge, best.fair_value,
                        best.market_price, distance, time_left_s, e2e_us
                    );
                }
            }

            FeedEvent::OrderAck(ack) => {
                // Look up which strategy triggered this order
                let strategy = order_strategies.remove(&ack.order_id)
                    .unwrap_or("unknown")
                    .to_string();

                // Record fill result
                let pnl_if_correct = ack
                    .filled_price
                    .zip(ack.filled_size)
                    .map(|(p, s)| (1.0 - p) * s);

                let _ = telem_tx.try_send(TelemetryEvent::OrderResult(FillRecord {
                    ts_ms: now_ms,
                    order_id: ack.order_id,
                    strategy: strategy.clone(),
                    status: format!("{:?}", ack.status),
                    filled_price: ack.filled_price,
                    filled_size: ack.filled_size,
                    submit_to_ack_ms: ack.latency_ms,
                    pnl_if_correct,
                }));

                match ack.status {
                    OrderStatus::Filled | OrderStatus::PartialFill => {
                        state.total_filled += 1;
                        if let Some(pnl) = pnl_if_correct {
                            state.gross_pnl += pnl;
                        }

                        // Per-strategy fill counter + PnL
                        let strat_name = strategy.as_str();
                        for (&key, stats) in state.strategy_stats.iter_mut() {
                            if key == strat_name {
                                stats.filled += 1;
                                if let Some(pnl) = pnl_if_correct {
                                    stats.gross_pnl += pnl;
                                }
                                break;
                            }
                        }

                        eprintln!(
                            "[FILL] Order #{} [{}] {:?} price={:?} size={:?} latency={:.1}ms",
                            ack.order_id, strategy, ack.status, ack.filled_price, ack.filled_size, ack.latency_ms
                        );
                    }
                    _ => {
                        eprintln!("[FILL] Order #{} [{}] {:?}", ack.order_id, strategy, ack.status);
                    }
                }

                state.position.on_fill(&ack);
                risk.on_fill(&ack);
            }

            FeedEvent::Tick => {
                if state.is_stale(now_ms) {
                    eprintln!(
                        "[WARN] Stale: bn_age={}ms pm_age={}ms",
                        if state.binance_ts > 0 { now_ms - state.binance_ts } else { -1 },
                        if state.pm_last_ts > 0 { now_ms - state.pm_last_ts } else { -1 },
                    );
                }
            }
        }

        // Check market end
        if now_ms >= state.info.end_ms + 10_000 {
            break;
        }
    }

    // Determine outcome
    let final_distance = state.distance();
    let outcome = if final_distance >= 0.0 { Side::Up } else { Side::Down };

    // Build per-strategy summary
    let per_strategy: Vec<PerStrategyEnd> = state.strategy_stats.iter()
        .map(|(&name, stats)| PerStrategyEnd {
            strategy: name.to_string(),
            signals: stats.signals,
            orders: stats.orders,
            filled: stats.filled,
            gross_pnl: stats.gross_pnl,
            avg_edge: stats.avg_edge(),
        })
        .collect();

    let _ = telem_tx.try_send(TelemetryEvent::MarketEnd(MarketEndRecord {
        ts_ms: chrono::Utc::now().timestamp_millis(),
        slug: state.info.slug.clone(),
        final_binance_price: state.binance_price,
        final_distance,
        outcome,
        total_signals: state.total_signals,
        total_orders: state.total_orders,
        total_filled: state.total_filled,
        gross_pnl: state.gross_pnl,
        per_strategy,
    }));

    eprintln!(
        "[ENGINE] Market {} ended | outcome={:?} | signals={} orders={} filled={} pnl=${:.2}",
        state.info.slug, outcome, state.total_signals, state.total_orders,
        state.total_filled, state.gross_pnl,
    );
    for (&name, stats) in &state.strategy_stats {
        eprintln!(
            "[ENGINE]   {}: sig={} ord={} fill={} pnl=${:.2} avg_edge={:.3}",
            name, stats.signals, stats.orders, stats.filled, stats.gross_pnl, stats.avg_edge()
        );
    }
}
