use std::collections::VecDeque;
use std::io::{self, Write as IoWrite};
use std::time::Instant;

use polymarket_crypto::engine::risk::StrategyRiskManager;
use polymarket_crypto::engine::state::{BinanceState, MarketState};
use polymarket_crypto::math::oracle::OracleBasis;
use polymarket_crypto::math::pricing::{delta_bin, p_fair, z_score};
use polymarket_crypto::math::regime::Regime;
use polymarket_crypto::strategies::certainty_capture::CertaintyCapture;
use polymarket_crypto::strategies::convexity_fade::ConvexityFade;
use polymarket_crypto::strategies::cross_timeframe::CrossTimeframe;
use polymarket_crypto::strategies::latency_arb::LatencyArb;
use polymarket_crypto::strategies::lp_extreme::LpExtreme;
use polymarket_crypto::strategies::strike_misalign::StrikeMisalign;
use polymarket_crypto::strategies::{evaluate_filtered, Strategy};
use polymarket_crypto::types::*;

use crate::types::{
    App, LoadedMarketInfo, OrderEntry, ReplayEvent, SignalEntry, replay_config,
};

// ─── Constants ───

const CHART_HISTORY_CAP: usize = 500;
const VOLUME_HISTORY_CAP: usize = 200;
const PM_QUOTE_CHART_CAP: usize = 500;

// ─── Strategy helpers (avoid repeating strategy setup in 3 places) ───

struct StrategySet {
    latency_arb: LatencyArb,
    certainty_capture: CertaintyCapture,
    convexity_fade: ConvexityFade,
    cross_timeframe: CrossTimeframe,
    strike_misalign: StrikeMisalign,
    lp_extreme: LpExtreme,
}

impl StrategySet {
    fn new() -> Self {
        Self {
            latency_arb: LatencyArb,
            certainty_capture: CertaintyCapture,
            convexity_fade: ConvexityFade,
            cross_timeframe: CrossTimeframe,
            strike_misalign: StrikeMisalign,
            lp_extreme: LpExtreme,
        }
    }

    fn binance_strategies(&self) -> Vec<&dyn Strategy> {
        vec![&self.latency_arb, &self.lp_extreme]
    }

    fn pm_strategies(&self) -> Vec<&dyn Strategy> {
        vec![
            &self.certainty_capture,
            &self.convexity_fade,
            &self.cross_timeframe,
            &self.lp_extreme,
        ]
    }

    fn open_strategies(&self) -> Vec<&dyn Strategy> {
        vec![&self.strike_misalign]
    }
}

// ─── MarketState construction helper ───

fn new_market_state(info: &LoadedMarketInfo, strike: f64) -> MarketState {
    let oracle = OracleBasis::new(0.0, 2.0);
    let bs = BinanceState::new(0.94, 10, 0.30, 60_000, 30_000);
    MarketState::new(
        MarketInfo {
            slug: info.slug.clone(),
            start_ms: info.start_ms,
            end_ms: info.end_ms,
            up_token_id: String::new(),
            down_token_id: String::new(),
            strike,
        },
        bs,
        oracle,
    )
}

// ─── Evaluate strategies for one event, collect signals & orders ───

fn evaluate_event(
    event: &ReplayEvent,
    state: &MarketState,
    strats: &StrategySet,
    start_ms: i64,
    signal_buf: &mut Vec<Signal>,
    open_buf: &mut Vec<Signal>,
) {
    let now_ms = event.ts_ms();
    // Open window scales with market duration: ~5% of window, capped [15s, 300s]
    let market_duration_ms = state.info.end_ms - state.info.start_ms;
    let open_window_ms = (market_duration_ms / 20).clamp(15_000, 300_000);
    match event {
        ReplayEvent::Binance { .. } => {
            evaluate_filtered(&strats.binance_strategies(), state, now_ms, signal_buf);
            let elapsed_ms = now_ms - start_ms;
            if elapsed_ms >= 0 && elapsed_ms <= open_window_ms {
                evaluate_filtered(&strats.open_strategies(), state, now_ms, open_buf);
                signal_buf.extend(open_buf.drain(..));
            }
        }
        ReplayEvent::Polymarket { .. } | ReplayEvent::Book { .. } => {
            evaluate_filtered(&strats.pm_strategies(), state, now_ms, signal_buf);
            let elapsed_ms = now_ms - start_ms;
            if elapsed_ms >= 0 && elapsed_ms <= open_window_ms {
                evaluate_filtered(&strats.open_strategies(), state, now_ms, open_buf);
                signal_buf.extend(open_buf.drain(..));
            }
        }
    }
}

fn record_signals_and_orders(
    signal_buf: &[Signal],
    state: &MarketState,
    event_idx: usize,
    risk: &mut StrategyRiskManager,
    house_side: &mut Option<Side>,
    next_order_id: &mut u64,
    signal_log: &mut Vec<SignalEntry>,
    order_log: &mut Vec<OrderEntry>,
    now_ms: i64,
) {
    let btc = state.bn.binance_price;
    let time_left_s = state.time_left_s(now_ms);

    for sig in signal_buf {
        signal_log.push(SignalEntry {
            event_idx,
            btc_price: btc,
            strategy: sig.strategy.to_string(),
            side: format!("{}", sig.side),
            edge: sig.edge,
            fair_value: sig.fair_value,
            market_price: sig.market_price,
            time_left_s,
            is_passive: sig.is_passive,
        });

        if let Some(order) = risk.check_strategy(sig, state, *next_order_id, now_ms) {
            if house_side.is_none() && !sig.is_passive {
                *house_side = Some(sig.side);
            }
            order_log.push(OrderEntry {
                event_idx,
                btc_price: btc,
                id: order.id,
                strategy: sig.strategy.to_string(),
                side: format!("{}", sig.side),
                price: order.price,
                size: order.size,
                edge: sig.edge,
                time_left_s,
                is_passive: sig.is_passive,
            });
            risk.on_order_sent(sig.strategy, now_ms, order.size);
            *next_order_id += 1;
        }
    }
}

// ─── App implementation ───

impl App {
    pub fn new(
        events: Vec<ReplayEvent>,
        market_info: LoadedMarketInfo,
        strike: f64,
        data_dir: String,
    ) -> Self {
        let state = new_market_state(&market_info, strike);
        App {
            events,
            cursor: 0,
            state,
            market_info,
            data_dir,
            playing: false,
            speed: 1,
            snapshots: Vec::new(),
            snapshot_interval: 1000,
            price_history: VecDeque::with_capacity(CHART_HISTORY_CAP),
            vwap_history: VecDeque::with_capacity(CHART_HISTORY_CAP),
            buy_vol_history: VecDeque::with_capacity(VOLUME_HISTORY_CAP),
            sell_vol_history: VecDeque::with_capacity(VOLUME_HISTORY_CAP),
            up_bid_chart: VecDeque::with_capacity(PM_QUOTE_CHART_CAP),
            up_ask_chart: VecDeque::with_capacity(PM_QUOTE_CHART_CAP),
            down_bid_chart: VecDeque::with_capacity(PM_QUOTE_CHART_CAP),
            down_ask_chart: VecDeque::with_capacity(PM_QUOTE_CHART_CAP),
            signal_log: Vec::new(),
            order_log: Vec::new(),
            risk: StrategyRiskManager::new(&replay_config()),
            house_side: None,
            next_order_id: 1,
            fake_instant: Instant::now(),
            status_msg: None,
        }
    }

    // ── Snapshots ──

    pub fn build_snapshots(&mut self) {
        let mut state = new_market_state(&self.market_info, self.state.info.strike);
        self.snapshots.push((0, state.clone()));

        for (i, event) in self.events.iter().enumerate() {
            Self::apply_event(&mut state, event, self.fake_instant);
            if (i + 1) % self.snapshot_interval == 0 {
                self.snapshots.push((i + 1, state.clone()));
            }
        }
    }

    fn nearest_snapshot(&self, target: usize) -> (usize, &MarketState) {
        let idx = match self.snapshots.binary_search_by_key(&target, |(i, _)| *i) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let (cursor, state) = &self.snapshots[idx];
        (*cursor, state)
    }

    // ── Event application ──

    pub fn apply_event(state: &mut MarketState, event: &ReplayEvent, fake_instant: Instant) {
        match event {
            ReplayEvent::Binance { ts_ms, price, qty, is_buy } => {
                state.on_binance_trade(BinanceTrade {
                    exchange_ts_ms: *ts_ms,
                    recv_at: fake_instant,
                    price: *price,
                    qty: *qty,
                    is_buy: *is_buy,
                });
            }
            ReplayEvent::Polymarket { ts_ms, up_bid, up_ask, down_bid, down_ask } => {
                let is_real_bid = |v: f64| v > 0.02;
                let is_real_ask = |v: f64| v > 0.0 && v < 0.98;
                state.on_polymarket_quote(PolymarketQuote {
                    server_ts_ms: *ts_ms,
                    recv_at: fake_instant,
                    up_bid: if is_real_bid(*up_bid) { Some(*up_bid) } else { None },
                    up_ask: if is_real_ask(*up_ask) { Some(*up_ask) } else { None },
                    down_bid: if is_real_bid(*down_bid) { Some(*down_bid) } else { None },
                    down_ask: if is_real_ask(*down_ask) { Some(*down_ask) } else { None },
                });
            }
            ReplayEvent::Book { is_up, bids, asks, .. } => {
                state.on_book_update(PolymarketBook {
                    recv_at: fake_instant,
                    is_up_token: *is_up,
                    bids: bids.clone(),
                    asks: asks.clone(),
                });
            }
        }
    }

    // ── History tracking ──

    fn push_binance_history(&mut self, idx: usize, price: f64, qty: f64, is_buy: bool, vwap: f64) {
        push_capped(&mut self.price_history, CHART_HISTORY_CAP, (idx as f64, price));

        if vwap > 0.0 {
            push_capped(&mut self.vwap_history, CHART_HISTORY_CAP, (idx as f64, vwap));
        }

        let vol_scaled = (qty * 1_000_000.0) as u64;
        if is_buy {
            push_capped(&mut self.buy_vol_history, VOLUME_HISTORY_CAP, vol_scaled);
            push_capped(&mut self.sell_vol_history, VOLUME_HISTORY_CAP, 0);
        } else {
            push_capped(&mut self.sell_vol_history, VOLUME_HISTORY_CAP, vol_scaled);
            push_capped(&mut self.buy_vol_history, VOLUME_HISTORY_CAP, 0);
        }
    }

    fn push_quote_history(&mut self, idx: usize, up_bid: f64, up_ask: f64, down_bid: f64, down_ask: f64) {
        let x = idx as f64;
        if up_bid > 0.01 {
            push_capped(&mut self.up_bid_chart, PM_QUOTE_CHART_CAP, (x, up_bid));
        }
        if up_ask > 0.0 && up_ask < 0.99 {
            push_capped(&mut self.up_ask_chart, PM_QUOTE_CHART_CAP, (x, up_ask));
        }
        if down_bid > 0.01 {
            push_capped(&mut self.down_bid_chart, PM_QUOTE_CHART_CAP, (x, down_bid));
        }
        if down_ask > 0.0 && down_ask < 0.99 {
            push_capped(&mut self.down_ask_chart, PM_QUOTE_CHART_CAP, (x, down_ask));
        }
    }

    fn clear_histories(&mut self) {
        self.price_history.clear();
        self.vwap_history.clear();
        self.buy_vol_history.clear();
        self.sell_vol_history.clear();
        self.up_bid_chart.clear();
        self.up_ask_chart.clear();
        self.down_bid_chart.clear();
        self.down_ask_chart.clear();
    }

    // ── Navigation ──

    pub fn jump_to(&mut self, target: usize) {
        let target = target.min(self.events.len());
        let (snap_cursor, snap_state) = self.nearest_snapshot(target);
        self.state = snap_state.clone();

        for i in snap_cursor..target {
            Self::apply_event(&mut self.state, &self.events[i], self.fake_instant);
        }

        self.rebuild_history(target);
        self.cursor = target;
    }

    fn rebuild_history(&mut self, up_to: usize) {
        self.clear_histories();

        let start = up_to.saturating_sub(CHART_HISTORY_CAP);
        let (snap_cursor, snap_state) = self.nearest_snapshot(start);
        let mut tmp_state = snap_state.clone();

        // Replay state from snapshot to history window start (no recording)
        for i in snap_cursor..start {
            Self::apply_event(&mut tmp_state, &self.events[i], self.fake_instant);
        }

        // Replay history window, recording chart data
        for i in start..up_to {
            let event = &self.events[i];
            Self::apply_event(&mut tmp_state, event, self.fake_instant);
            match event {
                ReplayEvent::Binance { price, qty, is_buy, .. } => {
                    let vwap = tmp_state.bn.vwap_tracker.vwap();
                    self.push_binance_history(i, *price, *qty, *is_buy, vwap);
                }
                ReplayEvent::Polymarket { up_bid, up_ask, down_bid, down_ask, .. } => {
                    self.push_quote_history(i, *up_bid, *up_ask, *down_bid, *down_ask);
                }
                _ => {}
            }
        }
    }

    pub fn step_forward(&mut self, n: usize) {
        let strats = StrategySet::new();
        let mut signal_buf: Vec<Signal> = Vec::new();
        let mut open_buf: Vec<Signal> = Vec::new();
        let end = (self.cursor + n).min(self.events.len());

        for i in self.cursor..end {
            let event = self.events[i].clone();
            Self::apply_event(&mut self.state, &event, self.fake_instant);

            // Update chart histories
            match &event {
                ReplayEvent::Binance { price, qty, is_buy, .. } => {
                    let vwap = self.state.bn.vwap_tracker.vwap();
                    self.push_binance_history(i, *price, *qty, *is_buy, vwap);
                }
                ReplayEvent::Polymarket { up_bid, up_ask, down_bid, down_ask, .. } => {
                    self.push_quote_history(i, *up_bid, *up_ask, *down_bid, *down_ask);
                }
                _ => {}
            }

            // Evaluate strategies & collect signals
            if self.state.has_data() {
                evaluate_event(
                    &event, &self.state, &strats,
                    self.market_info.start_ms,
                    &mut signal_buf, &mut open_buf,
                );

                if let Some(hs) = self.house_side {
                    signal_buf.retain(|s| s.is_passive || s.side == hs);
                }

                let now_ms = event.ts_ms();
                record_signals_and_orders(
                    &signal_buf, &self.state, i,
                    &mut self.risk, &mut self.house_side, &mut self.next_order_id,
                    &mut self.signal_log, &mut self.order_log, now_ms,
                );
            }
            signal_buf.clear();
        }
        self.cursor = end;
    }

    pub fn step_back(&mut self, n: usize) {
        let target = self.cursor.saturating_sub(n);
        self.signal_log.clear();
        self.order_log.clear();
        self.risk = StrategyRiskManager::new(&replay_config());
        self.house_side = None;
        self.next_order_id = 1;
        self.jump_to(target);

        let sig_start = target.saturating_sub(1000);
        self.collect_signals_range(sig_start, target);
    }

    fn collect_signals_range(&mut self, from: usize, to: usize) {
        let (snap_cursor, snap_state) = self.nearest_snapshot(from);
        let mut tmp_state = snap_state.clone();

        let strats = StrategySet::new();
        let mut signal_buf: Vec<Signal> = Vec::new();
        let mut open_buf: Vec<Signal> = Vec::new();

        for i in snap_cursor..to {
            let event = &self.events[i];
            Self::apply_event(&mut tmp_state, event, self.fake_instant);

            if i < from || !tmp_state.has_data() {
                continue;
            }

            evaluate_event(
                event, &tmp_state, &strats,
                self.market_info.start_ms,
                &mut signal_buf, &mut open_buf,
            );

            if let Some(hs) = self.house_side {
                signal_buf.retain(|s| s.is_passive || s.side == hs);
            }

            let now_ms = event.ts_ms();
            record_signals_and_orders(
                &signal_buf, &tmp_state, i,
                &mut self.risk, &mut self.house_side, &mut self.next_order_id,
                &mut self.signal_log, &mut self.order_log, now_ms,
            );
            signal_buf.clear();
        }
    }

    // ── CSV export ──

    pub fn export_csv(&self) -> Result<String, String> {
        let up_to = self.cursor;
        if up_to == 0 {
            return Err("Nothing to export (cursor at 0)".to_string());
        }

        let out_path = format!("{}/replay_dump.csv", self.data_dir);
        let file = std::fs::File::create(&out_path)
            .map_err(|e| format!("Failed to create {}: {}", out_path, e))?;
        let mut w = io::BufWriter::new(file);

        let header = [
            "idx","ts_ms","event_type",
            "btc_price","strike","distance","distance_frac","s_est",
            "time_left_s","tau_eff_s",
            "sigma","z_score","p_fair","delta",
            "vwap",
            "regime","regime_frac","regime_trend_up",
            "ewma_n","ewma_sigma_raw",
            "up_bid","up_ask","down_bid","down_ask",
            "up_book_best_bid","up_book_best_ask","up_book_spread","up_book_microprice","up_book_imbalance","up_book_bid_depth5","up_book_ask_depth5",
            "dn_book_best_bid","dn_book_best_ask","dn_book_spread","dn_book_microprice","dn_book_imbalance","dn_book_bid_depth5","dn_book_ask_depth5",
            "signal_strategy","signal_side","signal_edge","signal_fair","signal_mkt","signal_confidence","signal_size_frac","signal_passive",
            "order_id","order_strategy","order_side","order_price","order_size","order_edge","order_passive",
        ];
        writeln!(w, "{}", header.join(","))
            .map_err(|e| format!("Write error: {}", e))?;

        let mut state = new_market_state(&self.market_info, self.state.info.strike);
        let strats = StrategySet::new();
        let mut signal_buf: Vec<Signal> = Vec::new();
        let mut open_buf: Vec<Signal> = Vec::new();
        let mut risk = StrategyRiskManager::new(&replay_config());
        let mut house_side: Option<Side> = None;
        let mut next_order_id: u64 = 1;

        for i in 0..up_to {
            let event = &self.events[i];
            Self::apply_event(&mut state, event, self.fake_instant);

            let ts_ms = event.ts_ms();
            let btc_price = state.bn.binance_price;
            let strike = state.info.strike;
            let s_est = state.s_est();
            let tau = state.tau_eff_s(ts_ms);
            let sigma = state.sigma_real();

            let (pf, z, delta) = if sigma > 0.0 && tau > 0.0 && s_est > 0.0 && strike > 0.0 {
                (p_fair(s_est, strike, sigma, tau), z_score(s_est, strike, sigma, tau), delta_bin(s_est, strike, sigma, tau))
            } else {
                (0.0, 0.0, 0.0)
            };

            let regime = state.bn.regime.classify();
            let regime_str = match regime {
                Regime::Range => "Range",
                Regime::Trend => "Trend",
                Regime::Ambiguous => "Ambiguous",
            };

            let base: Vec<String> = vec![
                format!("{}", i), format!("{}", ts_ms), event.type_label().to_string(),
                format!("{}", btc_price), format!("{}", strike),
                format!("{:.2}", state.distance()), format!("{:.6}", state.distance_frac()),
                format!("{:.2}", s_est), format!("{:.3}", state.time_left_s(ts_ms)), format!("{:.3}", tau),
                format!("{:.8}", sigma), format!("{:.4}", z), format!("{:.6}", pf), format!("{:.8}", delta),
                format!("{:.2}", state.bn.vwap_tracker.vwap()),
                regime_str.to_string(), format!("{:.4}", state.bn.regime.dominant_frac()),
                format!("{}", state.bn.regime.trend_direction_up()),
                format!("{}", state.bn.ewma_vol.n_samples()), format!("{:.8}", state.bn.ewma_vol.sigma()),
                format!("{}", state.up_bid), format!("{}", state.up_ask),
                format!("{}", state.down_bid), format!("{}", state.down_ask),
                format!("{:.4}", state.up_book.best_bid()), format!("{:.4}", state.up_book.best_ask()),
                format!("{:.4}", state.up_book.spread()), format!("{:.4}", state.up_book.microprice()),
                format!("{:.4}", state.up_book.depth_imbalance(5)),
                format!("{:.1}", state.up_book.bid_depth(5)), format!("{:.1}", state.up_book.ask_depth(5)),
                format!("{:.4}", state.down_book.best_bid()), format!("{:.4}", state.down_book.best_ask()),
                format!("{:.4}", state.down_book.spread()), format!("{:.4}", state.down_book.microprice()),
                format!("{:.4}", state.down_book.depth_imbalance(5)),
                format!("{:.1}", state.down_book.bid_depth(5)), format!("{:.1}", state.down_book.ask_depth(5)),
            ];

            // Evaluate strategies
            let mut signals_this_event: Vec<&Signal> = Vec::new();
            if state.has_data() {
                evaluate_event(event, &state, &strats, self.market_info.start_ms, &mut signal_buf, &mut open_buf);
                if let Some(hs) = house_side {
                    signal_buf.retain(|s| s.is_passive || s.side == hs);
                }
                signals_this_event = signal_buf.iter().collect();
            }

            if signals_this_event.is_empty() {
                writeln!(w, "{},,,,,,,,,,,,,,,", base.join(","))
                    .map_err(|e| format!("Write error: {}", e))?;
            } else {
                for sig in &signals_this_event {
                    let order = risk.check_strategy(sig, &state, next_order_id, ts_ms);
                    let mut row = base.clone();

                    row.push(sig.strategy.to_string());
                    row.push(format!("{}", sig.side));
                    row.push(format!("{:.4}", sig.edge));
                    row.push(format!("{:.4}", sig.fair_value));
                    row.push(format!("{:.4}", sig.market_price));
                    row.push(format!("{:.4}", sig.confidence));
                    row.push(format!("{:.4}", sig.size_frac));
                    row.push(format!("{}", sig.is_passive));

                    if let Some(ref ord) = order {
                        if house_side.is_none() && !sig.is_passive {
                            house_side = Some(sig.side);
                        }
                        row.push(format!("{}", next_order_id));
                        row.push(sig.strategy.to_string());
                        row.push(format!("{}", sig.side));
                        row.push(format!("{:.4}", ord.price));
                        row.push(format!("{:.2}", ord.size));
                        row.push(format!("{:.4}", sig.edge));
                        row.push(format!("{}", sig.is_passive));
                        risk.on_order_sent(sig.strategy, ts_ms, ord.size);
                        next_order_id += 1;
                    } else {
                        for _ in 0..7 { row.push(String::new()); }
                    }

                    writeln!(w, "{}", row.join(","))
                        .map_err(|e| format!("Write error: {}", e))?;
                }
            }
            signal_buf.clear();
        }

        w.flush().map_err(|e| format!("Flush error: {}", e))?;
        Ok(out_path)
    }
}

// ─── Helpers ───

fn push_capped<T>(deque: &mut VecDeque<T>, cap: usize, val: T) {
    if deque.len() >= cap {
        deque.pop_front();
    }
    deque.push_back(val);
}
