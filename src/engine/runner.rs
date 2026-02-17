use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::engine::risk::StrategyRiskManager;
use crate::engine::state::{BinanceState, MarketState, StrategyStats};
use crate::math::oracle::OracleBasis;
use crate::math::pricing::z_score;
use crate::math::regime::Regime;
use crate::strategies::evaluate_filtered;
use crate::strategies::latency_arb::LatencyArb;
use crate::strategies::certainty_capture::CertaintyCapture;
use crate::strategies::convexity_fade::ConvexityFade;
use crate::strategies::strike_misalign::StrikeMisalign;
use crate::strategies::lp_extreme::LpExtreme;
use crate::types::*;

/// Core engine event loop. Single task, owns all state.
///
/// Accepts BinanceState (persistent across markets) and returns it at market end.
///
/// Side coherence: first dispatched order sets `house_side`. All subsequent
/// ACTIVE orders must agree. Passive signals (lp_extreme) are exempt —
/// they intentionally take the opposite side for LP purposes.
///
/// PnL: fills are recorded, settled at market end when outcome is known.
pub async fn run_engine(
    market: MarketInfo,
    binance_state: BinanceState,
    mut feed_rx: mpsc::Receiver<FeedEvent>,
    order_tx: mpsc::Sender<Order>,
    telem_tx: mpsc::Sender<TelemetryEvent>,
    config: &Config,
) -> BinanceState {
    let oracle = OracleBasis::new(config.oracle_beta, config.oracle_delta_s);
    let mut state = MarketState::new(market, binance_state, oracle);
    let mut risk = StrategyRiskManager::new(config);

    // ── Instantiate strategies (only those enabled in config) ──
    let latency_arb = LatencyArb;
    let certainty_capture = CertaintyCapture;
    let convexity_fade = ConvexityFade;
    let strike_misalign = StrikeMisalign;
    let lp_extreme = LpExtreme;

    // ── Partition strategies by trigger type, respecting config toggles ──
    let mut binance_strategies: Vec<&dyn crate::strategies::Strategy> = Vec::with_capacity(3);
    if config.strategy_latency_arb { binance_strategies.push(&latency_arb); }
    if config.strategy_lp_extreme  { binance_strategies.push(&lp_extreme); }

    let mut pm_strategies: Vec<&dyn crate::strategies::Strategy> = Vec::with_capacity(4);
    if config.strategy_certainty_capture { pm_strategies.push(&certainty_capture); }
    if config.strategy_convexity_fade    { pm_strategies.push(&convexity_fade); }
    if config.strategy_lp_extreme        { pm_strategies.push(&lp_extreme); }

    let mut open_strategies: Vec<&dyn crate::strategies::Strategy> = Vec::with_capacity(2);
    if config.strategy_strike_misalign { open_strategies.push(&strike_misalign); }

    {
        let enabled: Vec<&str> = [
            (config.strategy_latency_arb, "latency_arb"),
            (config.strategy_certainty_capture, "certainty_capture"),
            (config.strategy_convexity_fade, "convexity_fade"),
            (config.strategy_strike_misalign, "strike_misalign"),
            (config.strategy_lp_extreme, "lp_extreme"),
            (config.strategy_cross_timeframe, "cross_timeframe"),
        ].iter().filter(|(on, _)| *on).map(|(_, name)| *name).collect();
        eprintln!("[ENGINE] Strategies enabled: {:?}", enabled);
    }

    let mut signals_buf: Vec<Signal> = Vec::with_capacity(8);
    let mut open_buf: Vec<Signal> = Vec::with_capacity(2);
    let mut next_order_id: u64 = 1;

    // Map order_id → (strategy_name, side) for fill attribution + settlement
    let mut order_strategies: HashMap<u64, (&'static str, Side)> = HashMap::new();

    // Fill tracking for settlement PnL
    let mut fills: Vec<Fill> = Vec::with_capacity(64);

    // Side coherence: first ACTIVE order sets the house view for this market
    // Passive signals (lp_extreme) are exempt from house_side filtering
    let mut house_side: Option<Side> = None;

    // Diagnostic: periodic strategy health log (every 10s)
    let mut last_diag_ms: i64 = 0;

    // Log market start
    let _ = telem_tx.try_send(TelemetryEvent::MarketStart(MarketStartRecord {
        ts_ms: chrono::Utc::now().timestamp_millis(),
        slug: state.info.slug.clone(),
        strike: state.info.strike,
        start_ms: state.info.start_ms,
        end_ms: state.info.end_ms,
    }));

    let mut warmup_done = false;

    eprintln!(
        "[ENGINE] Running market {} | strike=${:.0} | window={}s | bankroll=${:.0} | ewma_n={}",
        state.info.slug,
        state.info.strike,
        (state.info.end_ms - state.info.start_ms) / 1000,
        config.bankroll,
        state.bn.ewma_vol.n_samples(),
    );

    while let Some(event) = feed_rx.recv().await {
        let now_ms = chrono::Utc::now().timestamp_millis();

        match event {
            FeedEvent::BinanceTrade(t) => {
                let recv_at = t.recv_at;
                let recv_latency_us = recv_at.elapsed().as_micros() as u64;
                state.on_binance_trade(t);

                let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                    ts_ms: now_ms,
                    event: "binance_recv",
                    latency_us: recv_latency_us,
                }));

                if !state.has_data() {
                    continue;
                }

                if !state.bn.ewma_vol.is_valid() {
                    continue;
                }
                if !warmup_done {
                    warmup_done = true;
                    eprintln!(
                        "[ENGINE] Warmup complete: σ_real={:.8} σ_raw={:.8} samples={} | trading enabled",
                        state.sigma_real(),
                        state.bn.ewma_vol.sigma(),
                        state.bn.ewma_vol.n_samples(),
                    );
                }

                // ── Periodic diagnostic log (every 10s) ──
                if now_ms - last_diag_ms >= 10_000 {
                    last_diag_ms = now_ms;
                    log_strategy_diagnostics(&state, now_ms, &house_side);
                }

                // ── Evaluate Binance-triggered strategies ──
                let eval_start = Instant::now();
                evaluate_filtered(&binance_strategies, &state, now_ms, &mut signals_buf);

                let elapsed_ms = now_ms - state.info.start_ms;
                if elapsed_ms >= 0 && elapsed_ms <= 15_000 {
                    evaluate_filtered(&open_strategies, &state, now_ms, &mut open_buf);
                    signals_buf.extend(open_buf.drain(..));
                }

                let eval_us = eval_start.elapsed().as_micros() as u64;
                let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                    ts_ms: now_ms,
                    event: "eval_binance",
                    latency_us: eval_us,
                }));

                process_all_signals(
                    &mut signals_buf, &mut state, &mut risk, &order_tx, &telem_tx,
                    &mut next_order_id, &mut order_strategies, &mut house_side,
                    now_ms, eval_us, recv_at,
                );
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

                if !state.has_data() || !warmup_done {
                    continue;
                }

                let eval_start = Instant::now();
                evaluate_filtered(&pm_strategies, &state, now_ms, &mut signals_buf);

                let elapsed_ms = now_ms - state.info.start_ms;
                if elapsed_ms >= 0 && elapsed_ms <= 15_000 {
                    evaluate_filtered(&open_strategies, &state, now_ms, &mut open_buf);
                    signals_buf.extend(open_buf.drain(..));
                }

                let eval_us = eval_start.elapsed().as_micros() as u64;
                let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                    ts_ms: now_ms,
                    event: "eval_pm",
                    latency_us: eval_us,
                }));

                process_all_signals(
                    &mut signals_buf, &mut state, &mut risk, &order_tx, &telem_tx,
                    &mut next_order_id, &mut order_strategies, &mut house_side,
                    now_ms, eval_us, recv_at,
                );
            }

            FeedEvent::PolymarketBook(book) => {
                let recv_at = book.recv_at;
                state.on_book_update(book);

                if !state.has_data() || !warmup_done {
                    continue;
                }

                let eval_start = Instant::now();
                evaluate_filtered(&pm_strategies, &state, now_ms, &mut signals_buf);

                let elapsed_ms = now_ms - state.info.start_ms;
                if elapsed_ms >= 0 && elapsed_ms <= 15_000 {
                    evaluate_filtered(&open_strategies, &state, now_ms, &mut open_buf);
                    signals_buf.extend(open_buf.drain(..));
                }

                let eval_us = eval_start.elapsed().as_micros() as u64;
                let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                    ts_ms: now_ms,
                    event: "eval_book",
                    latency_us: eval_us,
                }));

                process_all_signals(
                    &mut signals_buf, &mut state, &mut risk, &order_tx, &telem_tx,
                    &mut next_order_id, &mut order_strategies, &mut house_side,
                    now_ms, eval_us, recv_at,
                );
            }

            FeedEvent::CrossMarketQuote(cm) => {
                state.on_cross_market_quote(cm);
            }

            FeedEvent::OrderAck(ack) => {
                let (strat_name, order_side) = order_strategies.remove(&ack.order_id)
                    .unwrap_or(("unknown", Side::Up));
                let strategy = strat_name.to_string();

                let pnl_if_correct = ack
                    .filled_price
                    .zip(ack.filled_size)
                    .map(|(p, s)| (1.0 - p) * s);

                let _ = telem_tx.try_send(TelemetryEvent::OrderResult(FillRecord {
                    ts_ms: now_ms,
                    order_id: ack.order_id,
                    strategy: strategy.clone(),
                    side: order_side,
                    status: format!("{:?}", ack.status),
                    filled_price: ack.filled_price,
                    filled_size: ack.filled_size,
                    submit_to_ack_ms: ack.latency_ms,
                    pnl_if_correct,
                }));

                match ack.status {
                    OrderStatus::Filled | OrderStatus::PartialFill => {
                        state.total_filled += 1;

                        if let (Some(price), Some(size)) = (ack.filled_price, ack.filled_size) {
                            fills.push(Fill {
                                order_id: ack.order_id,
                                strategy: strat_name,
                                side: order_side,
                                price,
                                size,
                            });
                        }

                        for (&key, stats) in state.strategy_stats.iter_mut() {
                            if key == strat_name {
                                stats.filled += 1;
                                break;
                            }
                        }

                        risk.on_fill(&strategy, &ack);

                        let side_str = match order_side {
                            Side::Up => "Up",
                            Side::Down => "Down",
                        };
                        eprintln!(
                            "[FILL] #{} [{}] {} {:?} price={:?} size={:?} lat={:.1}ms",
                            ack.order_id, strategy, side_str, ack.status,
                            ack.filled_price, ack.filled_size, ack.latency_ms
                        );
                    }
                    _ => {
                        risk.on_fill(&strategy, &ack);
                        eprintln!("[FILL] #{} [{}] {:?}", ack.order_id, strategy, ack.status);
                    }
                }

                state.position.on_fill(&ack);
            }

            FeedEvent::Tick => {
                if state.is_stale(now_ms) {
                    eprintln!(
                        "[WARN] Stale: bn_age={}ms pm_age={}ms",
                        if state.bn.binance_ts > 0 { now_ms - state.bn.binance_ts } else { -1 },
                        if state.pm_last_ts > 0 { now_ms - state.pm_last_ts } else { -1 },
                    );
                }
            }
        }

        if now_ms >= state.info.end_ms + 10_000 {
            break;
        }
    }

    // ── Settlement ──
    let final_distance = state.distance();
    let outcome = if final_distance >= 0.0 { Side::Up } else { Side::Down };

    let mut realized_pnl = 0.0_f64;
    let mut per_strat_pnl: HashMap<&str, f64> = HashMap::new();
    for fill in &fills {
        let pnl = if fill.side == outcome {
            (1.0 - fill.price) * fill.size
        } else {
            -(fill.price * fill.size)
        };
        realized_pnl += pnl;
        *per_strat_pnl.entry(fill.strategy).or_insert(0.0) += pnl;
    }
    state.gross_pnl = realized_pnl;

    for (&name, stats) in state.strategy_stats.iter_mut() {
        if let Some(&pnl) = per_strat_pnl.get(name) {
            stats.gross_pnl = pnl;
        }
    }

    risk.settle_market(outcome, &fills);

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
        final_binance_price: state.bn.binance_price,
        final_distance,
        outcome,
        total_signals: state.total_signals,
        total_orders: state.total_orders,
        total_filled: state.total_filled,
        gross_pnl: state.gross_pnl,
        per_strategy,
    }));

    eprintln!(
        "[ENGINE] Market {} ended | outcome={:?} | house={:?} | sig={} ord={} fill={} pnl=${:.2} ({}fills settled)",
        state.info.slug, outcome, house_side, state.total_signals, state.total_orders,
        state.total_filled, state.gross_pnl, fills.len(),
    );
    for (&name, stats) in &state.strategy_stats {
        let strat_pnl = per_strat_pnl.get(name).copied().unwrap_or(0.0);
        eprintln!(
            "[ENGINE]   {}: sig={} ord={} fill={} pnl=${:.2} avg_edge={:.3}",
            name, stats.signals, stats.orders, stats.filled, strat_pnl, stats.avg_edge()
        );
    }

    state.take_binance_state()
}

/// Periodic diagnostic: log internal values for each strategy to understand why they fire or don't.
fn log_strategy_diagnostics(state: &MarketState, now_ms: i64, house_side: &Option<Side>) {
    let sigma = state.sigma_real();
    let s = state.s_est();
    let k = state.info.strike;
    let tau = state.tau_eff_s(now_ms);
    let dist = state.distance();
    let dist_frac = state.distance_frac().abs();
    let regime = state.bn.regime.classify();

    let z = if sigma > 0.0 && tau > 0.0 && s > 0.0 && k > 0.0 {
        z_score(s, k, sigma, tau)
    } else {
        0.0
    };

    eprintln!(
        "[DIAG] t_left={:.0}s σ={:.8} z={:.2} dist=${:.0} dist_frac={:.5} regime={:?}({:.0}%/{}) house={:?} \
         up_ask={:.3} down_ask={:.3} S={:.2} K={:.0}",
        tau, sigma, z, dist, dist_frac, regime,
        state.bn.regime.dominant_frac() * 100.0, state.bn.regime.total_ticks(),
        house_side,
        state.up_ask, state.down_ask, s, k,
    );

    // Per-strategy gate analysis
    // certainty_capture: needs |z| >= 1.5, edge >= 0.02
    let z_abs = z.abs();
    let (cc_fair, cc_ask, cc_edge) = if z > 0.0 {
        let fair = crate::math::pricing::p_fair(s, k, sigma, tau);
        (fair, state.up_ask, fair - state.up_ask)
    } else {
        let fair = 1.0 - crate::math::pricing::p_fair(s, k, sigma, tau);
        (fair, state.down_ask, fair - state.down_ask)
    };
    let cc_gate = if z_abs < 1.5 { "z<1.5" }
        else if cc_ask <= 0.0 || cc_ask >= 1.0 { "bad_ask" }
        else if cc_edge < 0.02 { "edge<0.02" }
        else { "PASS" };
    eprintln!(
        "[DIAG]   certainty_capture: z_abs={:.2} fair={:.3} ask={:.3} edge={:.3} → {}",
        z_abs, cc_fair, cc_ask, cc_edge, cc_gate,
    );

    // convexity_fade: needs regime=Range|Ambiguous, dist_frac <= 0.003, edge >= 0.02
    let cf_gate = if regime == Regime::Trend { "regime=Trend" }
        else if dist_frac > 0.003 { "dist>0.3%" }
        else { "PASS(regime+dist)" };
    eprintln!(
        "[DIAG]   convexity_fade: regime={:?} dist_frac={:.5} → {}",
        regime, dist_frac, cf_gate,
    );

    // lp_extreme: needs |z| >= 1.5, losing_ask < 0.25, !Trend
    let (lp_ask, lp_side) = if z > 0.0 {
        (state.down_ask, "Down")
    } else {
        (state.up_ask, "Up")
    };
    let lp_gate = if regime == Regime::Trend { "regime=Trend" }
        else if tau < 60.0 { "tau<60" }
        else if z_abs < 1.5 { "z<1.5" }
        else if lp_ask <= 0.0 || lp_ask >= 0.25 { "ask>=0.25" }
        else { "PASS" };
    eprintln!(
        "[DIAG]   lp_extreme: z_abs={:.2} losing_side={} ask={:.3} regime={:?} → {}",
        z_abs, lp_side, lp_ask, regime, lp_gate,
    );

    // strike_misalign: only first 15s
    let elapsed_ms = now_ms - state.info.start_ms;
    let sm_gate = if elapsed_ms > 15_000 { "past_15s" } else { "in_window" };
    eprintln!(
        "[DIAG]   strike_misalign: elapsed={}ms → {}",
        elapsed_ms, sm_gate,
    );
}

/// Process ALL signals through risk, dispatch every order that passes.
/// Side coherence: active signals filtered by house_side.
/// Passive signals (lp_extreme) are exempt from house_side — they
/// intentionally LP on the losing side.
#[inline]
fn process_all_signals(
    signals: &mut Vec<Signal>,
    state: &mut MarketState,
    risk: &mut StrategyRiskManager,
    order_tx: &mpsc::Sender<Order>,
    telem_tx: &mpsc::Sender<TelemetryEvent>,
    next_order_id: &mut u64,
    order_strategies: &mut HashMap<u64, (&'static str, Side)>,
    house_side: &mut Option<Side>,
    now_ms: i64,
    eval_us: u64,
    recv_at: Instant,
) {
    if signals.is_empty() {
        return;
    }

    // ── Side coherence (active signals only) ──
    // Passive signals (lp_extreme) are exempt — they intentionally take the opposite side
    if let Some(hs) = *house_side {
        signals.retain(|s| s.is_passive || s.side == hs);
        if signals.is_empty() {
            return;
        }
    }
    // If no house view and ACTIVE signals disagree, pick dominant active side
    else {
        let active_signals: Vec<&Signal> = signals.iter().filter(|s| !s.is_passive).collect();
        if active_signals.len() > 1 {
            let (mut up_score, mut down_score) = (0.0_f64, 0.0_f64);
            for s in &active_signals {
                match s.side {
                    Side::Up => up_score += s.edge * s.confidence,
                    Side::Down => down_score += s.edge * s.confidence,
                }
            }
            if up_score > 0.0 && down_score > 0.0 {
                let dominant = if up_score >= down_score { Side::Up } else { Side::Down };
                signals.retain(|s| s.is_passive || s.side == dominant);
            }
        }
    }

    // Sort by edge × confidence descending
    signals.sort_unstable_by(|a, b| {
        let score_a = a.edge * a.confidence;
        let score_b = b.edge * b.confidence;
        score_b.partial_cmp(&score_a).unwrap_or(std::cmp::Ordering::Equal)
    });

    let time_left_s = state.time_left_s(now_ms);
    let distance = state.distance();
    let mut any_dispatched = false;

    // Log ALL signals
    for sig in signals.iter() {
        let _ = telem_tx.try_send(TelemetryEvent::Signal(SignalRecord {
            ts_ms: now_ms,
            strategy: sig.strategy.to_string(),
            side: sig.side,
            edge: sig.edge,
            fair_value: sig.fair_value,
            market_price: sig.market_price,
            confidence: sig.confidence,
            size_frac: sig.size_frac,
            binance_price: state.bn.binance_price,
            distance,
            time_left_s,
            eval_latency_us: eval_us,
            selected: false,
        }));
    }

    // Process each signal through risk
    for sig in signals.iter() {
        state.total_signals += 1;
        state.strategy_stats
            .entry(sig.strategy)
            .or_insert_with(StrategyStats::new)
            .signals += 1;

        if let Some(order) = risk.check_strategy(sig, state, *next_order_id, now_ms) {
            state.total_orders += 1;

            let strat_stats = state.strategy_stats
                .entry(sig.strategy)
                .or_insert_with(StrategyStats::new);
            strat_stats.orders += 1;
            strat_stats.total_edge += sig.edge;

            order_strategies.insert(order.id, (sig.strategy, sig.side));

            // Set house view on first ACTIVE dispatch only
            if house_side.is_none() && !sig.is_passive {
                *house_side = Some(sig.side);
            }

            let _ = telem_tx.try_send(TelemetryEvent::OrderSent(OrderRecord {
                ts_ms: now_ms,
                order_id: order.id,
                side: order.side,
                price: order.price,
                size: order.size,
                strategy: order.strategy.to_string(),
                edge_at_submit: sig.edge,
                binance_price: state.bn.binance_price,
                time_left_s,
            }));

            risk.on_order_sent(sig.strategy, now_ms, order.size);
            state.position.on_order_sent();

            eprintln!(
                "[SIG] {} {:?} edge={:.3} fair={:.3} mkt={:.3} sz=${:.1} {}",
                sig.strategy, sig.side, sig.edge, sig.fair_value,
                sig.market_price, order.size,
                if sig.is_passive { "PASSIVE" } else { "ACTIVE" },
            );

            let _ = order_tx.try_send(order);
            *next_order_id += 1;
            any_dispatched = true;
        }
    }

    if any_dispatched {
        let e2e_us = recv_at.elapsed().as_micros() as u64;
        let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
            ts_ms: now_ms,
            event: "e2e",
            latency_us: e2e_us,
        }));
    }
}
