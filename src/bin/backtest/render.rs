use ratatui::prelude::*;
use ratatui::widgets::*;

use polymarket_crypto::types::Side;

use crate::types::{BacktestApp, Tab};

// ─── Colors ───

const BORDER: Style = Style::new().fg(Color::DarkGray);
const GREEN: Color = Color::Green;
const RED: Color = Color::Red;
const YELLOW: Color = Color::Yellow;
const CYAN: Color = Color::Cyan;
const WHITE: Color = Color::White;
const GRAY: Color = Color::DarkGray;
const MAGENTA: Color = Color::Magenta;

fn pnl_color(pnl: f64) -> Color {
    if pnl > 0.0 { GREEN } else if pnl < 0.0 { RED } else { WHITE }
}

fn strategy_color(name: &str) -> Color {
    match name {
        "latency_arb" => YELLOW,
        "certainty_capture" => Color::LightCyan,
        "convexity_fade" => Color::LightMagenta,
        "cross_timeframe" => Color::LightBlue,
        "strike_misalign" => Color::LightRed,
        "lp_extreme" => Color::LightGreen,
        _ => WHITE,
    }
}

fn strategy_short(name: &str) -> &'static str {
    match name {
        "latency_arb" => "LA",
        "certainty_capture" => "CC",
        "convexity_fade" => "CF",
        "cross_timeframe" => "CT",
        "strike_misalign" => "SM",
        "lp_extreme" => "LP",
        _ => "??",
    }
}

/// Heatmap-style color for correlation values (-1..+1)
fn corr_color(v: f64) -> Color {
    if v > 0.7 { Color::LightGreen }
    else if v > 0.3 { GREEN }
    else if v > -0.3 { WHITE }
    else if v > -0.7 { Color::LightRed }
    else { RED }
}

/// Color gradient for win rate (0..100)
fn wr_color(wr: f64) -> Color {
    if wr >= 70.0 { Color::LightGreen }
    else if wr >= 50.0 { GREEN }
    else if wr >= 40.0 { YELLOW }
    else { RED }
}

// ─── Main draw ───

pub fn draw(app: &BacktestApp, frame: &mut Frame) {
    let [header_area, tabs_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(10),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_header(app, header_area, frame.buffer_mut());
    render_tabs(app, tabs_area, frame.buffer_mut());
    render_footer(app, footer_area, frame.buffer_mut());

    // Market drill-down overrides the Markets tab
    if app.tab == Tab::Markets && app.market_drill_down.is_some() {
        render_market_drill_down(app, body_area, frame.buffer_mut());
        return;
    }

    match app.tab {
        Tab::Summary => render_summary(app, body_area, frame.buffer_mut()),
        Tab::Strategies => render_strategies(app, body_area, frame.buffer_mut()),
        Tab::Markets => render_markets(app, body_area, frame.buffer_mut()),
        Tab::Trades => render_trades(app, body_area, frame.buffer_mut()),
        Tab::Equity => render_equity(app, body_area, frame.buffer_mut()),
        Tab::Risk => render_risk(app, body_area, frame.buffer_mut()),
        Tab::Timing => render_timing(app, body_area, frame.buffer_mut()),
        Tab::Correlation => render_correlation(app, body_area, frame.buffer_mut()),
    }
}

// ─── Header ───

fn render_header(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let pnl = app.total_pnl;
    let pnl_sign = if pnl >= 0.0 { "+" } else { "" };
    let text = format!(
        " BACKTEST | {} mkts | {} trades | PnL {}{:.2} | WR {:.0}% | Sharpe {:.2} | Sortino {:.2} | PF {:.2} | MDD ${:.1} | Kelly {:.0}% | ${:.0}",
        app.markets.len(),
        app.all_trades.len(),
        pnl_sign, pnl,
        app.win_rate() * 100.0,
        app.sharpe_ratio(),
        app.sortino_ratio,
        app.profit_factor(),
        app.max_drawdown(),
        app.kelly_fraction * 100.0,
        app.bankroll,
    );
    Paragraph::new(text)
        .style(Style::default().fg(Color::Black).bg(if pnl >= 0.0 { GREEN } else { RED }))
        .render(area, buf);
}

// ─── Tabs bar ───

fn render_tabs(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let titles: Vec<Span> = Tab::all().iter().enumerate().map(|(i, t)| {
        let style = if *t == app.tab {
            Style::default().fg(Color::Black).bg(CYAN).bold()
        } else {
            Style::default().fg(WHITE).bg(Color::Reset)
        };
        Span::styled(format!(" {}{} ", i + 1, t.label()), style)
    }).collect();

    let mut line_spans = Vec::new();
    for (i, title) in titles.into_iter().enumerate() {
        if i > 0 {
            line_spans.push(Span::styled("\u{2502}", Style::default().fg(GRAY)));
        }
        line_spans.push(title);
    }

    Paragraph::new(Line::from(line_spans)).render(area, buf);
}

// ─── Footer ───

fn render_footer(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let base = " [1-8] Tab  [j/k] Scroll  [PgUp/Dn] Page";
    let extra = match app.tab {
        Tab::Trades => "  [f] Filter",
        Tab::Markets if app.market_drill_down.is_some() => "  [Esc] Back",
        Tab::Markets => "  [Enter] Drill-down",
        _ => "",
    };
    let text = format!("{}{}  [q] Quit", base, extra);
    Paragraph::new(text)
        .style(Style::default().fg(Color::Black).bg(GRAY))
        .render(area, buf);
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab 1: Summary Dashboard
// ─────────────────────────────────────────────────────────────────────────────

fn render_summary(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let [top_area, bottom_area] = Layout::vertical([
        Constraint::Min(18),
        Constraint::Length(12),
    ])
    .areas(area);

    let [left_area, right_area] = Layout::horizontal([
        Constraint::Length(48),
        Constraint::Min(30),
    ])
    .areas(top_area);

    let [stats_area, side_area] = Layout::vertical([
        Constraint::Min(14),
        Constraint::Length(6),
    ])
    .areas(left_area);

    render_summary_stats(app, stats_area, buf);
    render_side_summary(app, side_area, buf);

    let [breakdown_area, edge_area] = Layout::vertical([
        Constraint::Min(8),
        Constraint::Length(8),
    ])
    .areas(right_area);

    render_strategy_pnl_bars(app, breakdown_area, buf);
    render_edge_comparison(app, edge_area, buf);

    render_mini_equity(app, bottom_area, buf);
}

fn render_summary_stats(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let pnl = app.total_pnl;
    let wr = app.win_rate() * 100.0;
    let roi = app.roi();
    let pf = app.profit_factor();
    let sharpe = app.sharpe_ratio();
    let mdd = app.max_drawdown();

    let lines = vec![
        Line::from(""),
        kv_line("  Total PnL", format!("${:+.2}", pnl), pnl_color(pnl)),
        kv_line("  ROI", format!("{:.1}%", roi), pnl_color(roi)),
        kv_line("  Win Rate", format!("{:.1}% ({}/{})", wr, app.n_wins, app.n_wins + app.n_losses), wr_color(wr)),
        kv_line("  Profit Factor", format!("{:.2}", pf), if pf > 1.0 { GREEN } else { RED }),
        Line::from(""),
        kv_line("  Sharpe", format!("{:.2}", sharpe), if sharpe > 0.5 { GREEN } else if sharpe > 0.0 { YELLOW } else { RED }),
        kv_line("  Sortino", format!("{:.2}", app.sortino_ratio), if app.sortino_ratio > 0.5 { GREEN } else { YELLOW }),
        kv_line("  Calmar", format!("{:.2}", app.calmar_ratio), if app.calmar_ratio > 1.0 { GREEN } else { YELLOW }),
        kv_line("  Max Drawdown", format!("${:.2} ({} trades)", mdd, app.max_drawdown_duration), RED),
        Line::from(""),
        kv_line("  Avg Win / Loss", format!("${:.2} / ${:.2}", app.avg_win, app.avg_loss), WHITE),
        kv_line("  Payoff Ratio", format!("{:.2}", app.payoff_ratio), if app.payoff_ratio > 1.0 { GREEN } else { RED }),
        kv_line("  Expectancy", format!("${:.3}/trade", app.expectancy), pnl_color(app.expectancy)),
        kv_line("  Kelly f*", format!("{:.1}%", app.kelly_fraction * 100.0), CYAN),
    ];

    Widget::render(
        Paragraph::new(lines).block(Block::bordered().title("Performance").border_style(BORDER)),
        area, buf,
    );
}

fn render_side_summary(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let lines = vec![
        Line::from(vec![
            Span::styled("  UP  ", Style::default().fg(GREEN).bold()),
            Span::styled(format!("{} trades  PnL ${:+.2}  ", app.up_trades, app.up_pnl), Style::default().fg(if app.up_pnl >= 0.0 { GREEN } else { RED })),
            Span::styled(format!("outcomes {}", app.up_outcomes), Style::default().fg(GRAY)),
        ]),
        Line::from(vec![
            Span::styled("  DN  ", Style::default().fg(RED).bold()),
            Span::styled(format!("{} trades  PnL ${:+.2}  ", app.dn_trades, app.dn_pnl), Style::default().fg(if app.dn_pnl >= 0.0 { GREEN } else { RED })),
            Span::styled(format!("outcomes {}", app.dn_outcomes), Style::default().fg(GRAY)),
        ]),
        Line::from(vec![
            Span::styled("  Streaks  ", Style::default().fg(GRAY)),
            Span::styled(format!("W:{}", app.max_consecutive_wins), Style::default().fg(GREEN)),
            Span::styled(" / ", Style::default().fg(GRAY)),
            Span::styled(format!("L:{}", app.max_consecutive_losses), Style::default().fg(RED)),
        ]),
    ];
    Widget::render(
        Paragraph::new(lines).block(Block::bordered().title("Directional").border_style(BORDER)),
        area, buf,
    );
}

fn kv_line(label: &str, value: String, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<22}", label), Style::default().fg(GRAY)),
        Span::styled(value, Style::default().fg(color)),
    ])
}

fn render_strategy_pnl_bars(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let mut strat_pnls: Vec<(&str, f64, f64, u32, Color)> = Vec::new();
    let mut strat_names: Vec<&String> = app.strategy_stats.keys().collect();
    strat_names.sort();

    for name in &strat_names {
        let stats = &app.strategy_stats[*name];
        strat_pnls.push((name.as_str(), stats.total_pnl, stats.win_rate() * 100.0, stats.n_orders, strategy_color(name)));
    }

    let max_abs = strat_pnls.iter().map(|s| s.1.abs()).fold(1.0f64, f64::max);

    let mut rows: Vec<Row> = Vec::new();
    for (name, pnl, wr, n, color) in &strat_pnls {
        let bar_width = (area.width as f64 * 0.3) as usize;
        let filled = ((pnl.abs() / max_abs) * bar_width as f64) as usize;
        let bar = if *pnl >= 0.0 {
            format!("{}{}", "\u{2588}".repeat(filled), " ".repeat(bar_width.saturating_sub(filled)))
        } else {
            format!("{}{}", " ".repeat(bar_width.saturating_sub(filled)), "\u{2588}".repeat(filled))
        };

        rows.push(Row::new(vec![
            Cell::from(format!("{:<2}", strategy_short(name))).style(Style::default().fg(*color)),
            Cell::from(format!("{:>3}", n)).style(Style::default().fg(WHITE)),
            Cell::from(format!("{:>5.0}%", wr)).style(Style::default().fg(wr_color(*wr))),
            Cell::from(format!("{:>+7.2}", pnl)).style(Style::default().fg(pnl_color(*pnl))),
            Cell::from(bar).style(Style::default().fg(pnl_color(*pnl))),
        ]));
    }

    let widths = [
        Constraint::Length(3),
        Constraint::Length(4),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Min(10),
    ];
    let header = Row::new(vec!["St", "#", "WR", "PnL", ""])
        .style(Style::default().fg(CYAN).bold());

    Widget::render(
        Table::new(rows, widths).header(header)
            .block(Block::bordered().title("Strategy PnL").border_style(BORDER)),
        area, buf,
    );
}

fn render_edge_comparison(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let mut strat_names: Vec<&String> = app.strategy_stats.keys().collect();
    strat_names.sort();

    let mut rows: Vec<Row> = Vec::new();
    for name in &strat_names {
        let predicted = app.edge_predicted.get(*name).copied().unwrap_or(0.0);
        let realized = app.edge_realized.get(*name).copied().unwrap_or(0.0);
        let ratio = if predicted != 0.0 { realized / predicted } else { 0.0 };
        let ratio_color = if ratio > 0.8 { GREEN } else if ratio > 0.0 { YELLOW } else { RED };

        rows.push(Row::new(vec![
            Cell::from(format!("{:<2}", strategy_short(name))).style(Style::default().fg(strategy_color(name))),
            Cell::from(format!("{:.4}", predicted)).style(Style::default().fg(YELLOW)),
            Cell::from(format!("{:+.4}", realized)).style(Style::default().fg(pnl_color(realized))),
            Cell::from(format!("{:.0}%", ratio * 100.0)).style(Style::default().fg(ratio_color)),
        ]));
    }

    let widths = [Constraint::Length(3), Constraint::Length(7), Constraint::Length(7), Constraint::Length(6)];
    let header = Row::new(vec!["St", "Pred", "Real", "Capt"])
        .style(Style::default().fg(CYAN).bold());

    Widget::render(
        Table::new(rows, widths).header(header)
            .block(Block::bordered().title("Edge Capture").border_style(BORDER)),
        area, buf,
    );
}

fn render_mini_equity(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    if app.trade_equity_curve.len() < 2 {
        Widget::render(Block::bordered().title("Equity Curve (per trade)").border_style(BORDER), area, buf);
        return;
    }

    let data: Vec<(f64, f64)> = app.trade_equity_curve.clone();
    let x_min = data.first().map(|d| d.0).unwrap_or(0.0);
    let x_max = data.last().map(|d| d.0).unwrap_or(1.0);
    let y_min = data.iter().map(|d| d.1).fold(f64::MAX, f64::min) - 1.0;
    let y_max = data.iter().map(|d| d.1).fold(f64::MIN, f64::max) + 1.0;

    let zero_line = vec![(x_min, 0.0), (x_max, 0.0)];

    let datasets = vec![
        Dataset::default().name("PnL").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(if app.total_pnl >= 0.0 { GREEN } else { RED })).data(&data),
        Dataset::default().name("Zero").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(GRAY)).data(&zero_line),
    ];

    let chart = Chart::new(datasets)
        .block(Block::bordered().title(format!("Equity Curve ({} trades)", app.all_trades.len())).border_style(BORDER))
        .x_axis(Axis::default().bounds([x_min, x_max])
            .labels::<Vec<Line>>(vec!["0".into(), format!("{}", app.all_trades.len()).into()]).style(BORDER))
        .y_axis(Axis::default().bounds([y_min, y_max])
            .labels::<Vec<Line>>(vec![
                format!("{:.0}", y_min).into(),
                "0".into(),
                format!("{:.0}", y_max).into(),
            ]).style(BORDER));

    Widget::render(chart, area, buf);
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab 2: Strategies (detailed per-strategy)
// ─────────────────────────────────────────────────────────────────────────────

fn render_strategies(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let [table_area, detail_area] = Layout::vertical([
        Constraint::Min(10),
        Constraint::Length(14),
    ])
    .areas(area);

    let mut strat_names: Vec<&String> = app.strategy_stats.keys().collect();
    strat_names.sort();

    let rows: Vec<Row> = strat_names.iter().enumerate().map(|(i, name)| {
        let stats = &app.strategy_stats[*name];
        let highlight = if i == app.strategy_scroll { Style::default().bg(Color::DarkGray) } else { Style::default() };
        Row::new(vec![
            Cell::from(format!("{:<2}", strategy_short(name))).style(Style::default().fg(strategy_color(name))),
            Cell::from(name.as_str()),
            Cell::from(format!("{}", stats.n_orders)),
            Cell::from(format!("{}", stats.n_wins)).style(Style::default().fg(GREEN)),
            Cell::from(format!("{}", stats.n_losses)).style(Style::default().fg(RED)),
            Cell::from(format!("{:.1}%", stats.win_rate() * 100.0)).style(Style::default().fg(wr_color(stats.win_rate() * 100.0))),
            Cell::from(format!("${:+.2}", stats.total_pnl)).style(Style::default().fg(pnl_color(stats.total_pnl))),
            Cell::from(format!("{:.1}%", stats.roi())).style(Style::default().fg(pnl_color(stats.roi()))),
            Cell::from(format!("{:.2}", stats.profit_factor())).style(Style::default().fg(if stats.profit_factor() > 1.0 { GREEN } else { RED })),
            Cell::from(format!("{:.2}", stats.sortino())).style(Style::default().fg(if stats.sortino() > 0.0 { GREEN } else { RED })),
            Cell::from(format!("${:.2}", stats.max_drawdown())).style(Style::default().fg(RED)),
            Cell::from(format!("{:.4}", stats.avg_edge())).style(Style::default().fg(YELLOW)),
            Cell::from(format!("{:.2}", stats.avg_confidence())),
            Cell::from(format!("{}/{}",stats.n_passive, stats.n_active)).style(Style::default().fg(GRAY)),
            Cell::from(format!("{:.0}s", stats.avg_time_left_s)),
        ]).style(highlight)
    }).collect();

    let widths = [
        Constraint::Length(3), Constraint::Length(18), Constraint::Length(4),
        Constraint::Length(4), Constraint::Length(4), Constraint::Length(6),
        Constraint::Length(9), Constraint::Length(6),
        Constraint::Length(6), Constraint::Length(7), Constraint::Length(7),
        Constraint::Length(7), Constraint::Length(5), Constraint::Length(5),
        Constraint::Length(5),
    ];
    let header = Row::new(vec![
        "", "Strategy", "Trd", "W", "L", "WR%", "PnL", "ROI", "PF", "Sortino", "MaxDD", "Edge", "Conf", "P/A", "AvgT",
    ]).style(Style::default().fg(CYAN).bold());

    Widget::render(
        Table::new(rows, widths).header(header)
            .block(Block::bordered().title("Strategy Performance").border_style(BORDER)),
        table_area, buf,
    );

    if let Some(name) = strat_names.get(app.strategy_scroll) {
        render_strategy_detail(app, name, detail_area, buf);
    }
}

fn render_strategy_detail(app: &BacktestApp, name: &str, area: Rect, buf: &mut Buffer) {
    let stats = match app.strategy_stats.get(name) {
        Some(s) => s,
        None => return,
    };

    let [left, mid, right] = Layout::horizontal([
        Constraint::Percentage(33),
        Constraint::Percentage(33),
        Constraint::Percentage(34),
    ])
    .areas(area);

    // Left: per-market breakdown
    let mut market_pnls: Vec<(usize, &str, f64, u32)> = Vec::new();
    for (mi, market) in app.markets.iter().enumerate() {
        let trades: Vec<_> = market.trades.iter().filter(|t| t.strategy == name).collect();
        if !trades.is_empty() {
            let pnl: f64 = trades.iter().map(|t| t.pnl).sum();
            market_pnls.push((mi, &market.dir_name, pnl, trades.len() as u32));
        }
    }

    let rows: Vec<Row> = market_pnls.iter().map(|(_, dir, pnl, n)| {
        let short_name: String = dir.chars().rev().take(20).collect::<String>().chars().rev().collect();
        Row::new(vec![
            Cell::from(short_name),
            Cell::from(format!("{}", n)),
            Cell::from(format!("${:+.2}", pnl)).style(Style::default().fg(pnl_color(*pnl))),
        ])
    }).collect();

    let widths = [Constraint::Min(10), Constraint::Length(3), Constraint::Length(8)];
    let header = Row::new(vec!["Market", "#", "PnL"]).style(Style::default().fg(CYAN).bold());

    Widget::render(
        Table::new(rows, widths).header(header)
            .block(Block::bordered().title(format!("{} markets", strategy_short(name))).border_style(BORDER)),
        left, buf,
    );

    // Mid: detailed metrics
    let lines = vec![
        kv_line_short("  Best", format!("${:+.2}", stats.max_win), GREEN),
        kv_line_short("  Worst", format!("${:+.2}", stats.max_loss), RED),
        kv_line_short("  AvgSize", format!("${:.1}", stats.avg_size()), WHITE),
        kv_line_short("  AvgPrice", format!("{:.3}", stats.avg_price()), WHITE),
        kv_line_short("  EdgeStd", format!("{:.4}", stats.edge_std()), YELLOW),
        kv_line_short("  ConsW", format!("{}", stats.max_consecutive_wins()), GREEN),
        kv_line_short("  ConsL", format!("{}", stats.max_consecutive_losses()), RED),
        kv_line_short("  Invested", format!("${:.1}", stats.total_invested), GRAY),
    ];
    Widget::render(
        Paragraph::new(lines).block(Block::bordered().title(format!("{} detail", strategy_short(name))).border_style(BORDER)),
        mid, buf,
    );

    // Right: strategy equity curve
    if stats.pnl_history.len() >= 2 {
        let data: Vec<(f64, f64)> = stats.pnl_history.iter().enumerate()
            .map(|(i, &pnl)| (i as f64, pnl)).collect();
        let x_max = data.last().map(|d| d.0).unwrap_or(1.0);
        let y_min = data.iter().map(|d| d.1).fold(f64::MAX, f64::min) - 0.5;
        let y_max = data.iter().map(|d| d.1).fold(f64::MIN, f64::max) + 0.5;
        let zero_line = vec![(0.0, 0.0), (x_max, 0.0)];

        let datasets = vec![
            Dataset::default().name(strategy_short(name)).marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line).style(Style::default().fg(strategy_color(name))).data(&data),
            Dataset::default().name("0").marker(symbols::Marker::Braille)
                .graph_type(GraphType::Line).style(Style::default().fg(GRAY)).data(&zero_line),
        ];
        let chart = Chart::new(datasets)
            .block(Block::bordered().title(format!("{} equity", name)).border_style(BORDER))
            .x_axis(Axis::default().bounds([0.0, x_max]).style(BORDER))
            .y_axis(Axis::default().bounds([y_min, y_max])
                .labels::<Vec<Line>>(vec![format!("{:.1}", y_min).into(), "0".into(), format!("{:.1}", y_max).into()])
                .style(BORDER));
        Widget::render(chart, right, buf);
    } else {
        Widget::render(
            Block::bordered().title(format!("{} equity", name)).border_style(BORDER),
            right, buf,
        );
    }
}

fn kv_line_short(label: &str, value: String, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<12}", label), Style::default().fg(GRAY)),
        Span::styled(value, Style::default().fg(color)),
    ])
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab 3: Markets
// ─────────────────────────────────────────────────────────────────────────────

fn render_markets(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let visible_height = area.height.saturating_sub(4) as usize;
    let start = app.market_scroll;
    let end = (start + visible_height).min(app.markets.len());

    let rows: Vec<Row> = app.markets[start..end].iter().enumerate().map(|(i, m)| {
        let outcome_str = if m.outcome == Side::Up { "UP" } else { "DN" };
        let outcome_color = if m.outcome == Side::Up { GREEN } else { RED };
        let n_trades = m.trades.len();
        let n_wins = m.trades.iter().filter(|t| t.won).count();
        let wr = if n_trades > 0 { n_wins as f64 / n_trades as f64 * 100.0 } else { 0.0 };

        let strat_summary: String = {
            let mut counts: Vec<(String, usize)> = Vec::new();
            for t in &m.trades {
                if let Some(entry) = counts.iter_mut().find(|(s, _)| s == &t.strategy) {
                    entry.1 += 1;
                } else {
                    counts.push((t.strategy.clone(), 1));
                }
            }
            counts.iter().map(|(s, n)| format!("{}:{}", strategy_short(s), n)).collect::<Vec<_>>().join(" ")
        };

        let is_selected = start + i == app.market_scroll;
        let highlight = if is_selected { Style::default().bg(Color::DarkGray) } else { Style::default() };
        Row::new(vec![
            Cell::from(format!("{}", start + i + 1)).style(Style::default().fg(GRAY)),
            Cell::from(m.dir_name.chars().rev().take(28).collect::<String>().chars().rev().collect::<String>()),
            Cell::from(format!("${:.0}", m.strike)),
            Cell::from(format!("${:.0}", m.final_price)),
            Cell::from(format!("{:+.0}", m.final_distance)).style(Style::default().fg(if m.final_distance >= 0.0 { GREEN } else { RED })),
            Cell::from(outcome_str).style(Style::default().fg(outcome_color)),
            Cell::from(format!("{}", m.n_events)).style(Style::default().fg(GRAY)),
            Cell::from(format!("{}", n_trades)),
            Cell::from(format!("{:.0}%", wr)).style(Style::default().fg(wr_color(wr))),
            Cell::from(format!("${:.2}", m.total_invested)),
            Cell::from(format!("${:+.2}", m.total_pnl)).style(Style::default().fg(pnl_color(m.total_pnl))),
            Cell::from(strat_summary).style(Style::default().fg(GRAY)),
        ]).style(highlight)
    }).collect();

    let widths = [
        Constraint::Length(3), Constraint::Length(28), Constraint::Length(8),
        Constraint::Length(8), Constraint::Length(6), Constraint::Length(4),
        Constraint::Length(6), Constraint::Length(4), Constraint::Length(5),
        Constraint::Length(9), Constraint::Length(9), Constraint::Min(15),
    ];
    let header = Row::new(vec![
        "#", "Market", "Strike", "Final", "Dist", "Out", "Events", "Trd", "WR", "Invested", "PnL", "Strategies",
    ]).style(Style::default().fg(CYAN).bold());

    Widget::render(
        Table::new(rows, widths).header(header)
            .block(Block::bordered().title(format!("Markets ({}) [j/k] scroll  [Enter] drill-down", app.markets.len())).border_style(BORDER)),
        area, buf,
    );
}

// ─── Market Drill-Down ───

fn render_market_drill_down(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let mi = match app.market_drill_down {
        Some(idx) if idx < app.markets.len() => idx,
        _ => return,
    };
    let market = &app.markets[mi];

    let [header_area, body_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
    ])
    .areas(area);

    // Header: market summary
    let outcome_str = if market.outcome == Side::Up { "UP" } else { "DN" };
    let n_wins = market.trades.iter().filter(|t| t.won).count();
    let wr = if market.trades.is_empty() { 0.0 } else { n_wins as f64 / market.trades.len() as f64 * 100.0 };
    let info = format!(
        " Market #{} | {} | K=${:.0} | Final=${:.0} | Dist={:+.0} | {} | {} trades | WR {:.0}% | PnL ${:+.2}",
        mi + 1, market.dir_name, market.strike, market.final_price, market.final_distance,
        outcome_str, market.trades.len(), wr, market.total_pnl,
    );
    Widget::render(
        Paragraph::new(info).style(Style::default().fg(WHITE)).block(Block::bordered().title("Market Detail [Esc] back").border_style(BORDER)),
        header_area, buf,
    );

    let [trades_area, chart_area] = Layout::vertical([
        Constraint::Min(8),
        Constraint::Length(10),
    ])
    .areas(body_area);

    // Trade table for this market
    let rows: Vec<Row> = market.trades.iter().map(|t| {
        let won_color = if t.won { GREEN } else { RED };
        Row::new(vec![
            Cell::from(format!("#{}", t.order_id)).style(Style::default().fg(GRAY)),
            Cell::from(format!("{:<2}", strategy_short(&t.strategy))).style(Style::default().fg(strategy_color(&t.strategy))),
            Cell::from(format!("{}", t.side)).style(Style::default().fg(if t.side == Side::Up { GREEN } else { RED })),
            Cell::from(format!("{:.3}", t.price)),
            Cell::from(format!("${:.1}", t.size)).style(Style::default().fg(YELLOW)),
            Cell::from(format!("{:.4}", t.edge)).style(Style::default().fg(YELLOW)),
            Cell::from(format!("{:.3}", t.fair_value)),
            Cell::from(format!("{:.2}", t.confidence)),
            Cell::from(format!("{:.0}s", t.time_left_s)),
            Cell::from(if t.is_passive { "P" } else { "A" }),
            Cell::from(format!("${:.0}", t.btc_price)),
            Cell::from(if t.won { "W" } else { "L" }).style(Style::default().fg(won_color).bold()),
            Cell::from(format!("${:+.2}", t.pnl)).style(Style::default().fg(pnl_color(t.pnl))),
        ])
    }).collect();

    let widths = [
        Constraint::Length(4), Constraint::Length(3), Constraint::Length(5),
        Constraint::Length(6), Constraint::Length(6), Constraint::Length(7),
        Constraint::Length(6), Constraint::Length(5), Constraint::Length(5),
        Constraint::Length(2), Constraint::Length(7), Constraint::Length(2),
        Constraint::Length(8),
    ];
    let header = Row::new(vec!["#", "St", "Side", "Price", "Size", "Edge", "Fair", "Conf", "T-lft", "T", "BTC", "W", "PnL"])
        .style(Style::default().fg(CYAN).bold());

    Widget::render(
        Table::new(rows, widths).header(header)
            .block(Block::bordered().title("Trades").border_style(BORDER)),
        trades_area, buf,
    );

    // Mini equity curve for just this market's trades
    if market.trades.len() >= 2 {
        let mut cum = 0.0;
        let data: Vec<(f64, f64)> = std::iter::once((0.0, 0.0)).chain(
            market.trades.iter().enumerate().map(|(i, t)| {
                cum += t.pnl;
                ((i + 1) as f64, cum)
            })
        ).collect();

        let x_max = data.last().map(|d| d.0).unwrap_or(1.0);
        let y_min = data.iter().map(|d| d.1).fold(f64::MAX, f64::min) - 0.5;
        let y_max = data.iter().map(|d| d.1).fold(f64::MIN, f64::max) + 0.5;
        let zero_line = vec![(0.0, 0.0), (x_max, 0.0)];

        let datasets = vec![
            Dataset::default().name("PnL").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
                .style(Style::default().fg(pnl_color(market.total_pnl))).data(&data),
            Dataset::default().name("0").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
                .style(Style::default().fg(GRAY)).data(&zero_line),
        ];
        let chart = Chart::new(datasets)
            .block(Block::bordered().title("Market Equity").border_style(BORDER))
            .x_axis(Axis::default().bounds([0.0, x_max]).style(BORDER))
            .y_axis(Axis::default().bounds([y_min, y_max])
                .labels::<Vec<Line>>(vec![format!("{:.1}", y_min).into(), "0".into(), format!("{:.1}", y_max).into()])
                .style(BORDER));
        Widget::render(chart, chart_area, buf);
    } else {
        Widget::render(Block::bordered().title("Market Equity").border_style(BORDER), chart_area, buf);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab 4: All Trades
// ─────────────────────────────────────────────────────────────────────────────

fn render_trades(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let filtered = app.filtered_trades();
    let visible_height = area.height.saturating_sub(4) as usize;
    let start = app.trade_scroll;
    let end = (start + visible_height).min(filtered.len());

    let rows: Vec<Row> = if start < filtered.len() {
        filtered[start..end].iter().map(|t| {
            let outcome_str = match t.outcome {
                Some(Side::Up) => "UP",
                Some(Side::Down) => "DN",
                None => "?",
            };
            let won_str = if t.won { "W" } else { "L" };
            let won_color = if t.won { GREEN } else { RED };

            Row::new(vec![
                Cell::from(format!("{}", t.market_idx + 1)).style(Style::default().fg(GRAY)),
                Cell::from(format!("#{}", t.order_id)).style(Style::default().fg(GRAY)),
                Cell::from(format!("{:<2}", strategy_short(&t.strategy))).style(Style::default().fg(strategy_color(&t.strategy))),
                Cell::from(format!("{}", t.side)).style(Style::default().fg(if t.side == Side::Up { GREEN } else { RED })),
                Cell::from(format!("{:.3}", t.price)),
                Cell::from(format!("${:.1}", t.size)).style(Style::default().fg(YELLOW)),
                Cell::from(format!("{:.3}", t.edge)).style(Style::default().fg(YELLOW)),
                Cell::from(format!("{:.3}", t.fair_value)),
                Cell::from(format!("{:.2}", t.confidence)),
                Cell::from(format!("{:.0}s", t.time_left_s)),
                Cell::from(if t.is_passive { "P" } else { "A" }),
                Cell::from(format!("${:.0}", t.btc_price)),
                Cell::from(outcome_str).style(Style::default().fg(if t.outcome == Some(Side::Up) { GREEN } else { RED })),
                Cell::from(won_str).style(Style::default().fg(won_color).bold()),
                Cell::from(format!("${:+.2}", t.pnl)).style(Style::default().fg(pnl_color(t.pnl))),
            ])
        }).collect()
    } else {
        vec![]
    };

    let widths = [
        Constraint::Length(3), Constraint::Length(4), Constraint::Length(3),
        Constraint::Length(5), Constraint::Length(6), Constraint::Length(6),
        Constraint::Length(6), Constraint::Length(6), Constraint::Length(5),
        Constraint::Length(5), Constraint::Length(2), Constraint::Length(7),
        Constraint::Length(4), Constraint::Length(2), Constraint::Length(8),
    ];
    let header = Row::new(vec![
        "Mkt", "#", "St", "Side", "Price", "Size", "Edge", "Fair", "Conf", "T-lft", "T", "BTC", "Out", "W", "PnL",
    ]).style(Style::default().fg(CYAN).bold());

    let filter_label = match &app.trade_filter {
        None => "All".to_string(),
        Some(name) => format!("{}", strategy_short(name)),
    };

    Widget::render(
        Table::new(rows, widths).header(header)
            .block(Block::bordered()
                .title(format!("Trades ({}) filter=[{}] [f] cycle [j/k] scroll", filtered.len(), filter_label))
                .border_style(BORDER)),
        area, buf,
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab 5: Full Equity Curve
// ─────────────────────────────────────────────────────────────────────────────

fn render_equity(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let [chart_area, bottom_area] = Layout::vertical([
        Constraint::Min(12),
        Constraint::Length(10),
    ])
    .areas(area);

    // Main equity chart (per-trade)
    if app.trade_equity_curve.len() >= 2 {
        let data: Vec<(f64, f64)> = app.trade_equity_curve.clone();
        let x_min = data.first().map(|d| d.0).unwrap_or(0.0);
        let x_max = data.last().map(|d| d.0).unwrap_or(1.0);
        let y_min = data.iter().map(|d| d.1).fold(f64::MAX, f64::min) - 2.0;
        let y_max = data.iter().map(|d| d.1).fold(f64::MIN, f64::max) + 2.0;
        let zero_line = vec![(x_min, 0.0), (x_max, 0.0)];

        // Per-strategy equity overlays
        let mut strat_names: Vec<&String> = app.strategy_stats.keys().collect();
        strat_names.sort();

        let strat_curves: Vec<(String, Color, Vec<(f64, f64)>)> = strat_names.iter().filter_map(|name| {
            let stats = &app.strategy_stats[*name];
            if stats.pnl_history.len() < 2 { return None; }
            let data: Vec<(f64, f64)> = stats.pnl_history.iter().enumerate()
                .map(|(i, &pnl)| (i as f64, pnl)).collect();
            Some((name.to_string(), strategy_color(name), data))
        }).collect();

        let mut datasets = vec![
            Dataset::default().name("Total").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
                .style(Style::default().fg(WHITE).bold()).data(&data),
            Dataset::default().name("Zero").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
                .style(Style::default().fg(GRAY)).data(&zero_line),
        ];

        for (name, color, ref curve_data) in &strat_curves {
            datasets.push(
                Dataset::default().name(strategy_short(name)).marker(symbols::Marker::Braille)
                    .graph_type(GraphType::Line).style(Style::default().fg(*color)).data(curve_data)
            );
        }

        let chart = Chart::new(datasets)
            .block(Block::bordered().title("Equity (per trade, strategy overlays)").border_style(BORDER))
            .x_axis(Axis::default().bounds([x_min, x_max])
                .labels::<Vec<Line>>(vec![
                    "0".into(),
                    format!("{}", app.all_trades.len() / 2).into(),
                    format!("{}", app.all_trades.len()).into(),
                ]).style(BORDER))
            .y_axis(Axis::default().bounds([y_min, y_max])
                .labels::<Vec<Line>>(vec![
                    format!("${:.0}", y_min).into(),
                    "$0".into(),
                    format!("${:.0}", y_max).into(),
                ]).style(BORDER))
            .legend_position(Some(LegendPosition::TopRight))
            .hidden_legend_constraints((Constraint::Percentage(60), Constraint::Percentage(60)));

        Widget::render(chart, chart_area, buf);
    } else {
        Widget::render(Block::bordered().title("Equity Curve").border_style(BORDER), chart_area, buf);
    }

    // Bottom: per-market PnL sparkline + rolling metrics
    let [bar_area, rolling_area] = Layout::horizontal([
        Constraint::Percentage(40),
        Constraint::Percentage(60),
    ])
    .areas(bottom_area);

    render_market_pnl_sparkline(app, bar_area, buf);
    render_rolling_metrics(app, rolling_area, buf);
}

fn render_market_pnl_sparkline(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let block = Block::bordered().title("Per-Market PnL").border_style(BORDER);
    let inner = block.inner(area);
    Widget::render(block, area, buf);

    if inner.height < 2 || app.markets.is_empty() { return; }

    let [win_area, loss_area] = Layout::vertical([
        Constraint::Length(inner.height / 2),
        Constraint::Min(1),
    ])
    .areas(inner);

    let wins: Vec<u64> = app.markets.iter().map(|m| {
        if m.total_pnl > 0.0 { (m.total_pnl * 100.0) as u64 } else { 0 }
    }).collect();
    let losses: Vec<u64> = app.markets.iter().map(|m| {
        if m.total_pnl < 0.0 { (m.total_pnl.abs() * 100.0) as u64 } else { 0 }
    }).collect();

    Sparkline::default().data(&wins).style(Style::default().fg(GREEN)).render(win_area, buf);
    Sparkline::default().data(&losses).style(Style::default().fg(RED)).render(loss_area, buf);
}

fn render_rolling_metrics(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    if app.rolling_sharpe.len() < 2 {
        Widget::render(Block::bordered().title("Rolling Metrics (need 20+ trades)").border_style(BORDER), area, buf);
        return;
    }

    let data: Vec<(f64, f64)> = app.rolling_sharpe.clone();
    let wr_data: Vec<(f64, f64)> = app.rolling_win_rate.iter().map(|&(x, wr)| (x, wr / 100.0)).collect();
    let x_min = data.first().map(|d| d.0).unwrap_or(0.0);
    let x_max = data.last().map(|d| d.0).unwrap_or(1.0);

    let all_y = data.iter().map(|d| d.1).chain(wr_data.iter().map(|d| d.1));
    let y_min = all_y.clone().fold(f64::MAX, f64::min) - 0.1;
    let y_max = all_y.fold(f64::MIN, f64::max) + 0.1;
    let zero_line = vec![(x_min, 0.0), (x_max, 0.0)];

    let datasets = vec![
        Dataset::default().name("Sharpe20").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(CYAN)).data(&data),
        Dataset::default().name("WR20").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(MAGENTA)).data(&wr_data),
        Dataset::default().name("0").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(GRAY)).data(&zero_line),
    ];

    let chart = Chart::new(datasets)
        .block(Block::bordered().title("Rolling 20-trade (Sharpe + WR)").border_style(BORDER))
        .x_axis(Axis::default().bounds([x_min, x_max]).style(BORDER))
        .y_axis(Axis::default().bounds([y_min, y_max])
            .labels::<Vec<Line>>(vec![format!("{:.1}", y_min).into(), "0".into(), format!("{:.1}", y_max).into()])
            .style(BORDER))
        .legend_position(Some(LegendPosition::TopRight))
        .hidden_legend_constraints((Constraint::Percentage(50), Constraint::Percentage(50)));

    Widget::render(chart, area, buf);
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab 6: Risk Analysis
// ─────────────────────────────────────────────────────────────────────────────

fn render_risk(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let [top_area, bottom_area] = Layout::vertical([
        Constraint::Min(12),
        Constraint::Length(14),
    ])
    .areas(area);

    let [dd_chart_area, risk_stats_area] = Layout::horizontal([
        Constraint::Percentage(60),
        Constraint::Percentage(40),
    ])
    .areas(top_area);

    // Drawdown chart
    render_drawdown_chart(app, dd_chart_area, buf);

    // Risk metrics panel
    render_risk_metrics(app, risk_stats_area, buf);

    // Bottom: PnL distribution + Edge distribution
    let [pnl_hist_area, edge_hist_area] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .areas(bottom_area);

    render_pnl_histogram(app, pnl_hist_area, buf);
    render_edge_histogram(app, edge_hist_area, buf);
}

fn render_drawdown_chart(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    if app.drawdown_curve.is_empty() {
        Widget::render(Block::bordered().title("Drawdown").border_style(BORDER), area, buf);
        return;
    }

    let data: Vec<(f64, f64)> = app.drawdown_curve.iter()
        .map(|d| (d.trade_idx as f64, -(d.drawdown)))  // negative so it hangs down
        .collect();

    let x_min = data.first().map(|d| d.0).unwrap_or(0.0);
    let x_max = data.last().map(|d| d.0).unwrap_or(1.0);
    let y_min = data.iter().map(|d| d.1).fold(f64::MAX, f64::min) - 1.0;
    let zero_line = vec![(x_min, 0.0), (x_max, 0.0)];

    let datasets = vec![
        Dataset::default().name("DD").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(RED)).data(&data),
        Dataset::default().name("0").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(GRAY)).data(&zero_line),
    ];

    let chart = Chart::new(datasets)
        .block(Block::bordered().title(format!("Drawdown (max ${:.2}, {} trades)", app.max_drawdown(), app.max_drawdown_duration)).border_style(BORDER))
        .x_axis(Axis::default().bounds([x_min, x_max])
            .labels::<Vec<Line>>(vec!["0".into(), format!("{}", app.all_trades.len()).into()]).style(BORDER))
        .y_axis(Axis::default().bounds([y_min, 0.5])
            .labels::<Vec<Line>>(vec![format!("-${:.0}", y_min.abs()).into(), "$0".into()]).style(BORDER));

    Widget::render(chart, area, buf);
}

fn render_risk_metrics(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let mdd = app.max_drawdown();
    let mdd_pct = if app.bankroll > 0.0 { mdd / app.bankroll * 100.0 } else { 0.0 };

    let lines = vec![
        Line::from(""),
        kv_line("  Max Drawdown", format!("${:.2} ({:.1}%)", mdd, mdd_pct), RED),
        kv_line("  DD Duration", format!("{} trades", app.max_drawdown_duration), RED),
        kv_line("  Recovery Factor", format!("{:.2}", app.recovery_factor), if app.recovery_factor > 1.0 { GREEN } else { YELLOW }),
        Line::from(""),
        kv_line("  Sharpe Ratio", format!("{:.3}", app.sharpe_ratio()), if app.sharpe_ratio() > 0.5 { GREEN } else { YELLOW }),
        kv_line("  Sortino Ratio", format!("{:.3}", app.sortino_ratio), if app.sortino_ratio > 0.5 { GREEN } else { YELLOW }),
        kv_line("  Calmar Ratio", format!("{:.3}", app.calmar_ratio), if app.calmar_ratio > 1.0 { GREEN } else { YELLOW }),
        Line::from(""),
        kv_line("  Consec Wins", format!("{}", app.max_consecutive_wins), GREEN),
        kv_line("  Consec Losses", format!("{}", app.max_consecutive_losses), RED),
        kv_line("  Payoff Ratio", format!("{:.2}", app.payoff_ratio), if app.payoff_ratio > 1.0 { GREEN } else { RED }),
        kv_line("  Kelly f*", format!("{:.1}%", app.kelly_fraction * 100.0), CYAN),
        kv_line("  Expectancy", format!("${:.4}/trade", app.expectancy), pnl_color(app.expectancy)),
    ];

    Widget::render(
        Paragraph::new(lines).block(Block::bordered().title("Risk Metrics").border_style(BORDER)),
        area, buf,
    );
}

fn render_pnl_histogram(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let block = Block::bordered().title("PnL Distribution").border_style(BORDER);
    let inner = block.inner(area);
    Widget::render(block, area, buf);

    if app.pnl_histogram.is_empty() || inner.height < 2 { return; }

    let max_count = app.pnl_histogram.iter().map(|h| h.1).max().unwrap_or(1);
    let bar_height = inner.height.saturating_sub(1) as f64;

    // Render bars column by column
    let n_bins = app.pnl_histogram.len();
    let bin_width = if n_bins > 0 { (inner.width as usize) / n_bins } else { 1 };
    if bin_width == 0 { return; }

    for (i, &(center, count)) in app.pnl_histogram.iter().enumerate() {
        let x = inner.x + (i * bin_width) as u16;
        if x >= inner.x + inner.width { break; }
        let h = ((count as f64 / max_count as f64) * bar_height) as u16;
        let color = if center >= 0.0 { GREEN } else { RED };

        for dy in 0..h {
            let y = inner.y + inner.height - 1 - dy;
            if y >= inner.y && x < inner.x + inner.width {
                let cell = buf.cell_mut((x, y));
                if let Some(cell) = cell {
                    cell.set_char('\u{2588}');
                    cell.set_fg(color);
                }
            }
        }
    }
}

fn render_edge_histogram(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let block = Block::bordered().title("Edge Distribution").border_style(BORDER);
    let inner = block.inner(area);
    Widget::render(block, area, buf);

    if app.edge_histogram.is_empty() || inner.height < 2 { return; }

    let max_count = app.edge_histogram.iter().map(|h| h.1).max().unwrap_or(1);
    let bar_height = inner.height.saturating_sub(1) as f64;

    let n_bins = app.edge_histogram.len();
    let bin_width = if n_bins > 0 { (inner.width as usize) / n_bins } else { 1 };
    if bin_width == 0 { return; }

    for (i, &(_center, count)) in app.edge_histogram.iter().enumerate() {
        let x = inner.x + (i * bin_width) as u16;
        if x >= inner.x + inner.width { break; }
        let h = ((count as f64 / max_count as f64) * bar_height) as u16;

        for dy in 0..h {
            let y = inner.y + inner.height - 1 - dy;
            if y >= inner.y && x < inner.x + inner.width {
                let cell = buf.cell_mut((x, y));
                if let Some(cell) = cell {
                    cell.set_char('\u{2588}');
                    cell.set_fg(YELLOW);
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab 7: Timing Analysis
// ─────────────────────────────────────────────────────────────────────────────

fn render_timing(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let [table_area, chart_area] = Layout::vertical([
        Constraint::Min(12),
        Constraint::Length(14),
    ])
    .areas(area);

    // Time bucket table
    let rows: Vec<Row> = app.time_buckets.iter().filter(|b| b.n_trades > 0).map(|b| {
        let wr = if b.n_trades > 0 { b.n_wins as f64 / b.n_trades as f64 * 100.0 } else { 0.0 };
        Row::new(vec![
            Cell::from(b.label),
            Cell::from(format!("{}", b.n_trades)),
            Cell::from(format!("{}", b.n_wins)).style(Style::default().fg(GREEN)),
            Cell::from(format!("{}", b.n_trades - b.n_wins)).style(Style::default().fg(RED)),
            Cell::from(format!("{:.0}%", wr)).style(Style::default().fg(wr_color(wr))),
            Cell::from(format!("${:+.2}", b.total_pnl)).style(Style::default().fg(pnl_color(b.total_pnl))),
            Cell::from(format!("{:.4}", b.avg_edge)).style(Style::default().fg(YELLOW)),
            Cell::from(format!("{:.2}", b.avg_confidence)),
            Cell::from(format!("${:.1}", b.avg_size)),
            Cell::from(render_inline_bar(b.total_pnl, app.time_buckets.iter().map(|b| b.total_pnl.abs()).fold(0.0f64, f64::max))),
        ])
    }).collect();

    let widths = [
        Constraint::Length(8), Constraint::Length(4), Constraint::Length(4),
        Constraint::Length(4), Constraint::Length(5), Constraint::Length(9),
        Constraint::Length(7), Constraint::Length(5), Constraint::Length(6),
        Constraint::Min(15),
    ];
    let header = Row::new(vec!["T-left", "Trd", "W", "L", "WR%", "PnL", "Edge", "Conf", "Size", ""])
        .style(Style::default().fg(CYAN).bold());

    Widget::render(
        Table::new(rows, widths).header(header)
            .block(Block::bordered().title("Timing Analysis (PnL by time remaining)").border_style(BORDER)),
        table_area, buf,
    );

    // Bottom: per-strategy timing breakdown
    let [left_chart, right_chart] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .areas(chart_area);

    render_timing_pnl_chart(app, left_chart, buf);
    render_timing_wr_chart(app, right_chart, buf);
}

fn render_inline_bar(val: f64, max_abs: f64) -> String {
    let max_abs = if max_abs == 0.0 { 1.0 } else { max_abs };
    let width: usize = 12;
    let filled = ((val.abs() / max_abs) * width as f64) as usize;
    if val >= 0.0 {
        format!("{}{}", "\u{2588}".repeat(filled), " ".repeat(width.saturating_sub(filled)))
    } else {
        format!("{}{}", " ".repeat(width.saturating_sub(filled)), "\u{2588}".repeat(filled))
    }
}

fn render_timing_pnl_chart(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let active_buckets: Vec<&crate::types::TimeBucket> = app.time_buckets.iter()
        .filter(|b| b.n_trades > 0).collect();
    if active_buckets.is_empty() {
        Widget::render(Block::bordered().title("PnL by Time").border_style(BORDER), area, buf);
        return;
    }

    let data: Vec<(f64, f64)> = active_buckets.iter().enumerate()
        .map(|(i, b)| (i as f64, b.total_pnl)).collect();
    let x_max = data.last().map(|d| d.0).unwrap_or(1.0);
    let y_min = data.iter().map(|d| d.1).fold(f64::MAX, f64::min) - 1.0;
    let y_max = data.iter().map(|d| d.1).fold(f64::MIN, f64::max) + 1.0;
    let zero_line = vec![(0.0, 0.0), (x_max, 0.0)];

    let datasets = vec![
        Dataset::default().name("PnL").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(CYAN)).data(&data),
        Dataset::default().name("0").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(GRAY)).data(&zero_line),
    ];

    let chart = Chart::new(datasets)
        .block(Block::bordered().title("PnL by Bucket").border_style(BORDER))
        .x_axis(Axis::default().bounds([0.0, x_max]).style(BORDER))
        .y_axis(Axis::default().bounds([y_min, y_max])
            .labels::<Vec<Line>>(vec![format!("{:.0}", y_min).into(), "0".into(), format!("{:.0}", y_max).into()])
            .style(BORDER));

    Widget::render(chart, area, buf);
}

fn render_timing_wr_chart(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let active_buckets: Vec<&crate::types::TimeBucket> = app.time_buckets.iter()
        .filter(|b| b.n_trades > 0).collect();
    if active_buckets.is_empty() {
        Widget::render(Block::bordered().title("WR by Time").border_style(BORDER), area, buf);
        return;
    }

    let data: Vec<(f64, f64)> = active_buckets.iter().enumerate()
        .map(|(i, b)| {
            let wr = if b.n_trades > 0 { b.n_wins as f64 / b.n_trades as f64 * 100.0 } else { 0.0 };
            (i as f64, wr)
        }).collect();
    let x_max = data.last().map(|d| d.0).unwrap_or(1.0);
    let fifty_line = vec![(0.0, 50.0), (x_max, 50.0)];

    let datasets = vec![
        Dataset::default().name("WR%").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(MAGENTA)).data(&data),
        Dataset::default().name("50%").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(GRAY)).data(&fifty_line),
    ];

    let chart = Chart::new(datasets)
        .block(Block::bordered().title("Win Rate by Bucket").border_style(BORDER))
        .x_axis(Axis::default().bounds([0.0, x_max]).style(BORDER))
        .y_axis(Axis::default().bounds([0.0, 100.0])
            .labels::<Vec<Line>>(vec!["0%".into(), "50%".into(), "100%".into()])
            .style(BORDER));

    Widget::render(chart, area, buf);
}

// ─────────────────────────────────────────────────────────────────────────────
// Tab 8: Correlation Matrix
// ─────────────────────────────────────────────────────────────────────────────

fn render_correlation(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let [matrix_area, details_area] = Layout::vertical([
        Constraint::Min(10),
        Constraint::Length(12),
    ])
    .areas(area);

    render_correlation_matrix(app, matrix_area, buf);
    render_correlation_details(app, details_area, buf);
}

fn render_correlation_matrix(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let names = &app.correlation_names;
    let corr = &app.strategy_correlations;
    let n = names.len();

    if n == 0 {
        Widget::render(Block::bordered().title("Correlation Matrix").border_style(BORDER), area, buf);
        return;
    }

    // Build table: header = ["", S1, S2, ...], rows = [S1, corr11, corr12, ...]
    let mut header_cells = vec![""];
    let short_names: Vec<&str> = names.iter().map(|n| strategy_short(n)).collect();
    for sn in &short_names {
        header_cells.push(sn);
    }
    let header = Row::new(header_cells).style(Style::default().fg(CYAN).bold());

    let rows: Vec<Row> = (0..n).map(|i| {
        let mut cells = vec![
            Cell::from(format!("{:<2}", short_names[i])).style(Style::default().fg(strategy_color(&names[i]))),
        ];
        for j in 0..n {
            let v = corr[i][j];
            let display = if i == j {
                "1.00".to_string()
            } else {
                format!("{:+.2}", v)
            };
            let color = if i == j { GRAY } else { corr_color(v) };
            cells.push(Cell::from(display).style(Style::default().fg(color)));
        }
        Row::new(cells)
    }).collect();

    let mut widths = vec![Constraint::Length(3)];
    for _ in 0..n {
        widths.push(Constraint::Length(6));
    }

    Widget::render(
        Table::new(rows, widths).header(header)
            .block(Block::bordered().title("Strategy Correlation Matrix (per-market PnL)").border_style(BORDER)),
        area, buf,
    );
}

fn render_correlation_details(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let [left, right] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .areas(area);

    // Left: diversification score & interpretation
    let names = &app.correlation_names;
    let corr = &app.strategy_correlations;
    let n = names.len();

    let mut avg_corr = 0.0;
    let mut count = 0;
    for i in 0..n {
        for j in (i+1)..n {
            avg_corr += corr[i][j];
            count += 1;
        }
    }
    if count > 0 { avg_corr /= count as f64; }

    let div_score = 1.0 - avg_corr.abs();
    let div_rating = if div_score > 0.8 { "Excellent" }
        else if div_score > 0.6 { "Good" }
        else if div_score > 0.4 { "Fair" }
        else { "Poor" };
    let div_color = if div_score > 0.6 { GREEN } else if div_score > 0.4 { YELLOW } else { RED };

    let lines = vec![
        Line::from(""),
        kv_line("  Avg Correlation", format!("{:+.3}", avg_corr), corr_color(avg_corr)),
        kv_line("  Diversification", format!("{:.0}% ({})", div_score * 100.0, div_rating), div_color),
        Line::from(""),
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("\u{2588} >0.7 ", Style::default().fg(Color::LightGreen)),
            Span::styled("\u{2588} >0.3 ", Style::default().fg(GREEN)),
            Span::styled("\u{2588} neutral ", Style::default().fg(WHITE)),
            Span::styled("\u{2588} <-0.3 ", Style::default().fg(Color::LightRed)),
            Span::styled("\u{2588} <-0.7", Style::default().fg(RED)),
        ]),
    ];

    Widget::render(
        Paragraph::new(lines).block(Block::bordered().title("Diversification").border_style(BORDER)),
        left, buf,
    );

    // Right: strongest pairs
    let mut pairs: Vec<(usize, usize, f64)> = Vec::new();
    for i in 0..n {
        for j in (i+1)..n {
            pairs.push((i, j, corr[i][j]));
        }
    }
    pairs.sort_by(|a, b| b.2.abs().partial_cmp(&a.2.abs()).unwrap_or(std::cmp::Ordering::Equal));

    let pair_rows: Vec<Row> = pairs.iter().take(6).map(|&(i, j, c)| {
        Row::new(vec![
            Cell::from(format!("{}", strategy_short(&names[i]))).style(Style::default().fg(strategy_color(&names[i]))),
            Cell::from("\u{2194}").style(Style::default().fg(GRAY)),
            Cell::from(format!("{}", strategy_short(&names[j]))).style(Style::default().fg(strategy_color(&names[j]))),
            Cell::from(format!("{:+.3}", c)).style(Style::default().fg(corr_color(c))),
            Cell::from(corr_bar(c)),
        ])
    }).collect();

    let widths = [
        Constraint::Length(3), Constraint::Length(2), Constraint::Length(3),
        Constraint::Length(6), Constraint::Min(8),
    ];
    let header = Row::new(vec!["", "", "", "Corr", ""])
        .style(Style::default().fg(CYAN).bold());

    Widget::render(
        Table::new(pair_rows, widths).header(header)
            .block(Block::bordered().title("Strongest Pairs").border_style(BORDER)),
        right, buf,
    );
}

fn corr_bar(v: f64) -> String {
    let width = 8;
    let center = width / 2;
    let magnitude = (v.abs() * center as f64) as usize;
    let mut bar = vec!['\u{2500}'; width];
    bar[center] = '\u{2502}';
    if v > 0.0 {
        for i in center+1..=(center + magnitude).min(width - 1) {
            bar[i] = '\u{2588}';
        }
    } else {
        for i in (center.saturating_sub(magnitude)..center).rev() {
            bar[i] = '\u{2588}';
        }
    }
    bar.iter().collect()
}
