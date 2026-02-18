//! Backtest TUI: runs all recorded markets through the full strategy + risk pipeline,
//! simulates fills, settles PnL, and presents an interactive dashboard.
//!
//! Usage: cargo run --bin backtest -- <data_dir>
//!   e.g. cargo run --bin backtest -- logs/5m
//!
//! The data_dir can point to:
//!   - A single market directory (with binance.csv, polymarket.csv, etc.)
//!   - A parent directory containing multiple market subdirectories
//!
//! Keys:
//!   [Tab/1-5] Switch tab  [j/k] Scroll  [f] Filter trades  [q/Esc] Quit

mod engine;
mod render;
mod types;

use std::io::{self, stdout};
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event as CEvent, KeyCode, KeyEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;

use crate::types::{BacktestApp, Tab};

fn handle_key(app: &mut BacktestApp, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return true,

        // Tab switching
        KeyCode::Tab => app.tab = app.tab.next(),
        KeyCode::BackTab => app.tab = app.tab.prev(),
        KeyCode::Char('1') => app.tab = Tab::Summary,
        KeyCode::Char('2') => app.tab = Tab::Strategies,
        KeyCode::Char('3') => app.tab = Tab::Markets,
        KeyCode::Char('4') => app.tab = Tab::Trades,
        KeyCode::Char('5') => app.tab = Tab::Equity,

        // Scroll down
        KeyCode::Char('j') | KeyCode::Down => {
            match app.tab {
                Tab::Markets => {
                    if app.market_scroll + 1 < app.markets.len() {
                        app.market_scroll += 1;
                    }
                }
                Tab::Trades => {
                    let max = app.filtered_trades().len().saturating_sub(1);
                    if app.trade_scroll < max {
                        app.trade_scroll += 1;
                    }
                }
                Tab::Strategies => {
                    let max = app.strategy_stats.len().saturating_sub(1);
                    if app.strategy_scroll < max {
                        app.strategy_scroll += 1;
                    }
                }
                _ => {}
            }
        }

        // Scroll up
        KeyCode::Char('k') | KeyCode::Up => {
            match app.tab {
                Tab::Markets => app.market_scroll = app.market_scroll.saturating_sub(1),
                Tab::Trades => app.trade_scroll = app.trade_scroll.saturating_sub(1),
                Tab::Strategies => app.strategy_scroll = app.strategy_scroll.saturating_sub(1),
                _ => {}
            }
        }

        // Page down
        KeyCode::PageDown => {
            match app.tab {
                Tab::Markets => {
                    app.market_scroll = (app.market_scroll + 20).min(app.markets.len().saturating_sub(1));
                }
                Tab::Trades => {
                    let max = app.filtered_trades().len().saturating_sub(1);
                    app.trade_scroll = (app.trade_scroll + 20).min(max);
                }
                _ => {}
            }
        }

        // Page up
        KeyCode::PageUp => {
            match app.tab {
                Tab::Markets => app.market_scroll = app.market_scroll.saturating_sub(20),
                Tab::Trades => app.trade_scroll = app.trade_scroll.saturating_sub(20),
                _ => {}
            }
        }

        // Filter trades by strategy
        KeyCode::Char('f') => {
            if app.tab == Tab::Trades {
                app.cycle_filter_forward();
            }
        }

        // Home / End
        KeyCode::Home => {
            match app.tab {
                Tab::Markets => app.market_scroll = 0,
                Tab::Trades => app.trade_scroll = 0,
                Tab::Strategies => app.strategy_scroll = 0,
                _ => {}
            }
        }
        KeyCode::End => {
            match app.tab {
                Tab::Markets => app.market_scroll = app.markets.len().saturating_sub(1),
                Tab::Trades => app.trade_scroll = app.filtered_trades().len().saturating_sub(1),
                Tab::Strategies => app.strategy_scroll = app.strategy_stats.len().saturating_sub(1),
                _ => {}
            }
        }

        _ => {}
    }
    false
}

// ─── Dump mode: print full results to stdout (no TUI) ───

fn print_dump(app: &BacktestApp) {
    use polymarket_crypto::types::Side;

    println!("╔══════════════════════════════════════════════════════════════════════════════════╗");
    println!("║  BACKTEST RESULTS                                                               ║");
    println!("╚══════════════════════════════════════════════════════════════════════════════════╝");
    println!();

    // ── Portfolio summary ──
    println!("─── PORTFOLIO SUMMARY ─────────────────────────────────────────────────────────────");
    println!("  Bankroll         ${:.0}", app.bankroll);
    println!("  Total PnL        ${:+.2}", app.total_pnl);
    println!("  Total Invested   ${:.2}", app.total_invested);
    println!("  ROI              {:.1}%", app.roi());
    println!("  Win Rate         {:.1}% ({}/{})", app.win_rate() * 100.0, app.n_wins, app.n_wins + app.n_losses);
    println!("  Profit Factor    {:.2}", app.profit_factor());
    println!("  Sharpe Ratio     {:.2}", app.sharpe_ratio());
    println!("  Max Drawdown     ${:.2}", app.max_drawdown());
    println!("  Markets          {}", app.markets.len());
    println!("  Total Trades     {}", app.all_trades.len());

    let wins_pnl: f64 = app.all_trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).sum();
    let loss_pnl: f64 = app.all_trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl).sum();
    let avg_win = if app.n_wins > 0 { wins_pnl / app.n_wins as f64 } else { 0.0 };
    let avg_loss = if app.n_losses > 0 { loss_pnl / app.n_losses as f64 } else { 0.0 };
    println!("  Avg Win          ${:.2}", avg_win);
    println!("  Avg Loss         ${:.2}", avg_loss);
    println!("  Expectancy       ${:.2}/trade", if app.all_trades.is_empty() { 0.0 } else { app.total_pnl / app.all_trades.len() as f64 });
    println!();

    // ── Per-strategy breakdown ──
    println!("─── STRATEGY BREAKDOWN ────────────────────────────────────────────────────────────");
    println!("{:<20} {:>5} {:>5} {:>5} {:>7} {:>9} {:>9} {:>7} {:>7} {:>8} {:>8} {:>8} {:>6}",
        "Strategy", "Trd", "Win", "Loss", "WR%", "PnL", "Invested", "ROI%", "PF", "MaxDD", "Best", "Worst", "AvgT");
    println!("{:-<130}", "");

    let mut strat_names: Vec<&String> = app.strategy_stats.keys().collect();
    strat_names.sort();
    for name in &strat_names {
        let s = &app.strategy_stats[*name];
        println!("{:<20} {:>5} {:>5} {:>5} {:>6.1}% ${:>+8.2} ${:>8.2} {:>6.1}% {:>6.2} ${:>7.2} ${:>+7.2} ${:>+7.2} {:>5.0}s",
            name, s.n_orders, s.n_wins, s.n_losses,
            s.win_rate() * 100.0,
            s.total_pnl, s.total_invested,
            s.roi(), s.profit_factor(), s.max_drawdown(),
            s.max_win, s.max_loss, s.avg_time_left_s);
    }
    println!();

    // ── Per-strategy edge analysis ──
    println!("─── EDGE ANALYSIS ─────────────────────────────────────────────────────────────────");
    println!("{:<20} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "Strategy", "AvgEdge", "AvgConf", "AvgFair", "AvgMkt", "AvgSize", "Passive", "Active");
    println!("{:-<100}", "");
    for name in &strat_names {
        let trades: Vec<_> = app.all_trades.iter().filter(|t| &t.strategy == *name).collect();
        if trades.is_empty() { continue; }
        let n = trades.len() as f64;
        let avg_edge: f64 = trades.iter().map(|t| t.edge).sum::<f64>() / n;
        let avg_conf: f64 = trades.iter().map(|t| t.confidence).sum::<f64>() / n;
        let avg_fair: f64 = trades.iter().map(|t| t.fair_value).sum::<f64>() / n;
        let avg_mkt: f64 = trades.iter().map(|t| t.price).sum::<f64>() / n;
        let avg_size: f64 = trades.iter().map(|t| t.size).sum::<f64>() / n;
        let n_passive = trades.iter().filter(|t| t.is_passive).count();
        let n_active = trades.len() - n_passive;
        println!("{:<20} {:>8.4} {:>8.3} {:>8.4} {:>8.4} ${:>6.1} {:>7} {:>7}",
            name, avg_edge, avg_conf, avg_fair, avg_mkt, avg_size, n_passive, n_active);
    }
    println!();

    // ── Per-market results ──
    println!("─── MARKET RESULTS ────────────────────────────────────────────────────────────────");
    println!("{:>3} {:<30} {:>8} {:>8} {:>7} {:>4} {:>5} {:>5} {:>9} {:>9}  {}",
        "#", "Market", "Strike", "Final", "Dist", "Out", "Trd", "WR%", "Invested", "PnL", "Strategies");
    println!("{:-<130}", "");

    let mut cumulative = 0.0;
    for (i, m) in app.markets.iter().enumerate() {
        let n_trades = m.trades.len();
        let n_wins = m.trades.iter().filter(|t| t.won).count();
        let wr = if n_trades > 0 { n_wins as f64 / n_trades as f64 * 100.0 } else { 0.0 };
        cumulative += m.total_pnl;

        let mut strat_counts: Vec<(String, usize)> = Vec::new();
        for t in &m.trades {
            if let Some(entry) = strat_counts.iter_mut().find(|(s, _)| s == &t.strategy) {
                entry.1 += 1;
            } else {
                strat_counts.push((t.strategy.clone(), 1));
            }
        }
        let strat_str: String = strat_counts.iter()
            .map(|(s, n)| {
                let short = match s.as_str() {
                    "latency_arb" => "LA",
                    "certainty_capture" => "CC",
                    "convexity_fade" => "CF",
                    "cross_timeframe" => "CT",
                    "strike_misalign" => "SM",
                    "lp_extreme" => "LP",
                    _ => "??",
                };
                format!("{}:{}", short, n)
            })
            .collect::<Vec<_>>().join(" ");

        let outcome_str = if m.outcome == Side::Up { "UP" } else { "DN" };

        println!("{:>3} {:<30} ${:>7.0} ${:>7.0} {:>+6.0} {:>4} {:>4} {:>4.0}% ${:>8.2} ${:>+8.2}  {}",
            i + 1, m.dir_name, m.strike, m.final_price, m.final_distance,
            outcome_str, n_trades, wr, m.total_invested, m.total_pnl, strat_str);
    }
    println!("{:-<130}", "");
    println!("{:>3} {:<30} {:>8} {:>8} {:>7} {:>4} {:>5} {:>5} ${:>8.2} ${:>+8.2}",
        "", "TOTAL", "", "", "", "",
        app.all_trades.len(),
        format!("{:.0}%", app.win_rate() * 100.0),
        app.total_invested, app.total_pnl);
    println!();

    // ── Equity curve (text sparkline) ──
    println!("─── EQUITY CURVE ──────────────────────────────────────────────────────────────────");
    let max_pnl = app.equity_curve.iter().map(|e| e.1).fold(f64::MIN, f64::max);
    let min_pnl = app.equity_curve.iter().map(|e| e.1).fold(f64::MAX, f64::min);
    let range = (max_pnl - min_pnl).max(1.0);
    let width = 70;
    let bars = [' ', '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}'];

    let curve_str: String = app.equity_curve.iter().skip(1).map(|&(_, pnl)| {
        let normalized = ((pnl - min_pnl) / range * 8.0).round() as usize;
        bars[normalized.min(8)]
    }).collect();

    println!("  ${:>+7.1} |{}", max_pnl, " ".repeat(width));
    println!("          |{}", curve_str);
    println!("  ${:>+7.1} |{}", min_pnl, " ".repeat(width));
    println!("           mkt 1{:>width$}", format!("mkt {}", app.markets.len()), width = width - 5);
    println!();

    // ── All trades ──
    println!("─── ALL TRADES ────────────────────────────────────────────────────────────────────");
    println!("{:>3} {:>4} {:<18} {:>5} {:>6} {:>6} {:>7} {:>7} {:>5} {:>5} {:>2} {:>8} {:>3} {:>2} {:>9}",
        "Mkt", "#", "Strategy", "Side", "Price", "Size", "Edge", "Fair", "Conf", "T-lft", "T", "BTC", "Out", "W", "PnL");
    println!("{:-<130}", "");

    for t in &app.all_trades {
        let outcome_str = match t.outcome {
            Some(Side::Up) => "UP",
            Some(Side::Down) => "DN",
            _ => "?",
        };
        let won_str = if t.won { "W" } else { "L" };
        println!("{:>3} {:>4} {:<18} {:>5} {:>6.3} ${:>5.1} {:>7.4} {:>7.4} {:>5.2} {:>4.0}s {:>2} ${:>7.0} {:>3} {:>2} ${:>+8.2}",
            t.market_idx + 1, t.order_id, t.strategy,
            format!("{}", t.side), t.price, t.size, t.edge, t.fair_value,
            t.confidence, t.time_left_s,
            if t.is_passive { "P" } else { "A" },
            t.btc_price, outcome_str, won_str, t.pnl);
    }
    println!();

    // ── Win/Loss distribution by time remaining ──
    println!("─── TIMING ANALYSIS ───────────────────────────────────────────────────────────────");
    let buckets = [(0.0, 30.0, "0-30s"), (30.0, 60.0, "30-60s"), (60.0, 120.0, "1-2min"), (120.0, 180.0, "2-3min"), (180.0, 300.0, "3-5min")];
    println!("{:<10} {:>5} {:>5} {:>5} {:>7} {:>9} {:>8}",
        "Time Left", "Trd", "Win", "Loss", "WR%", "PnL", "AvgEdge");
    println!("{:-<60}", "");
    for (lo, hi, label) in &buckets {
        let trades: Vec<_> = app.all_trades.iter()
            .filter(|t| t.time_left_s >= *lo && t.time_left_s < *hi).collect();
        if trades.is_empty() { continue; }
        let n = trades.len();
        let wins = trades.iter().filter(|t| t.won).count();
        let pnl: f64 = trades.iter().map(|t| t.pnl).sum();
        let avg_edge: f64 = trades.iter().map(|t| t.edge).sum::<f64>() / n as f64;
        println!("{:<10} {:>5} {:>5} {:>5} {:>6.1}% ${:>+8.2} {:>8.4}",
            label, n, wins, n - wins,
            wins as f64 / n as f64 * 100.0,
            pnl, avg_edge);
    }
    println!();

    // ── Side analysis ──
    println!("─── SIDE ANALYSIS ─────────────────────────────────────────────────────────────────");
    for side in &[Side::Up, Side::Down] {
        let trades: Vec<_> = app.all_trades.iter().filter(|t| t.side == *side).collect();
        if trades.is_empty() { continue; }
        let n = trades.len();
        let wins = trades.iter().filter(|t| t.won).count();
        let pnl: f64 = trades.iter().map(|t| t.pnl).sum();
        println!("  {:>5}: {} trades, {}/{} wins ({:.0}%), PnL ${:+.2}",
            format!("{}", side), n, wins, n, wins as f64 / n as f64 * 100.0, pnl);
    }

    // Outcome distribution
    let up_outcomes = app.markets.iter().filter(|m| m.outcome == Side::Up).count();
    let dn_outcomes = app.markets.len() - up_outcomes;
    println!("  Market outcomes: {} UP / {} DOWN", up_outcomes, dn_outcomes);
    println!();
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let dump_mode = args.iter().any(|a| a == "--dump");
    let data_dir = args.iter().skip(1).find(|a| !a.starts_with("--"));

    let data_dir = match data_dir {
        Some(d) => d.as_str(),
        None => {
            eprintln!("Usage: backtest [--dump] <data_dir>");
            eprintln!("  e.g. cargo run --bin backtest -- logs/5m");
            eprintln!("  --dump  Print results to stdout instead of TUI");
            std::process::exit(1);
        }
    };

    eprintln!("Discovering markets in {}...", data_dir);
    let market_dirs = engine::discover_markets(data_dir);
    if market_dirs.is_empty() {
        eprintln!("No market data found in {}", data_dir);
        eprintln!("Expected directories containing binance.csv + polymarket.csv");
        std::process::exit(1);
    }
    eprintln!("Found {} market(s)", market_dirs.len());

    eprintln!("Running backtest...");
    let results = engine::run_all_markets(&market_dirs);
    eprintln!("Completed {} market(s)", results.len());

    if results.is_empty() {
        eprintln!("No valid market results. Check your data files.");
        std::process::exit(1);
    }

    let config = engine::backtest_config();
    let app = BacktestApp::new(results, config.bankroll);

    eprintln!(
        "Total PnL: ${:+.2} | {} trades | {:.0}% win rate",
        app.total_pnl,
        app.all_trades.len(),
        app.win_rate() * 100.0,
    );

    if dump_mode {
        print_dump(&app);
        return Ok(());
    }

    eprintln!("Starting TUI...");

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tick_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();
    let mut app = app;

    loop {
        terminal.draw(|frame| render::draw(&app, frame))?;

        let timeout = tick_rate.checked_sub(last_tick.elapsed()).unwrap_or(Duration::ZERO);

        if crossterm::event::poll(timeout)? {
            if let CEvent::Key(key) = event::read()? {
                if handle_key(&mut app, key) {
                    break;
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
