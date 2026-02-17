//! Backtester: replays CSV data through the same strategy code used in live trading.
//! No async needed — runs synchronously on historical data.
//! Uses the library's strategies and MarketState directly — zero code duplication.

use std::time::Instant;

use polymarket_crypto::engine::state::{BinanceState, MarketState};
use polymarket_crypto::math::oracle::OracleBasis;
use polymarket_crypto::strategies::latency_arb::LatencyArb;
use polymarket_crypto::strategies::certainty_capture::CertaintyCapture;
use polymarket_crypto::strategies::convexity_fade::ConvexityFade;
use polymarket_crypto::strategies::cross_timeframe::CrossTimeframe;
use polymarket_crypto::strategies::strike_misalign::StrikeMisalign;
use polymarket_crypto::strategies::lp_extreme::LpExtreme;
use polymarket_crypto::strategies::{evaluate_filtered, Strategy};
use polymarket_crypto::types::*;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data_dir = if args.len() > 1 {
        &args[1]
    } else {
        "logs/feeds_15m_full"
    };

    eprintln!("╔══════════════════════════════════════════════════╗");
    eprintln!("║  Polymarket Strategy Backtester                   ║");
    eprintln!("║  Data: {:<42} ║", data_dir);
    eprintln!("╚══════════════════════════════════════════════════╝");

    // Load CSVs
    let binance_trades = load_binance_csv(&format!("{}/binance.csv", data_dir));
    let pm_quotes = load_polymarket_csv(&format!("{}/polymarket.csv", data_dir));
    let (slug, start_ms, end_ms) = load_market_info(&format!("{}/market_info.txt", data_dir));

    eprintln!("Loaded {} Binance trades, {} PM quotes", binance_trades.len(), pm_quotes.len());
    eprintln!("Market: {} | start={} end={}", slug, start_ms, end_ms);

    // Merge events chronologically
    let events = merge_events(&binance_trades, &pm_quotes);
    eprintln!("Merged {} events", events.len());

    // Strike from first Binance price
    let strike = binance_trades
        .first()
        .map(|t| t.price)
        .expect("No Binance trades");
    eprintln!("Strike: ${:.2}", strike);

    // Initialize MarketState with persistent BinanceState
    let oracle = OracleBasis::new(0.0, 2.0); // default oracle params for backtest
    let bs = BinanceState::new(0.94, 10, 0.30, 60_000, 30_000);
    let mut state = MarketState::new(
        MarketInfo {
            slug: slug.clone(),
            start_ms,
            end_ms,
            up_token_id: String::new(),
            down_token_id: String::new(),
            strike,
        },
        bs,
        oracle,
    );

    // All 6 strategies — same as live engine
    let latency_arb = LatencyArb;
    let certainty_capture = CertaintyCapture;
    let convexity_fade = ConvexityFade;
    let cross_timeframe = CrossTimeframe;
    let strike_misalign = StrikeMisalign;
    let lp_extreme = LpExtreme;

    // Partition by trigger type
    let binance_strategies: Vec<&dyn Strategy> = vec![
        &latency_arb,
        &lp_extreme,
    ];
    let pm_strategies: Vec<&dyn Strategy> = vec![
        &certainty_capture,
        &convexity_fade,
        &cross_timeframe,
        &lp_extreme,
    ];
    let open_strategies: Vec<&dyn Strategy> = vec![
        &strike_misalign,
    ];

    let mut signal_buf: Vec<Signal> = Vec::new();
    let mut open_buf: Vec<Signal> = Vec::new();
    let mut results: Vec<BacktestEntry> = Vec::new();
    let mut total_evals = 0u64;
    let fake_instant = Instant::now();

    eprintln!("\n--- Running strategies ---\n");

    for event in &events {
        match event {
            Event::Binance(t) => {
                state.on_binance_trade(BinanceTrade {
                    exchange_ts_ms: t.ts_ms,
                    recv_at: fake_instant,
                    price: t.price,
                    qty: t.qty,
                    is_buy: t.is_buy,
                });

                if !state.has_data() {
                    continue;
                }

                total_evals += 1;
                let now_ms = t.ts_ms;

                // Evaluate Binance-triggered strategies
                evaluate_filtered(&binance_strategies, &state, now_ms, &mut signal_buf);

                // MarketOpen strategies in first 15s
                let elapsed_ms = now_ms - start_ms;
                if elapsed_ms >= 0 && elapsed_ms <= 15_000 {
                    evaluate_filtered(&open_strategies, &state, now_ms, &mut open_buf);
                    signal_buf.extend(open_buf.drain(..));
                }

                for sig in &signal_buf {
                    results.push(BacktestEntry {
                        strategy: sig.strategy.to_string(),
                        side: format!("{}", sig.side),
                        edge: sig.edge,
                        fair_value: sig.fair_value,
                        market_price: sig.market_price,
                        time_left_s: state.time_left_s(now_ms),
                        is_passive: sig.is_passive,
                    });
                }
            }
            Event::Polymarket(q) => {
                state.on_polymarket_quote(PolymarketQuote {
                    server_ts_ms: q.ts_ms,
                    recv_at: fake_instant,
                    up_bid: if q.up_bid > 0.0 { Some(q.up_bid) } else { None },
                    up_ask: if q.up_ask > 0.0 { Some(q.up_ask) } else { None },
                    down_bid: if q.down_bid > 0.0 { Some(q.down_bid) } else { None },
                    down_ask: if q.down_ask > 0.0 { Some(q.down_ask) } else { None },
                });

                if !state.has_data() {
                    continue;
                }

                total_evals += 1;
                let now_ms = q.ts_ms;

                // Evaluate PM-triggered strategies
                evaluate_filtered(&pm_strategies, &state, now_ms, &mut signal_buf);

                // MarketOpen strategies in first 15s
                let elapsed_ms = now_ms - start_ms;
                if elapsed_ms >= 0 && elapsed_ms <= 15_000 {
                    evaluate_filtered(&open_strategies, &state, now_ms, &mut open_buf);
                    signal_buf.extend(open_buf.drain(..));
                }

                for sig in &signal_buf {
                    results.push(BacktestEntry {
                        strategy: sig.strategy.to_string(),
                        side: format!("{}", sig.side),
                        edge: sig.edge,
                        fair_value: sig.fair_value,
                        market_price: sig.market_price,
                        time_left_s: state.time_left_s(now_ms),
                        is_passive: sig.is_passive,
                    });
                }
            }
        }
    }

    // Determine outcome
    let final_distance = state.bn.binance_price - strike;
    let outcome_up = final_distance >= 0.0;
    eprintln!(
        "Final price: ${:.2} | Distance: ${:.0} | Outcome: {}",
        state.bn.binance_price,
        final_distance,
        if outcome_up { "UP wins" } else { "DOWN wins" }
    );

    // Print results
    print_results(&results, outcome_up, total_evals);
}

// ─── Results Printer ───

fn print_results(results: &[BacktestEntry], outcome_up: bool, total_evals: u64) {
    eprintln!("\n{:-<90}", "");
    eprintln!(
        "{:<20} {:>6} {:>8} {:>8} {:>8} {:>8} {:>10} {:>8} {:>6}",
        "Strategy", "Side", "Edge", "Fair", "Ask", "T-left", "Return%", "Correct", "Type"
    );
    eprintln!("{:-<90}", "");

    let mut by_strategy: std::collections::HashMap<String, Vec<&BacktestEntry>> =
        std::collections::HashMap::new();

    for entry in results {
        by_strategy.entry(entry.strategy.clone()).or_default().push(entry);

        let is_up = entry.side == "UP";
        let correct = (is_up && outcome_up) || (!is_up && !outcome_up);
        let ret = if correct {
            (1.0 - entry.market_price) / entry.market_price * 100.0
        } else {
            -100.0
        };

        eprintln!(
            "{:<20} {:>6} {:>8.3} {:>8.3} {:>8.3} {:>7.0}s {:>9.1}% {:>8} {:>6}",
            entry.strategy, entry.side, entry.edge, entry.fair_value,
            entry.market_price, entry.time_left_s, ret,
            if correct { "Y" } else { "N" },
            if entry.is_passive { "PASS" } else { "ACTV" },
        );
    }

    eprintln!("\n{:-<90}", "");
    eprintln!("Summary:");
    eprintln!("  Total evaluations: {}", total_evals);
    eprintln!("  Total signals: {}", results.len());

    // Sort strategies for consistent output
    let mut strat_names: Vec<&String> = by_strategy.keys().collect();
    strat_names.sort();

    for strategy in strat_names {
        let entries = &by_strategy[strategy];
        let correct: usize = entries.iter().filter(|e| {
            let is_up = e.side == "UP";
            (is_up && outcome_up) || (!is_up && !outcome_up)
        }).count();

        let total_invested: f64 = entries.iter().map(|e| e.market_price).sum();
        let total_return: f64 = entries.iter().map(|e| {
            let is_up = e.side == "UP";
            if (is_up && outcome_up) || (!is_up && !outcome_up) {
                1.0 - e.market_price
            } else {
                -e.market_price
            }
        }).sum();

        let avg_edge: f64 = if !entries.is_empty() {
            entries.iter().map(|e| e.edge).sum::<f64>() / entries.len() as f64
        } else {
            0.0
        };

        eprintln!(
            "  {}: {} signals, {}/{} correct, invested ${:.2} -> profit ${:.2} ({:.0}%) avg_edge={:.3}",
            strategy, entries.len(), correct, entries.len(),
            total_invested, total_return,
            if total_invested > 0.0 { total_return / total_invested * 100.0 } else { 0.0 },
            avg_edge,
        );
    }
}

// ─── Backtest-only types (CSV row structs — no strategy logic) ───

struct BacktestEntry {
    strategy: String,
    side: String,
    edge: f64,
    fair_value: f64,
    market_price: f64,
    time_left_s: f64,
    is_passive: bool,
}

struct BinanceCsvRow { ts_ms: i64, price: f64, qty: f64, is_buy: bool }
struct PmCsvRow { ts_ms: i64, up_bid: f64, up_ask: f64, down_bid: f64, down_ask: f64 }
enum Event { Binance(BinanceCsvRow), Polymarket(PmCsvRow) }

// ─── CSV Loaders ───

fn load_binance_csv(path: &str) -> Vec<BinanceCsvRow> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => { eprintln!("Failed to read {}: {}", path, e); return vec![]; }
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

fn load_polymarket_csv(path: &str) -> Vec<PmCsvRow> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => { eprintln!("Failed to read {}: {}", path, e); return vec![]; }
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

fn load_market_info(path: &str) -> (String, i64, i64) {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut slug = String::new();
    let mut start_ms = 0i64;
    let mut end_ms = 0i64;

    for line in content.lines() {
        let (key, val) = if let Some(pos) = line.find('=') {
            (line[..pos].trim(), line[pos + 1..].trim())
        } else if let Some(pos) = line.find(':') {
            (line[..pos].trim(), line[pos + 1..].trim())
        } else {
            continue;
        };
        match key {
            "slug" => slug = val.to_string(),
            "start_ms" => start_ms = val.parse().unwrap_or(0),
            "end_ms" => end_ms = val.parse().unwrap_or(0),
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
    (slug, start_ms, end_ms)
}

fn merge_events(binance: &[BinanceCsvRow], pm: &[PmCsvRow]) -> Vec<Event> {
    let mut events: Vec<(i64, Event)> = Vec::with_capacity(binance.len() + pm.len());
    for b in binance {
        events.push((b.ts_ms, Event::Binance(BinanceCsvRow {
            ts_ms: b.ts_ms, price: b.price, qty: b.qty, is_buy: b.is_buy,
        })));
    }
    for p in pm {
        events.push((p.ts_ms, Event::Polymarket(PmCsvRow {
            ts_ms: p.ts_ms, up_bid: p.up_bid, up_ask: p.up_ask,
            down_bid: p.down_bid, down_ask: p.down_ask,
        })));
    }
    events.sort_by_key(|(ts, _)| *ts);
    events.into_iter().map(|(_, e)| e).collect()
}
