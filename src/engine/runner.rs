use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::engine::pipeline::{self, ProcessConfig, SignalSink};
use crate::engine::risk::StrategyRiskManager;
use crate::engine::state::{BinanceState, MarketState};
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

// ─── LiveSink ──────────────────────────────────────────────────────────────

/// SignalSink implementation for the live engine.
/// Wraps async channels for order dispatch and telemetry, plus order attribution map.
struct LiveSink<'a> {
    order_tx: &'a mpsc::Sender<Order>,
    telem_tx: &'a mpsc::Sender<TelemetryEvent>,
    order_strategies: &'a mut HashMap<u64, (&'static str, Side)>,
    /// Per-batch eval latency (for telemetry records).
    eval_us: u64,
    dispatched: bool,
}

impl<'a> LiveSink<'a> {
    fn new(
        order_tx: &'a mpsc::Sender<Order>,
        telem_tx: &'a mpsc::Sender<TelemetryEvent>,
        order_strategies: &'a mut HashMap<u64, (&'static str, Side)>,
        eval_us: u64,
    ) -> Self {
        Self {
            order_tx,
            telem_tx,
            order_strategies,
            eval_us,
            dispatched: false,
        }
    }
}

impl<'a> SignalSink for LiveSink<'a> {
    fn on_signal(&mut self, sig: &Signal, state: &MarketState, now_ms: i64) {
        let time_left_s = state.time_left_s(now_ms);
        let distance = state.distance();
        let _ = self.telem_tx.try_send(TelemetryEvent::Signal(SignalRecord {
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
            eval_latency_us: self.eval_us,
            selected: false,
        }));
    }

    fn on_order(&mut self, sig: &Signal, order: &Order, state: &MarketState, now_ms: i64) {
        let time_left_s = state.time_left_s(now_ms);

        self.order_strategies.insert(order.id, (sig.strategy, sig.side));

        let _ = self.telem_tx.try_send(TelemetryEvent::OrderSent(OrderRecord {
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

        eprintln!(
            "[SIG] {} {:?} edge={:.3} fair={:.3} mkt={:.3} sz=${:.1} {} {:?} post_only={}",
            sig.strategy, sig.side, sig.edge, sig.fair_value,
            sig.market_price, order.size,
            if sig.is_passive { "PASSIVE" } else { "ACTIVE" },
            order.order_type, order.post_only,
        );

        // Set token_id from MarketInfo before dispatch
        let mut order = order.clone();
        order.token_id = match order.side {
            Side::Up => state.info.up_token_id.clone(),
            Side::Down => state.info.down_token_id.clone(),
        };

        if let Err(e) = self.order_tx.try_send(order) {
            eprintln!("[WARN] Order channel full, dropping order #{}", e.into_inner().id);
        }
        self.dispatched = true;
    }
}

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
    let mut flip_count: u32 = 0;

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

    // Per-market warmup: require fresh EWMA samples collected AFTER this market starts.
    // On market 1, BinanceState starts empty so this aligns with ewma_vol.is_valid().
    // On market 2+, BinanceState is pre-populated from the prior market, so
    // ewma_vol.is_valid() is immediately true. This counter ensures we collect
    // fresh volatility data for the new market before trading.
    let warmup_samples_at_start = state.bn.ewma_vol.n_samples();
    const MIN_FRESH_SAMPLES: u32 = 10; // 10 one-second samples (~10s of fresh data)

    eprintln!(
        "[ENGINE] Running market {} | strike=${:.0} | window={}s | bankroll=${:.0} | ewma_n={} | warmup_baseline={}",
        state.info.slug,
        state.info.strike,
        (state.info.end_ms - state.info.start_ms) / 1000,
        config.bankroll,
        state.bn.ewma_vol.n_samples(),
        warmup_samples_at_start,
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

                // ── Open-window strategies (strike_misalign) are exempt from
                // fresh-samples warmup. They only need ewma_vol.is_valid()
                // (checked above). Their edge comes from strike-VWAP divergence,
                // not from having perfectly fresh vol. Waiting 10s would eat
                // most of the opening window on short intervals (5m=15s).
                let elapsed_ms = now_ms - state.info.start_ms;
                let in_open_window = elapsed_ms >= 0 && elapsed_ms <= config.interval.open_window_ms();
                if in_open_window {
                    let eval_start = Instant::now();
                    evaluate_filtered(&open_strategies, &state, now_ms, &mut open_buf);
                    if !open_buf.is_empty() {
                        let eval_us = eval_start.elapsed().as_micros() as u64;
                        let config = ProcessConfig::live();
                        let mut sink = LiveSink::new(&order_tx, &telem_tx, &mut order_strategies, eval_us);
                        pipeline::process_signals(
                            &mut open_buf, &mut state, &mut risk,
                            &mut house_side, &mut flip_count, &mut next_order_id, now_ms,
                            &config, &mut sink,
                        );
                        if sink.dispatched {
                            let e2e_us = recv_at.elapsed().as_micros() as u64;
                            let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                                ts_ms: now_ms, event: "e2e", latency_us: e2e_us,
                            }));
                        }
                    }
                }

                // Per-market warmup: require MIN_FRESH_SAMPLES new 1-second EWMA
                // samples collected since this market started.
                let fresh_samples = state.bn.ewma_vol.n_samples() - warmup_samples_at_start;
                if !warmup_done {
                    if fresh_samples < MIN_FRESH_SAMPLES {
                        continue;
                    }
                    warmup_done = true;
                    eprintln!(
                        "[ENGINE] Warmup complete: σ_real={:.8} σ_raw={:.8} total_samples={} fresh={} | trading enabled",
                        state.sigma_real(),
                        state.bn.ewma_vol.sigma(),
                        state.bn.ewma_vol.n_samples(),
                        fresh_samples,
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

                let eval_us = eval_start.elapsed().as_micros() as u64;
                let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                    ts_ms: now_ms,
                    event: "eval_binance",
                    latency_us: eval_us,
                }));

                {
                    let config = ProcessConfig::live();
                    let mut sink = LiveSink::new(&order_tx, &telem_tx, &mut order_strategies, eval_us);
                    pipeline::process_signals(
                        &mut signals_buf, &mut state, &mut risk,
                        &mut house_side, &mut flip_count, &mut next_order_id, now_ms,
                        &config, &mut sink,
                    );
                    if sink.dispatched {
                        let e2e_us = recv_at.elapsed().as_micros() as u64;
                        let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                            ts_ms: now_ms, event: "e2e", latency_us: e2e_us,
                        }));
                    }
                }
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

                if !state.has_data() {
                    continue;
                }

                // Open-window strategies: exempt from fresh-samples warmup
                // (only need ewma_vol.is_valid(), checked via has_data)
                if !warmup_done && state.bn.ewma_vol.is_valid() {
                    let elapsed_ms = now_ms - state.info.start_ms;
                    if elapsed_ms >= 0 && elapsed_ms <= config.interval.open_window_ms() {
                        evaluate_filtered(&open_strategies, &state, now_ms, &mut open_buf);
                        if !open_buf.is_empty() {
                            let eval_us = 0u64;
                            let config = ProcessConfig::live();
                            let mut sink = LiveSink::new(&order_tx, &telem_tx, &mut order_strategies, eval_us);
                            pipeline::process_signals(
                                &mut open_buf, &mut state, &mut risk,
                                &mut house_side, &mut flip_count, &mut next_order_id, now_ms,
                                &config, &mut sink,
                            );
                        }
                    }
                    continue;
                }

                if !warmup_done {
                    continue;
                }

                let eval_start = Instant::now();
                evaluate_filtered(&pm_strategies, &state, now_ms, &mut signals_buf);

                let elapsed_ms = now_ms - state.info.start_ms;
                if elapsed_ms >= 0 && elapsed_ms <= config.interval.open_window_ms() {
                    evaluate_filtered(&open_strategies, &state, now_ms, &mut open_buf);
                    signals_buf.extend(open_buf.drain(..));
                }

                let eval_us = eval_start.elapsed().as_micros() as u64;
                let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                    ts_ms: now_ms,
                    event: "eval_pm",
                    latency_us: eval_us,
                }));

                {
                    let config = ProcessConfig::live();
                    let mut sink = LiveSink::new(&order_tx, &telem_tx, &mut order_strategies, eval_us);
                    pipeline::process_signals(
                        &mut signals_buf, &mut state, &mut risk,
                        &mut house_side, &mut flip_count, &mut next_order_id, now_ms,
                        &config, &mut sink,
                    );
                    if sink.dispatched {
                        let e2e_us = recv_at.elapsed().as_micros() as u64;
                        let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                            ts_ms: now_ms, event: "e2e", latency_us: e2e_us,
                        }));
                    }
                }
            }

            FeedEvent::PolymarketBook(book) => {
                let recv_at = book.recv_at;
                state.on_book_update(book);

                if !state.has_data() {
                    continue;
                }

                // Open-window strategies: exempt from fresh-samples warmup
                if !warmup_done && state.bn.ewma_vol.is_valid() {
                    let elapsed_ms = now_ms - state.info.start_ms;
                    if elapsed_ms >= 0 && elapsed_ms <= config.interval.open_window_ms() {
                        evaluate_filtered(&open_strategies, &state, now_ms, &mut open_buf);
                        if !open_buf.is_empty() {
                            let eval_us = 0u64;
                            let config = ProcessConfig::live();
                            let mut sink = LiveSink::new(&order_tx, &telem_tx, &mut order_strategies, eval_us);
                            pipeline::process_signals(
                                &mut open_buf, &mut state, &mut risk,
                                &mut house_side, &mut flip_count, &mut next_order_id, now_ms,
                                &config, &mut sink,
                            );
                        }
                    }
                    continue;
                }

                if !warmup_done {
                    continue;
                }

                let eval_start = Instant::now();
                evaluate_filtered(&pm_strategies, &state, now_ms, &mut signals_buf);

                let elapsed_ms = now_ms - state.info.start_ms;
                if elapsed_ms >= 0 && elapsed_ms <= config.interval.open_window_ms() {
                    evaluate_filtered(&open_strategies, &state, now_ms, &mut open_buf);
                    signals_buf.extend(open_buf.drain(..));
                }

                let eval_us = eval_start.elapsed().as_micros() as u64;
                let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                    ts_ms: now_ms,
                    event: "eval_book",
                    latency_us: eval_us,
                }));

                {
                    let config = ProcessConfig::live();
                    let mut sink = LiveSink::new(&order_tx, &telem_tx, &mut order_strategies, eval_us);
                    pipeline::process_signals(
                        &mut signals_buf, &mut state, &mut risk,
                        &mut house_side, &mut flip_count, &mut next_order_id, now_ms,
                        &config, &mut sink,
                    );
                    if sink.dispatched {
                        let e2e_us = recv_at.elapsed().as_micros() as u64;
                        let _ = telem_tx.try_send(TelemetryEvent::Latency(LatencyRecord {
                            ts_ms: now_ms, event: "e2e", latency_us: e2e_us,
                        }));
                    }
                }
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

        let post_buffer_ms = config.interval.post_end_buffer_secs() * 1000;
        if now_ms >= state.info.end_ms + post_buffer_ms {
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
        "[ENGINE] Market {} ended | outcome={:?} | house={:?} | flips={} | sig={} ord={} fill={} pnl=${:.2} ({}fills settled)",
        state.info.slug, outcome, house_side, flip_count, state.total_signals, state.total_orders,
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
    let market_dur_s = (state.info.end_ms - state.info.start_ms) as f64 / 1000.0;
    let lp_min_tau = (market_dur_s * 0.20).max(60.0);
    let lp_tau_msg = format!("tau<{:.0}", lp_min_tau);
    let lp_gate = if regime == Regime::Trend { "regime=Trend" }
        else if tau < lp_min_tau { &lp_tau_msg }
        else if z_abs < 1.5 { "z<1.5" }
        else if lp_ask <= 0.0 || lp_ask >= 0.25 { "ask>=0.25" }
        else { "PASS" };
    eprintln!(
        "[DIAG]   lp_extreme: z_abs={:.2} losing_side={} ask={:.3} regime={:?} → {}",
        z_abs, lp_side, lp_ask, regime, lp_gate,
    );

    // strike_misalign: only in open window (interval-dependent)
    let elapsed_ms = now_ms - state.info.start_ms;
    let window_s = (state.info.end_ms - state.info.start_ms) / 1000;
    // Approximate: open window is ~5% of total interval, capped per config
    let sm_gate = if elapsed_ms > 15_000 { "past_open_window" } else { "in_window" };
    eprintln!(
        "[DIAG]   strike_misalign: elapsed={}ms window={}s → {}",
        elapsed_ms, window_s, sm_gate,
    );
}

