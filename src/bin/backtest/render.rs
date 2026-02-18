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
    render_footer(footer_area, frame.buffer_mut());

    match app.tab {
        Tab::Summary => render_summary(app, body_area, frame.buffer_mut()),
        Tab::Strategies => render_strategies(app, body_area, frame.buffer_mut()),
        Tab::Markets => render_markets(app, body_area, frame.buffer_mut()),
        Tab::Trades => render_trades(app, body_area, frame.buffer_mut()),
        Tab::Equity => render_equity(app, body_area, frame.buffer_mut()),
    }
}

// ─── Header ───

fn render_header(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let pnl = app.total_pnl;
    let pnl_sign = if pnl >= 0.0 { "+" } else { "" };
    let text = format!(
        " BACKTEST | {} markets | {} trades | PnL {}{:.2} | WR {:.0}% | ROI {:.1}% | Sharpe {:.2} | Bankroll ${:.0}",
        app.markets.len(),
        app.all_trades.len(),
        pnl_sign, pnl,
        app.win_rate() * 100.0,
        app.roi(),
        app.sharpe_ratio(),
        app.bankroll,
    );
    Paragraph::new(text)
        .style(Style::default().fg(Color::Black).bg(if pnl >= 0.0 { GREEN } else { RED }))
        .render(area, buf);
}

// ─── Tabs bar ───

fn render_tabs(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let titles: Vec<Span> = Tab::all().iter().map(|t| {
        let style = if *t == app.tab {
            Style::default().fg(Color::Black).bg(CYAN).bold()
        } else {
            Style::default().fg(WHITE).bg(Color::Reset)
        };
        Span::styled(format!(" {} ", t.label()), style)
    }).collect();

    let mut line_spans = Vec::new();
    for (i, title) in titles.into_iter().enumerate() {
        if i > 0 {
            line_spans.push(Span::styled(" | ", Style::default().fg(GRAY)));
        }
        line_spans.push(title);
    }
    line_spans.push(Span::styled("   [Tab/1-5] switch  ", Style::default().fg(GRAY)));

    Paragraph::new(Line::from(line_spans)).render(area, buf);
}

// ─── Footer ───

fn render_footer(area: Rect, buf: &mut Buffer) {
    let text = " [Tab/1-5] Switch tab  [j/k] Scroll  [f] Filter trades  [q/Esc] Quit";
    Paragraph::new(text)
        .style(Style::default().fg(Color::Black).bg(GRAY))
        .render(area, buf);
}

// ─── Tab 1: Summary Dashboard ───

fn render_summary(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let [top_area, bottom_area] = Layout::vertical([
        Constraint::Min(16),
        Constraint::Length(12),
    ])
    .areas(area);

    let [stats_area, breakdown_area] = Layout::horizontal([
        Constraint::Length(44),
        Constraint::Min(30),
    ])
    .areas(top_area);

    // Left: Key performance metrics
    render_summary_stats(app, stats_area, buf);

    // Right: Strategy P&L breakdown bar chart (text-based)
    render_strategy_pnl_bars(app, breakdown_area, buf);

    // Bottom: Mini equity curve
    render_mini_equity(app, bottom_area, buf);
}

fn render_summary_stats(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let pnl = app.total_pnl;
    let wr = app.win_rate() * 100.0;
    let roi = app.roi();
    let pf = app.profit_factor();
    let sharpe = app.sharpe_ratio();
    let mdd = app.max_drawdown();
    let n_markets = app.markets.len();
    let n_trades = app.all_trades.len();
    let n_wins = app.n_wins;
    let n_losses = app.n_losses;
    let invested = app.total_invested;
    let wins_pnl: f64 = app.all_trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).sum();
    let loss_pnl: f64 = app.all_trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl).sum();
    let avg_win = if n_wins > 0 { wins_pnl / n_wins as f64 } else { 0.0 };
    let avg_loss = if n_losses > 0 { loss_pnl / n_losses as f64 } else { 0.0 };
    let best_market = app.markets.iter().max_by(|a, b| a.total_pnl.partial_cmp(&b.total_pnl).unwrap());
    let worst_market = app.markets.iter().min_by(|a, b| a.total_pnl.partial_cmp(&b.total_pnl).unwrap());

    let lines = vec![
        Line::from(""),
        kv_line("  Total PnL", format!("${:+.2}", pnl), pnl_color(pnl)),
        kv_line("  Total Invested", format!("${:.2}", invested), WHITE),
        kv_line("  ROI", format!("{:.1}%", roi), pnl_color(roi)),
        Line::from(""),
        kv_line("  Win Rate", format!("{:.1}% ({}/{})", wr, n_wins, n_wins + n_losses), if wr >= 50.0 { GREEN } else { RED }),
        kv_line("  Profit Factor", format!("{:.2}", pf), if pf > 1.0 { GREEN } else { RED }),
        kv_line("  Sharpe Ratio", format!("{:.2}", sharpe), if sharpe > 0.0 { GREEN } else { RED }),
        kv_line("  Max Drawdown", format!("${:.2}", mdd), RED),
        Line::from(""),
        kv_line("  Avg Win", format!("${:.2}", avg_win), GREEN),
        kv_line("  Avg Loss", format!("${:.2}", avg_loss), RED),
        kv_line("  Markets", format!("{}", n_markets), WHITE),
        kv_line("  Trades", format!("{}", n_trades), WHITE),
        Line::from(""),
        kv_line("  Best Market",
            best_market.map(|m| format!("{} ${:+.2}", m.dir_name.chars().rev().take(20).collect::<String>().chars().rev().collect::<String>(), m.total_pnl)).unwrap_or_default(),
            GREEN),
        kv_line("  Worst Market",
            worst_market.map(|m| format!("{} ${:+.2}", m.dir_name.chars().rev().take(20).collect::<String>().chars().rev().collect::<String>(), m.total_pnl)).unwrap_or_default(),
            RED),
    ];

    Widget::render(
        Paragraph::new(lines).block(Block::bordered().title("Performance Summary").border_style(BORDER)),
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
            Cell::from(format!("{:>5.0}%", wr)).style(Style::default().fg(if *wr >= 50.0 { GREEN } else { RED })),
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
            .block(Block::bordered().title("Strategy Breakdown").border_style(BORDER)),
        area, buf,
    );
}

fn render_mini_equity(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    if app.equity_curve.len() < 2 {
        Widget::render(Block::bordered().title("Equity Curve").border_style(BORDER), area, buf);
        return;
    }

    let data: Vec<(f64, f64)> = app.equity_curve.clone();
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
        .block(Block::bordered().title("Equity Curve (cumulative PnL per market)").border_style(BORDER))
        .x_axis(Axis::default().bounds([x_min, x_max])
            .labels::<Vec<Line>>(vec!["0".into(), format!("{}", app.markets.len()).into()]).style(BORDER))
        .y_axis(Axis::default().bounds([y_min, y_max])
            .labels::<Vec<Line>>(vec![
                format!("{:.0}", y_min).into(),
                "0".into(),
                format!("{:.0}", y_max).into(),
            ]).style(BORDER));

    Widget::render(chart, area, buf);
}

// ─── Tab 2: Strategies (detailed per-strategy) ───

fn render_strategies(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let [table_area, detail_area] = Layout::vertical([
        Constraint::Min(10),
        Constraint::Length(14),
    ])
    .areas(area);

    // Strategy comparison table
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
            Cell::from(format!("{:.1}%", stats.win_rate() * 100.0)).style(Style::default().fg(if stats.win_rate() >= 0.5 { GREEN } else { RED })),
            Cell::from(format!("${:+.2}", stats.total_pnl)).style(Style::default().fg(pnl_color(stats.total_pnl))),
            Cell::from(format!("${:.2}", stats.total_invested)),
            Cell::from(format!("{:.1}%", stats.roi())).style(Style::default().fg(pnl_color(stats.roi()))),
            Cell::from(format!("{:.2}", stats.profit_factor())).style(Style::default().fg(if stats.profit_factor() > 1.0 { GREEN } else { RED })),
            Cell::from(format!("${:.2}", stats.max_drawdown())).style(Style::default().fg(RED)),
            Cell::from(format!("{:.3}", stats.avg_edge())).style(Style::default().fg(YELLOW)),
            Cell::from(format!("${:+.2}", stats.max_win)).style(Style::default().fg(GREEN)),
            Cell::from(format!("${:+.2}", stats.max_loss)).style(Style::default().fg(RED)),
            Cell::from(format!("{:.0}s", stats.avg_time_left_s)),
        ]).style(highlight)
    }).collect();

    let widths = [
        Constraint::Length(3), Constraint::Length(18), Constraint::Length(5),
        Constraint::Length(5), Constraint::Length(5), Constraint::Length(7),
        Constraint::Length(9), Constraint::Length(9), Constraint::Length(7),
        Constraint::Length(7), Constraint::Length(8), Constraint::Length(7),
        Constraint::Length(8), Constraint::Length(8), Constraint::Length(6),
    ];
    let header = Row::new(vec![
        "", "Strategy", "Trd", "Win", "Loss", "WR%", "PnL", "Invested", "ROI%", "PF", "MaxDD", "Edge", "Best", "Worst", "AvgT",
    ]).style(Style::default().fg(CYAN).bold());

    Widget::render(
        Table::new(rows, widths).header(header)
            .block(Block::bordered().title("Strategy Performance (all markets)").border_style(BORDER)),
        table_area, buf,
    );

    // Detail panel for selected strategy
    if let Some(name) = strat_names.get(app.strategy_scroll) {
        render_strategy_detail(app, name, detail_area, buf);
    }
}

fn render_strategy_detail(app: &BacktestApp, name: &str, area: Rect, buf: &mut Buffer) {
    let stats = match app.strategy_stats.get(name) {
        Some(s) => s,
        None => return,
    };

    let [left, right] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
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
        let short_name: String = dir.chars().rev().take(25).collect::<String>().chars().rev().collect();
        Row::new(vec![
            Cell::from(short_name),
            Cell::from(format!("{}", n)),
            Cell::from(format!("${:+.2}", pnl)).style(Style::default().fg(pnl_color(*pnl))),
        ])
    }).collect();

    let widths = [Constraint::Min(15), Constraint::Length(4), Constraint::Length(9)];
    let header = Row::new(vec!["Market", "#", "PnL"]).style(Style::default().fg(CYAN).bold());

    Widget::render(
        Table::new(rows, widths).header(header)
            .block(Block::bordered().title(format!("{} per market", strategy_short(name))).border_style(BORDER)),
        left, buf,
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

// ─── Tab 3: Markets ───

fn render_markets(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let visible_height = area.height.saturating_sub(4) as usize; // border + header + border
    let start = app.market_scroll;
    let end = (start + visible_height).min(app.markets.len());

    let rows: Vec<Row> = app.markets[start..end].iter().enumerate().map(|(i, m)| {
        let outcome_str = if m.outcome == Side::Up { "UP" } else { "DN" };
        let outcome_color = if m.outcome == Side::Up { GREEN } else { RED };
        let n_trades = m.trades.len();
        let n_wins = m.trades.iter().filter(|t| t.won).count();
        let wr = if n_trades > 0 { n_wins as f64 / n_trades as f64 * 100.0 } else { 0.0 };

        // Per-strategy count
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

        let highlight = if start + i == app.market_scroll { Style::default() } else { Style::default() };
        Row::new(vec![
            Cell::from(format!("{}", start + i + 1)).style(Style::default().fg(GRAY)),
            Cell::from(m.dir_name.chars().rev().take(28).collect::<String>().chars().rev().collect::<String>()),
            Cell::from(format!("${:.0}", m.strike)),
            Cell::from(format!("${:.0}", m.final_price)),
            Cell::from(format!("{:+.0}", m.final_distance)).style(Style::default().fg(if m.final_distance >= 0.0 { GREEN } else { RED })),
            Cell::from(outcome_str).style(Style::default().fg(outcome_color)),
            Cell::from(format!("{}", m.n_events)).style(Style::default().fg(GRAY)),
            Cell::from(format!("{}", n_trades)),
            Cell::from(format!("{:.0}%", wr)).style(Style::default().fg(if wr >= 50.0 { GREEN } else { RED })),
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
            .block(Block::bordered().title(format!("Markets ({}) [j/k scroll]", app.markets.len())).border_style(BORDER)),
        area, buf,
    );
}

// ─── Tab 4: All Trades ───

fn render_trades(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let filtered = app.filtered_trades();
    let visible_height = area.height.saturating_sub(4) as usize;
    let start = app.trade_scroll;
    let end = (start + visible_height).min(filtered.len());

    let rows: Vec<Row> = filtered[start..end].iter().map(|t| {
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
    }).collect();

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

// ─── Tab 5: Full Equity Curve ───

fn render_equity(app: &BacktestApp, area: Rect, buf: &mut Buffer) {
    let [chart_area, stats_area] = Layout::vertical([
        Constraint::Min(12),
        Constraint::Length(8),
    ])
    .areas(area);

    // Main equity chart
    if app.equity_curve.len() >= 2 {
        let data: Vec<(f64, f64)> = app.equity_curve.clone();
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
            .block(Block::bordered().title("Equity Curve (per market)").border_style(BORDER))
            .x_axis(Axis::default().bounds([x_min, x_max])
                .labels::<Vec<Line>>(vec!["0".into(), format!("{}", app.markets.len() / 2).into(), format!("{}", app.markets.len()).into()])
                .style(BORDER))
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

    // Bottom stats: per-market P&L bar
    let [bar_area, _] = Layout::horizontal([
        Constraint::Min(10),
        Constraint::Length(0),
    ])
    .areas(stats_area);

    render_market_pnl_sparkline(app, bar_area, buf);
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
