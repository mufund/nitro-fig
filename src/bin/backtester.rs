//! Backtester: replays CSV data through the same strategy code used in live trading.
//! No async needed — runs synchronously on historical data.
//! Uses the library's strategies and MarketState directly — zero code duplication.
//!
//! Supports full orderbook depth via book.csv (optional — degrades gracefully).

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

    // Detect multi-market directory: if data_dir has subdirectories with market_info.txt,
    // iterate over each one. Otherwise treat data_dir as a single market.
    let market_dirs = detect_market_dirs(data_dir);

    let mut all_results: Vec<(Vec<BacktestEntry>, bool, u64, String)> = Vec::new();

    for (i, mdir) in market_dirs.iter().enumerate() {
        if market_dirs.len() > 1 {
            eprintln!("\n{}", "=".repeat(60));
            eprintln!("  Market {}/{}: {}", i + 1, market_dirs.len(), mdir);
            eprintln!("{}", "=".repeat(60));
        }
        let (results, outcome_up, total_evals) = run_single_market(mdir);
        let slug = std::path::Path::new(mdir)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| mdir.to_string());
        all_results.push((results, outcome_up, total_evals, slug));
    }

    // Print combined summary if multi-market
    if all_results.len() > 1 {
        print_combined_results(&all_results);
    }
}

/// Detect whether `data_dir` is a single market or a directory of markets.
fn detect_market_dirs(data_dir: &str) -> Vec<String> {
    // If data_dir itself has market_info.txt, it's a single market
    let info_path = format!("{}/market_info.txt", data_dir);
    if std::path::Path::new(&info_path).exists() {
        return vec![data_dir.to_string()];
    }

    // Otherwise scan subdirectories for market_info.txt
    let mut dirs: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let sub_info = path.join("market_info.txt");
                if sub_info.exists() {
                    dirs.push(path.to_string_lossy().to_string());
                }
            }
        }
    }
    dirs.sort(); // alphabetical order
    if dirs.is_empty() {
        // Fall back to treating it as single market (will error with useful message)
        vec![data_dir.to_string()]
    } else {
        eprintln!("Found {} markets in {}", dirs.len(), data_dir);
        dirs
    }
}

fn run_single_market(data_dir: &str) -> (Vec<BacktestEntry>, bool, u64) {
    // Load CSVs
    let binance_trades = load_binance_csv(&format!("{}/binance.csv", data_dir));
    let pm_quotes = load_polymarket_csv(&format!("{}/polymarket.csv", data_dir));
    let book_snapshots = load_book_csv(&format!("{}/book.csv", data_dir));
    let market_info = load_market_info(&format!("{}/market_info.txt", data_dir));

    eprintln!(
        "Loaded {} Binance trades, {} PM quotes, {} book rows",
        binance_trades.len(),
        pm_quotes.len(),
        book_snapshots.len(),
    );
    eprintln!(
        "Market: {} | start={} end={} strike={:.2}",
        market_info.slug, market_info.start_ms, market_info.end_ms, market_info.strike
    );

    // Merge events chronologically
    let events = merge_events(&binance_trades, &pm_quotes, &book_snapshots);
    eprintln!("Merged {} events", events.len());

    // Strike: prefer market_info, fall back to first Binance price
    let strike = if market_info.strike > 0.0 {
        market_info.strike
    } else {
        binance_trades
            .first()
            .map(|t| t.price)
            .expect("No Binance trades and no strike in market_info")
    };
    eprintln!("Strike: ${:.2}{}", strike, if market_info.strike > 0.0 { " (from market_info)" } else { " (from first Binance trade)" });

    // Initialize MarketState with persistent BinanceState
    let oracle = OracleBasis::new(0.0, 2.0);
    let bs = BinanceState::new(0.94, 10, 0.30, 60_000, 30_000);
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

    let open_window_ms = ((market_info.end_ms - market_info.start_ms) / 20).clamp(15_000, 300_000);

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

                evaluate_filtered(&binance_strategies, &state, now_ms, &mut signal_buf);

                let elapsed_ms = now_ms - market_info.start_ms;
                if elapsed_ms >= 0 && elapsed_ms <= open_window_ms {
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
                let is_real_bid = |v: f64| v > 0.02;
                let is_real_ask = |v: f64| v > 0.0 && v < 0.98;
                state.on_polymarket_quote(PolymarketQuote {
                    server_ts_ms: q.ts_ms,
                    recv_at: fake_instant,
                    up_bid: if is_real_bid(q.up_bid) { Some(q.up_bid) } else { None },
                    up_ask: if is_real_ask(q.up_ask) { Some(q.up_ask) } else { None },
                    down_bid: if is_real_bid(q.down_bid) { Some(q.down_bid) } else { None },
                    down_ask: if is_real_ask(q.down_ask) { Some(q.down_ask) } else { None },
                });

                if !state.has_data() {
                    continue;
                }

                total_evals += 1;
                let now_ms = q.ts_ms;

                evaluate_filtered(&pm_strategies, &state, now_ms, &mut signal_buf);

                let elapsed_ms = now_ms - market_info.start_ms;
                if elapsed_ms >= 0 && elapsed_ms <= open_window_ms {
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
            Event::Book(b) => {
                state.on_book_update(PolymarketBook {
                    recv_at: fake_instant,
                    is_up_token: b.is_up,
                    bids: b.bids.clone(),
                    asks: b.asks.clone(),
                });

                if !state.has_data() {
                    continue;
                }

                total_evals += 1;
                let now_ms = b.ts_ms;

                evaluate_filtered(&pm_strategies, &state, now_ms, &mut signal_buf);

                let elapsed_ms = now_ms - market_info.start_ms;
                if elapsed_ms >= 0 && elapsed_ms <= open_window_ms {
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

    // Print per-market results
    print_results(&results, outcome_up, total_evals);

    (results, outcome_up, total_evals)
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

// ─── Combined Multi-Market Results ───

fn print_combined_results(all_results: &[(Vec<BacktestEntry>, bool, u64, String)]) {
    eprintln!("\n{}", "=".repeat(90));
    eprintln!("  COMBINED RESULTS ({} markets)", all_results.len());
    eprintln!("{}", "=".repeat(90));

    let mut total_signals = 0usize;
    let mut total_correct = 0usize;
    let mut total_invested = 0.0_f64;
    let mut total_profit = 0.0_f64;
    let mut total_evals = 0u64;
    let bankroll: f64 = std::env::var("BANKROLL").ok().and_then(|s| s.parse().ok()).unwrap_or(1000.0);
    let max_exposure: f64 = std::env::var("MAX_EXPOSURE_FRAC").ok().and_then(|s| s.parse().ok()).unwrap_or(0.15);
    let bet_size = bankroll * max_exposure;

    // Per-strategy aggregation across all markets
    let mut by_strategy: std::collections::HashMap<String, (usize, usize, f64, f64, f64)> =
        std::collections::HashMap::new(); // (signals, correct, invested, profit, edge_sum)

    for (results, outcome_up, evals, slug) in all_results {
        let mut market_signals = 0;
        let mut market_correct = 0;
        let mut market_invested = 0.0;
        let mut market_profit = 0.0;

        for entry in results {
            let is_up = entry.side == "UP";
            let correct = (is_up && *outcome_up) || (!is_up && !*outcome_up);
            let profit = if correct {
                1.0 - entry.market_price
            } else {
                -entry.market_price
            };

            market_signals += 1;
            if correct { market_correct += 1; }
            market_invested += entry.market_price;
            market_profit += profit;

            let strat = by_strategy.entry(entry.strategy.clone()).or_insert((0, 0, 0.0, 0.0, 0.0));
            strat.0 += 1;
            if correct { strat.1 += 1; }
            strat.2 += entry.market_price;
            strat.3 += profit;
            strat.4 += entry.edge;
        }

        total_signals += market_signals;
        total_correct += market_correct;
        total_invested += market_invested;
        total_profit += market_profit;
        total_evals += evals;

        let pnl = market_profit * bet_size;
        eprintln!(
            "  {} | {} signals, {}/{} correct, PnL ${:.2}",
            slug, market_signals, market_correct, market_signals, pnl,
        );
    }

    eprintln!("\n{:-<90}", "");
    eprintln!("  Strategy Totals:");

    let mut strat_names: Vec<&String> = by_strategy.keys().collect();
    strat_names.sort();

    for name in &strat_names {
        let (signals, correct, invested, profit, edge_sum) = by_strategy[*name];
        let pnl = profit * bet_size;
        let avg_edge = if signals > 0 { edge_sum / signals as f64 } else { 0.0 };
        eprintln!(
            "    {:<20} {:>3} signals, {}/{} correct, PnL ${:>8.2}, avg_edge={:.3}",
            name, signals, correct, signals, pnl, avg_edge,
        );
    }

    let total_pnl = total_profit * bet_size;
    let win_rate = if total_signals > 0 { total_correct as f64 / total_signals as f64 * 100.0 } else { 0.0 };

    eprintln!("\n{:-<90}", "");
    eprintln!("  TOTAL: {} signals, {}/{} correct ({:.0}% win rate)", total_signals, total_correct, total_signals, win_rate);
    eprintln!("  TOTAL PnL: ${:.2} (bankroll=${:.0}, bet=${:.0})", total_pnl, bankroll, bet_size);
    eprintln!("  Total evaluations: {}", total_evals);
    eprintln!("{:-<90}", "");
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
struct BookSnapshot { ts_ms: i64, is_up: bool, bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)> }
enum Event { Binance(BinanceCsvRow), Polymarket(PmCsvRow), Book(BookSnapshot) }

struct LoadedMarketInfo {
    slug: String,
    start_ms: i64,
    end_ms: i64,
    strike: f64,
}

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

/// Load book.csv — full orderbook depth.
/// Format: recv_time,recv_ts_ms,token,side,level,price,size
/// Groups rows by (ts_ms, token) into BookSnapshot structs.
fn load_book_csv(path: &str) -> Vec<BookSnapshot> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("No book.csv found ({}), proceeding without book depth", e);
            return vec![];
        }
    };

    // Parse all rows, group by (ts_ms, token)
    let mut grouped: std::collections::BTreeMap<(i64, bool), (Vec<(f64, f64)>, Vec<(f64, f64)>)> =
        std::collections::BTreeMap::new();

    for line in content.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 7 { continue; }

        let ts_ms = match f[1].parse::<i64>() {
            Ok(t) if t > 0 => t,
            _ => continue,
        };
        let is_up = f[2].trim() == "up";
        let side = f[3].trim();
        let price: f64 = match f[5].parse() {
            Ok(p) if p > 0.0 => p,
            _ => continue,
        };
        let size: f64 = match f[6].parse() {
            Ok(s) if s > 0.0 => s,
            _ => continue,
        };

        let entry = grouped.entry((ts_ms, is_up)).or_insert_with(|| (Vec::new(), Vec::new()));
        match side {
            "bid" => entry.0.push((price, size)),
            "ask" => entry.1.push((price, size)),
            _ => {}
        }
    }

    grouped
        .into_iter()
        .map(|((ts_ms, is_up), (bids, asks))| BookSnapshot {
            ts_ms,
            is_up,
            bids,
            asks,
        })
        .collect()
}

fn load_market_info(path: &str) -> LoadedMarketInfo {
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
        } else {
            continue;
        };
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

fn merge_events(binance: &[BinanceCsvRow], pm: &[PmCsvRow], books: &[BookSnapshot]) -> Vec<Event> {
    let mut events: Vec<(i64, u8, usize)> = Vec::with_capacity(binance.len() + pm.len() + books.len());

    for (i, b) in binance.iter().enumerate() {
        events.push((b.ts_ms, 0, i));
    }
    for (i, p) in pm.iter().enumerate() {
        events.push((p.ts_ms, 1, i));
    }
    for (i, b) in books.iter().enumerate() {
        events.push((b.ts_ms, 2, i));
    }

    // Sort by timestamp, with books before quotes at same timestamp
    // (so book depth is populated before strategies evaluate on quote)
    events.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    events.into_iter().map(|(_, typ, idx)| {
        match typ {
            0 => {
                let b = &binance[idx];
                Event::Binance(BinanceCsvRow {
                    ts_ms: b.ts_ms, price: b.price, qty: b.qty, is_buy: b.is_buy,
                })
            }
            1 => {
                let p = &pm[idx];
                Event::Polymarket(PmCsvRow {
                    ts_ms: p.ts_ms, up_bid: p.up_bid, up_ask: p.up_ask,
                    down_bid: p.down_bid, down_ask: p.down_ask,
                })
            }
            _ => {
                let b = &books[idx];
                Event::Book(BookSnapshot {
                    ts_ms: b.ts_ms, is_up: b.is_up,
                    bids: b.bids.clone(), asks: b.asks.clone(),
                })
            }
        }
    }).collect()
}
