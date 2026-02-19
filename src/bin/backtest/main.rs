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
//!   [Tab/1-8] Switch tab  [j/k] Scroll  [f] Filter trades  [Enter] Drill-down  [Esc/Backspace] Back  [q] Quit

mod engine;
mod render;
mod types;

use std::io::{self, stdout};
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event as CEvent, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;

use crate::types::{BacktestApp, Tab};

fn handle_key(app: &mut BacktestApp, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Char('q') => return true,

        // Tab switching
        KeyCode::Tab => app.tab = app.tab.next(),
        KeyCode::BackTab => app.tab = app.tab.prev(),
        KeyCode::Char('1') => app.tab = Tab::Summary,
        KeyCode::Char('2') => app.tab = Tab::Strategies,
        KeyCode::Char('3') => app.tab = Tab::Markets,
        KeyCode::Char('4') => app.tab = Tab::Trades,
        KeyCode::Char('5') => app.tab = Tab::Equity,
        KeyCode::Char('6') => app.tab = Tab::Risk,
        KeyCode::Char('7') => app.tab = Tab::Timing,
        KeyCode::Char('8') => app.tab = Tab::Correlation,

        // Esc: back out of drill-down or quit
        KeyCode::Esc | KeyCode::Backspace => {
            if app.market_drill_down.is_some() {
                app.market_drill_down = None;
            } else {
                return true;
            }
        }

        // Enter: drill-down into selected market
        KeyCode::Enter => {
            match app.tab {
                Tab::Markets => {
                    if app.market_scroll < app.markets.len() {
                        app.market_drill_down = Some(app.market_scroll);
                    }
                }
                _ => {}
            }
        }

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

    println!("{}",
        "\u{2554}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2557}");
    println!("\u{2551}  BACKTEST RESULTS{}\u{2551}",
        " ".repeat(63));
    println!("{}",
        "\u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}");
    println!();

    // ── Portfolio summary ──
    println!("\u{2500}\u{2500}\u{2500} PORTFOLIO SUMMARY \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}");
    println!("  Bankroll         ${:.0}", app.bankroll);
    println!("  Total PnL        ${:+.2}", app.total_pnl);
    println!("  Total Invested   ${:.2}", app.total_invested);
    println!("  ROI              {:.1}%", app.roi());
    println!("  Win Rate         {:.1}% ({}/{})", app.win_rate() * 100.0, app.n_wins, app.n_wins + app.n_losses);
    println!("  Profit Factor    {:.2}", app.profit_factor());
    println!("  Sharpe Ratio     {:.2}", app.sharpe_ratio());
    println!("  Sortino Ratio    {:.2}", app.sortino_ratio);
    println!("  Calmar Ratio     {:.2}", app.calmar_ratio);
    println!("  Max Drawdown     ${:.2}", app.max_drawdown());
    println!("  Max DD Duration  {} trades", app.max_drawdown_duration);
    println!("  Recovery Factor  {:.2}", app.recovery_factor);
    println!("  Markets          {}", app.markets.len());
    println!("  Total Trades     {}", app.all_trades.len());
    println!("  Avg Win          ${:.2}", app.avg_win);
    println!("  Avg Loss         ${:.2}", app.avg_loss);
    println!("  Payoff Ratio     {:.2}", app.payoff_ratio);
    println!("  Expectancy       ${:.2}/trade", app.expectancy);
    println!("  Kelly Fraction   {:.1}%", app.kelly_fraction * 100.0);
    println!("  Consec Wins      {}", app.max_consecutive_wins);
    println!("  Consec Losses    {}", app.max_consecutive_losses);
    println!();

    // ── Per-strategy breakdown ──
    println!("\u{2500}\u{2500}\u{2500} STRATEGY BREAKDOWN \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}");
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
    println!("\u{2500}\u{2500}\u{2500} EDGE ANALYSIS \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}");
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
    println!("\u{2500}\u{2500}\u{2500} MARKET RESULTS \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}");
    println!("{:>3} {:<30} {:>8} {:>8} {:>7} {:>4} {:>5} {:>5} {:>9} {:>9}  {}",
        "#", "Market", "Strike", "Final", "Dist", "Out", "Trd", "WR%", "Invested", "PnL", "Strategies");
    println!("{:-<130}", "");

    for (i, m) in app.markets.iter().enumerate() {
        let n_trades = m.trades.len();
        let n_wins = m.trades.iter().filter(|t| t.won).count();
        let wr = if n_trades > 0 { n_wins as f64 / n_trades as f64 * 100.0 } else { 0.0 };

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
    println!("\u{2500}\u{2500}\u{2500} EQUITY CURVE \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}");
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
    println!("\u{2500}\u{2500}\u{2500} ALL TRADES \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}");
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
    println!("\u{2500}\u{2500}\u{2500} TIMING ANALYSIS \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}");
    println!("{:<10} {:>5} {:>5} {:>5} {:>7} {:>9} {:>8}",
        "Time Left", "Trd", "Win", "Loss", "WR%", "PnL", "AvgEdge");
    println!("{:-<60}", "");
    for bucket in &app.time_buckets {
        if bucket.n_trades == 0 { continue; }
        println!("{:<10} {:>5} {:>5} {:>5} {:>6.1}% ${:>+8.2} {:>8.4}",
            bucket.label, bucket.n_trades, bucket.n_wins, bucket.n_trades - bucket.n_wins,
            if bucket.n_trades > 0 { bucket.n_wins as f64 / bucket.n_trades as f64 * 100.0 } else { 0.0 },
            bucket.total_pnl, bucket.avg_edge);
    }
    println!();

    // ── Side analysis ──
    println!("\u{2500}\u{2500}\u{2500} SIDE ANALYSIS \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}");
    println!("  UP:   {} trades, PnL ${:+.2}", app.up_trades, app.up_pnl);
    println!("  DOWN: {} trades, PnL ${:+.2}", app.dn_trades, app.dn_pnl);
    println!("  Market outcomes: {} UP / {} DOWN", app.up_outcomes, app.dn_outcomes);
    println!();

    // ── Adverse selection analysis (fill-to-lose) ──
    println!("\u{2500}\u{2500}\u{2500} ADVERSE SELECTION ANALYSIS \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}");
    println!("  Do winning and losing trades differ at signal time?");
    println!();
    println!("{:<20} {:>7} {:>7} {:>7} {:>7} {:>9} {:>9} {:>9} {:>9}",
        "Strategy", "W_Edge", "L_Edge", "W_Sig", "L_Sig", "W_|z|", "L_|z|", "W_Dist", "L_Dist");
    println!("{:-<100}", "");
    for name in &strat_names {
        let wins: Vec<_> = app.all_trades.iter().filter(|t| &t.strategy == *name && t.won).collect();
        let losses: Vec<_> = app.all_trades.iter().filter(|t| &t.strategy == *name && !t.won).collect();
        if wins.is_empty() && losses.is_empty() { continue; }

        let avg_or = |trades: &[&types::TradeRecord], f: fn(&&types::TradeRecord) -> f64| -> f64 {
            if trades.is_empty() { 0.0 } else { trades.iter().map(f).sum::<f64>() / trades.len() as f64 }
        };

        let w_edge = avg_or(&wins, |t| t.edge);
        let l_edge = avg_or(&losses, |t| t.edge);
        let w_sigma = avg_or(&wins, |t| t.sigma_at_signal);
        let l_sigma = avg_or(&losses, |t| t.sigma_at_signal);
        let w_z = avg_or(&wins, |t| t.z_at_signal.abs());
        let l_z = avg_or(&losses, |t| t.z_at_signal.abs());
        let w_dist = avg_or(&wins, |t| t.distance_at_signal);
        let l_dist = avg_or(&losses, |t| t.distance_at_signal);

        println!("{:<20} {:>7.4} {:>7.4} {:>7.5} {:>7.5} {:>9.3} {:>9.3} ${:>7.0} ${:>7.0}",
            name, w_edge, l_edge, w_sigma, l_sigma, w_z, l_z, w_dist, l_dist);
    }
    println!();

    // Per-market fill quality
    println!("  Per-market fill quality:");
    println!("{:>3} {:<30} {:>5} {:>5} {:>8} {:>8} {:>8}",
        "#", "Market", "W", "L", "AvgEdge", "AvgSig", "Avg|z|");
    println!("{:-<80}", "");
    for (i, m) in app.markets.iter().enumerate() {
        if m.trades.is_empty() { continue; }
        let n = m.trades.len() as f64;
        let wins = m.trades.iter().filter(|t| t.won).count();
        let losses = m.trades.len() - wins;
        let avg_edge = m.trades.iter().map(|t| t.edge).sum::<f64>() / n;
        let avg_sigma = m.trades.iter().map(|t| t.sigma_at_signal).sum::<f64>() / n;
        let avg_z = m.trades.iter().map(|t| t.z_at_signal.abs()).sum::<f64>() / n;
        println!("{:>3} {:<30} {:>5} {:>5} {:>8.4} {:>8.5} {:>8.3}",
            i + 1, m.dir_name, wins, losses, avg_edge, avg_sigma, avg_z);
    }
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
