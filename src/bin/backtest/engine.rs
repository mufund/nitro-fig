//! Backtest engine: runs all markets through strategy + risk pipeline,
//! simulates fills (assumes immediate fill at market_ask), and settles PnL.

use std::time::Instant;

use polymarket_crypto::config::{Config, Interval};
use polymarket_crypto::engine::pipeline::{self, ProcessConfig, SignalSink};
use polymarket_crypto::engine::risk::StrategyRiskManager;
use polymarket_crypto::engine::state::{BinanceState, MarketState};
use polymarket_crypto::math::oracle::OracleBasis;
use polymarket_crypto::strategies::certainty_capture::CertaintyCapture;
use polymarket_crypto::strategies::convexity_fade::ConvexityFade;
use polymarket_crypto::strategies::cross_timeframe::CrossTimeframe;
use polymarket_crypto::strategies::latency_arb::LatencyArb;
use polymarket_crypto::strategies::lp_extreme::LpExtreme;
use polymarket_crypto::strategies::strike_misalign::StrikeMisalign;
use polymarket_crypto::strategies::{evaluate_filtered, Strategy};
use polymarket_crypto::types::*;

use crate::types::{MarketResult, TradeRecord};

// Re-use the replay CSV loader types
pub struct BinanceCsvRow {
    pub ts_ms: i64,
    pub price: f64,
    pub qty: f64,
    pub is_buy: bool,
}

pub struct PmCsvRow {
    pub ts_ms: i64,
    pub up_bid: f64,
    pub up_ask: f64,
    pub down_bid: f64,
    pub down_ask: f64,
}

pub struct BookSnapshot {
    pub ts_ms: i64,
    pub is_up: bool,
    pub bids: Vec<(f64, f64)>,
    pub asks: Vec<(f64, f64)>,
}

pub struct LoadedMarketInfo {
    pub slug: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub strike: f64,
}

#[derive(Clone)]
pub enum ReplayEvent {
    Binance { ts_ms: i64, price: f64, qty: f64, is_buy: bool },
    Polymarket { ts_ms: i64, up_bid: f64, up_ask: f64, down_bid: f64, down_ask: f64 },
    Book { ts_ms: i64, is_up: bool, bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)> },
}

impl ReplayEvent {
    pub fn ts_ms(&self) -> i64 {
        match self {
            Self::Binance { ts_ms, .. }
            | Self::Polymarket { ts_ms, .. }
            | Self::Book { ts_ms, .. } => *ts_ms,
        }
    }
}

// ─── Config for backtest (same as replay) ───

pub fn backtest_config() -> Config {
    Config {
        asset: "btc".to_string(),
        interval: Interval::M5,
        binance_ws: String::new(),
        binance_ws_fallback: String::new(),
        polymarket_clob_ws: String::new(),
        gamma_api_url: String::new(),
        series_id: String::new(),
        tg_bot_token: None,
        tg_chat_id: None,
        max_position_usd: 100.0,
        max_orders_per_market: 10,
        cooldown_ms: 5000,
        bankroll: 1000.0,
        max_total_exposure_frac: 0.15,
        daily_loss_halt_frac: -0.03,
        weekly_loss_halt_frac: -0.08,
        oracle_beta: 0.0,
        oracle_delta_s: 2.0,
        ewma_lambda: 0.94,
        sigma_floor_annual: 0.30,
        strategy_latency_arb: true,
        strategy_certainty_capture: true,
        strategy_convexity_fade: true,
        strategy_strike_misalign: true,
        strategy_lp_extreme: true,
        strategy_cross_timeframe: false,
        dry_run: true,
    }
}

// ─── Strategy set ───

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

// ─── CSV Loaders ───

pub fn load_binance_csv(path: &str) -> Vec<BinanceCsvRow> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    content.lines().skip(1).filter_map(|line| {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 6 { return None; }
        let ts_ms = f[2].parse::<i64>().ok()?;
        let price = f[3].parse::<f64>().ok()?;
        if ts_ms <= 0 || price <= 0.0 { return None; }
        Some(BinanceCsvRow {
            ts_ms, price,
            qty: f[4].parse().unwrap_or(0.0),
            is_buy: f[5].trim().to_lowercase() != "sell",
        })
    }).collect()
}

pub fn load_polymarket_csv(path: &str) -> Vec<PmCsvRow> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    content.lines().skip(1).filter_map(|line| {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 8 { return None; }
        let ts_ms = f[1].parse::<i64>().ok()?;
        if ts_ms <= 0 { return None; }
        Some(PmCsvRow {
            ts_ms,
            up_bid: f[4].parse().unwrap_or(0.0),
            up_ask: f[5].parse().unwrap_or(0.0),
            down_bid: f[6].parse().unwrap_or(0.0),
            down_ask: f[7].parse().unwrap_or(0.0),
        })
    }).collect()
}

pub fn load_book_csv(path: &str) -> Vec<BookSnapshot> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut grouped: std::collections::BTreeMap<(i64, bool), (Vec<(f64, f64)>, Vec<(f64, f64)>)> =
        std::collections::BTreeMap::new();
    for line in content.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 7 { continue; }
        let ts_ms = match f[1].parse::<i64>() { Ok(t) if t > 0 => t, _ => continue };
        let is_up = f[2].trim() == "up";
        let side = f[3].trim();
        let price: f64 = match f[5].parse() { Ok(p) if p > 0.0 => p, _ => continue };
        let size: f64 = match f[6].parse() { Ok(s) if s > 0.0 => s, _ => continue };
        let entry = grouped.entry((ts_ms, is_up)).or_insert_with(|| (Vec::new(), Vec::new()));
        match side { "bid" => entry.0.push((price, size)), "ask" => entry.1.push((price, size)), _ => {} }
    }
    grouped.into_iter().map(|((ts_ms, is_up), (bids, asks))| BookSnapshot { ts_ms, is_up, bids, asks }).collect()
}

pub fn load_market_info(path: &str) -> LoadedMarketInfo {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut slug = String::new();
    let mut start_ms = 0i64;
    let mut end_ms = 0i64;
    let mut strike = 0.0_f64;
    for line in content.lines() {
        let (key, val) = if let Some(pos) = line.find('=') {
            (line[..pos].trim(), line[pos + 1..].trim())
        } else if let Some(pos) = line.find(':') {
            (line[..pos].trim(), line[pos + 1..].trim())
        } else { continue };
        match key {
            "slug" => slug = val.to_string(),
            "start_ms" => start_ms = val.parse().unwrap_or(0),
            "end_ms" => end_ms = val.parse().unwrap_or(0),
            "strike" => strike = val.parse().unwrap_or(0.0),
            "start" | "start_date" => {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(val) {
                    if start_ms == 0 { start_ms = dt.timestamp_millis(); }
                }
            }
            "end" | "end_date" => {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(val) {
                    if end_ms == 0 { end_ms = dt.timestamp_millis(); }
                }
            }
            _ => {}
        }
    }
    if slug.is_empty() { slug = "unknown".to_string(); }
    LoadedMarketInfo { slug, start_ms, end_ms, strike }
}

pub fn merge_events(binance: &[BinanceCsvRow], pm: &[PmCsvRow], books: &[BookSnapshot]) -> Vec<ReplayEvent> {
    let mut tagged: Vec<(i64, u8, usize)> = Vec::with_capacity(binance.len() + pm.len() + books.len());
    for (i, b) in binance.iter().enumerate() { tagged.push((b.ts_ms, 0, i)); }
    for (i, p) in pm.iter().enumerate() { tagged.push((p.ts_ms, 1, i)); }
    for (i, b) in books.iter().enumerate() { tagged.push((b.ts_ms, 2, i)); }
    tagged.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    tagged.into_iter().map(|(_, typ, idx)| match typ {
        0 => { let b = &binance[idx]; ReplayEvent::Binance { ts_ms: b.ts_ms, price: b.price, qty: b.qty, is_buy: b.is_buy } }
        1 => { let p = &pm[idx]; ReplayEvent::Polymarket { ts_ms: p.ts_ms, up_bid: p.up_bid, up_ask: p.up_ask, down_bid: p.down_bid, down_ask: p.down_ask } }
        _ => { let b = &books[idx]; ReplayEvent::Book { ts_ms: b.ts_ms, is_up: b.is_up, bids: b.bids.clone(), asks: b.asks.clone() } }
    }).collect()
}

// ─── Discovery: find all market data directories ───

pub fn discover_markets(base_dir: &str) -> Vec<String> {
    let mut dirs: Vec<String> = Vec::new();

    // Try base_dir directly (if it has binance.csv)
    if std::path::Path::new(&format!("{}/binance.csv", base_dir)).exists() {
        dirs.push(base_dir.to_string());
        return dirs;
    }

    // Scan for subdirectories that contain binance.csv
    if let Ok(entries) = std::fs::read_dir(base_dir) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                let csv_path = path.join("binance.csv");
                if csv_path.exists() {
                    dirs.push(path.to_string_lossy().to_string());
                }
            }
        }
    }

    dirs
}

// ─── BacktestSink ───

/// SignalSink implementation for the backtest engine.
/// Pushes fills and trade records directly into Vecs (no async channels).
struct BacktestSink<'a> {
    fills: &'a mut Vec<Fill>,
    trade_records: &'a mut Vec<TradeRecord>,
    market_idx: usize,
    strike: f64,
}

impl<'a> SignalSink for BacktestSink<'a> {
    fn on_signal(&mut self, _sig: &Signal, _state: &MarketState, _now_ms: i64) {
        // Backtest doesn't need per-signal telemetry logging
    }

    fn on_order(&mut self, sig: &Signal, order: &Order, state: &MarketState, now_ms: i64) {
        self.fills.push(Fill {
            order_id: order.id,
            strategy: sig.strategy,
            side: sig.side,
            price: order.price,
            size: order.size,
        });

        let sigma = state.sigma_real();
        let s = state.s_est();
        let k = self.strike;
        let tau = state.tau_eff_s(now_ms);
        let z = if sigma > 0.0 && tau > 0.0 && s > 0.0 && k > 0.0 {
            (s / k).ln() / (sigma * tau.sqrt())
        } else {
            0.0
        };

        self.trade_records.push(TradeRecord {
            market_idx: self.market_idx,
            order_id: order.id,
            strategy: sig.strategy.to_string(),
            side: sig.side,
            price: order.price,
            size: order.size,
            edge: sig.edge,
            fair_value: sig.fair_value,
            confidence: sig.confidence,
            time_left_s: state.time_left_s(now_ms),
            is_passive: sig.is_passive,
            btc_price: state.bn.binance_price,
            strike: self.strike,
            sigma_at_signal: sigma,
            z_at_signal: z,
            distance_at_signal: (s - k).abs(),
            outcome: None,
            pnl: 0.0,
            won: false,
        });
    }
}

// ─── Run backtest for a single market ───

pub fn run_market(data_dir: &str, market_idx: usize, risk: &mut StrategyRiskManager, persistent_bs: Option<BinanceState>) -> Option<(MarketResult, BinanceState)> {
    let binance_trades = load_binance_csv(&format!("{}/binance.csv", data_dir));
    let pm_quotes = load_polymarket_csv(&format!("{}/polymarket.csv", data_dir));
    let book_snapshots = load_book_csv(&format!("{}/book.csv", data_dir));
    let market_info = load_market_info(&format!("{}/market_info.txt", data_dir));

    if binance_trades.is_empty() {
        return None;
    }

    let events = merge_events(&binance_trades, &pm_quotes, &book_snapshots);
    if events.is_empty() {
        return None;
    }

    let strike = if market_info.strike > 0.0 {
        market_info.strike
    } else {
        binance_trades.first().map(|t| t.price)?
    };

    let oracle = OracleBasis::new(0.0, 2.0);
    let bs = persistent_bs.unwrap_or_else(|| BinanceState::new(0.94, 10, 0.30, 60_000, 30_000));
    let mut state = MarketState::new(
        MarketInfo {
            slug: market_info.slug.clone(),
            start_ms: market_info.start_ms,
            end_ms: market_info.end_ms,
            up_token_id: String::new(),
            down_token_id: String::new(),
            strike,
        },
        bs,
        oracle,
    );

    let strats = StrategySet::new();
    let mut signal_buf: Vec<Signal> = Vec::new();
    let mut open_buf: Vec<Signal> = Vec::new();
    let mut house_side: Option<Side> = None;
    let mut flip_count: u32 = 0;
    let mut next_order_id: u64 = 1;
    let fake_instant = Instant::now();
    let n_events = events.len();

    // Pending fills (orders assumed filled at market_ask)
    let mut fills: Vec<Fill> = Vec::new();
    let mut trade_records: Vec<TradeRecord> = Vec::new();

    for event in &events {
        apply_event(&mut state, event, fake_instant);

        if !state.has_data() {
            continue;
        }

        let now_ms = event.ts_ms();

        // Evaluate strategies
        // Open window scales with market duration: ~5% of window, capped [15s, 300s]
        let market_duration_ms = market_info.end_ms - market_info.start_ms;
        let open_window_ms = (market_duration_ms / 20).clamp(15_000, 300_000);
        match event {
            ReplayEvent::Binance { .. } => {
                evaluate_filtered(&strats.binance_strategies(), &state, now_ms, &mut signal_buf);
                let elapsed_ms = now_ms - market_info.start_ms;
                if elapsed_ms >= 0 && elapsed_ms <= open_window_ms {
                    evaluate_filtered(&strats.open_strategies(), &state, now_ms, &mut open_buf);
                    signal_buf.extend(open_buf.drain(..));
                }
            }
            ReplayEvent::Polymarket { .. } | ReplayEvent::Book { .. } => {
                evaluate_filtered(&strats.pm_strategies(), &state, now_ms, &mut signal_buf);
                let elapsed_ms = now_ms - market_info.start_ms;
                if elapsed_ms >= 0 && elapsed_ms <= open_window_ms {
                    evaluate_filtered(&strats.open_strategies(), &state, now_ms, &mut open_buf);
                    signal_buf.extend(open_buf.drain(..));
                }
            }
        }

        // Shared signal pipeline: deconfliction, sorting, risk check, fill simulation
        {
            let config = ProcessConfig::backtest();
            let mut sink = BacktestSink {
                fills: &mut fills,
                trade_records: &mut trade_records,
                market_idx,
                strike,
            };
            pipeline::process_signals(
                &mut signal_buf, &mut state, risk,
                &mut house_side, &mut flip_count, &mut next_order_id, now_ms,
                &config, &mut sink,
            );
        }
        signal_buf.clear();
    }

    // Determine outcome
    let final_price = state.bn.binance_price;
    let final_distance = final_price - strike;
    let outcome = if final_distance >= 0.0 { Side::Up } else { Side::Down };

    // Settle PnL
    let mut total_pnl = 0.0;
    let mut total_invested = 0.0;
    for trade in &mut trade_records {
        trade.outcome = Some(outcome);
        let pnl = if trade.side == outcome {
            (1.0 - trade.price) * trade.size
        } else {
            -(trade.price * trade.size)
        };
        trade.pnl = pnl;
        trade.won = trade.side == outcome;
        total_pnl += pnl;
        total_invested += trade.size;
    }

    // Settle risk manager for multi-market accumulation
    risk.settle_market(outcome, &fills);

    let dir_name = std::path::Path::new(data_dir)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| data_dir.to_string());

    let bs_out = state.take_binance_state();

    Some((MarketResult {
        dir_name,
        slug: market_info.slug,
        strike,
        start_ms: market_info.start_ms,
        end_ms: market_info.end_ms,
        final_price,
        final_distance,
        outcome,
        n_events,
        trades: trade_records,
        total_pnl,
        total_invested,
    }, bs_out))
}

fn apply_event(state: &mut MarketState, event: &ReplayEvent, fake_instant: Instant) {
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

// ─── Run all markets ───

pub fn run_all_markets(market_dirs: &[String]) -> Vec<MarketResult> {
    let config = backtest_config();
    let mut risk = StrategyRiskManager::new(&config);
    let mut results = Vec::new();
    let mut persistent_bs: Option<BinanceState> = None;

    for (i, dir) in market_dirs.iter().enumerate() {
        if let Some((result, bs)) = run_market(dir, i, &mut risk, persistent_bs.take()) {
            results.push(result);
            persistent_bs = Some(bs);
        }
    }

    results
}
