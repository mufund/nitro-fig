use std::collections::VecDeque;
use std::time::Duration;

use ratatui::prelude::*;
use ratatui::widgets::*;

use polymarket_crypto::math::pricing::{delta_bin, p_fair, z_score};
use polymarket_crypto::math::regime::Regime;
use polymarket_crypto::types::Side;

use crate::types::App;

// ─── Colors & style helpers ───

const BORDER: Style = Style::new().fg(Color::DarkGray);

/// Strategy name → (short label, color)
fn strategy_style(name: &str) -> (&'static str, Color) {
    match name {
        "latency_arb"       => ("LA", Color::Yellow),
        "certainty_capture" => ("CC", Color::LightCyan),
        "convexity_fade"    => ("CF", Color::LightMagenta),
        "cross_timeframe"   => ("CT", Color::LightBlue),
        "strike_misalign"   => ("SM", Color::LightRed),
        "lp_extreme"        => ("LP", Color::LightGreen),
        _                   => ("??", Color::White),
    }
}

fn side_color(side: &str) -> Color {
    if side == "UP" { Color::Green } else { Color::Red }
}

// ─── Main draw ───

pub fn draw(app: &App, frame: &mut Frame) {
    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(10),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_header(app, header_area, frame.buffer_mut());
    render_footer(app, footer_area, frame.buffer_mut());

    let [left_area, right_area] = Layout::horizontal([
        Constraint::Length(36),
        Constraint::Min(40),
    ])
    .areas(body_area);

    // Left column
    let [up_book_area, dn_book_area, metrics_area] = Layout::vertical([
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Min(8),
    ])
    .areas(left_area);

    render_orderbook("UP Book", &app.state.up_book, up_book_area, frame.buffer_mut());
    render_orderbook("DN Book", &app.state.down_book, dn_book_area, frame.buffer_mut());
    render_metrics(app, metrics_area, frame.buffer_mut());

    // Right column
    let [btc_area, pm_area, vol_area, bottom_area] = Layout::vertical([
        Constraint::Percentage(30),
        Constraint::Percentage(30),
        Constraint::Length(4),
        Constraint::Min(6),
    ])
    .areas(right_area);

    render_price_chart(app, btc_area, frame.buffer_mut());
    render_pm_chart(app, pm_area, frame.buffer_mut());
    render_volume_sparklines(app, vol_area, frame.buffer_mut());

    let [sig_area, ord_area] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .areas(bottom_area);

    render_signals(app, sig_area, frame.buffer_mut());
    render_orders(app, ord_area, frame.buffer_mut());
}

// ─── Header / Footer ───

fn render_header(app: &App, area: Rect, buf: &mut Buffer) {
    let price = app.state.bn.binance_price;
    let strike = app.state.info.strike;
    let now_ms = app.current_event_ts();
    let t_left = app.state.time_left_s(now_ms);
    let event_type = app.current_event_label();
    let play_icon = if app.playing { "PLAY" } else { "PAUSE" };

    let text = format!(
        " {} | BTC ${:.0} | K ${:.0} | dist ${:+.0} | T-{:.0}s | [{}/{}] {}x {} | {}",
        app.market_info.slug, price, strike, price - strike, t_left,
        app.cursor, app.events.len(), app.speed, play_icon, event_type,
    );
    Paragraph::new(text)
        .style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .render(area, buf);
}

fn render_footer(app: &App, area: Rect, buf: &mut Buffer) {
    if let Some((ref msg, at)) = app.status_msg {
        if at.elapsed() < Duration::from_secs(5) {
            Paragraph::new(format!(" {}", msg))
                .style(Style::default().fg(Color::Black).bg(Color::Green))
                .render(area, buf);
            return;
        }
    }
    let text = " [</>] Step  [Space] Play/Pause  [+/-] Speed  [PgUp/Dn] +/-100  [Home/End] Jump  [s] Save CSV  [q] Quit";
    Paragraph::new(text)
        .style(Style::default().fg(Color::Black).bg(Color::DarkGray))
        .render(area, buf);
}

// ─── Orderbook ───

fn render_orderbook(
    title: &str,
    book: &polymarket_crypto::engine::state::OrderBook,
    area: Rect,
    buf: &mut Buffer,
) {
    let mut rows: Vec<Row> = Vec::new();

    let ask_levels: Vec<&(f64, f64)> = book.asks.iter().take(5).collect();
    for level in ask_levels.iter().rev() {
        let bar = "\u{2588}".repeat((level.1 / 50.0).min(20.0) as usize);
        rows.push(Row::new(vec![
            Cell::from(format!("{:.2}", level.0)).style(Style::default().fg(Color::Red)),
            Cell::from(format!("{:.0}", level.1)).style(Style::default().fg(Color::Red)),
            Cell::from(bar).style(Style::default().fg(Color::Red)),
        ]));
    }

    rows.push(Row::new(vec![
        Cell::from(format!("spread {:.3}", book.spread())).style(Style::default().fg(Color::DarkGray)),
        Cell::from(""), Cell::from(""),
    ]));

    for level in book.bids.iter().take(5) {
        let bar = "\u{2588}".repeat((level.1 / 50.0).min(20.0) as usize);
        rows.push(Row::new(vec![
            Cell::from(format!("{:.2}", level.0)).style(Style::default().fg(Color::Green)),
            Cell::from(format!("{:.0}", level.1)).style(Style::default().fg(Color::Green)),
            Cell::from(bar).style(Style::default().fg(Color::Green)),
        ]));
    }

    let widths = [Constraint::Length(8), Constraint::Length(7), Constraint::Min(5)];
    Widget::render(
        Table::new(rows, widths).block(Block::bordered().title(title).border_style(BORDER)),
        area, buf,
    );
}

// ─── BTC Price Chart ───

fn render_price_chart(app: &App, area: Rect, buf: &mut Buffer) {
    if app.price_history.is_empty() {
        Widget::render(Block::bordered().title("BTC Price").border_style(BORDER), area, buf);
        return;
    }

    let data: Vec<(f64, f64)> = app.price_history.iter().copied().collect();
    let vwap_data: Vec<(f64, f64)> = app.vwap_history.iter().copied().collect();
    let strike = app.state.info.strike;

    let x_min = data.first().map(|d| d.0).unwrap_or(0.0);
    let x_max = data.last().map(|d| d.0).unwrap_or(1.0);
    let strike_data = vec![(x_min, strike), (x_max, strike)];

    // Snap signal/order markers to nearest price line point
    let snap = |idx: usize| -> Option<(f64, f64)> {
        let x = idx as f64;
        if x < x_min || x > x_max { return None; }
        let pos = data.partition_point(|d| d.0 < x);
        let closest = if pos >= data.len() {
            &data[data.len() - 1]
        } else if pos == 0 {
            &data[0]
        } else {
            let (left, right) = (&data[pos - 1], &data[pos]);
            if (x - left.0).abs() <= (right.0 - x).abs() { left } else { right }
        };
        Some(*closest)
    };

    let sig_up: Vec<(f64, f64)> = app.signal_log.iter().filter(|s| s.side == "UP").filter_map(|s| snap(s.event_idx)).collect();
    let sig_dn: Vec<(f64, f64)> = app.signal_log.iter().filter(|s| s.side == "DOWN").filter_map(|s| snap(s.event_idx)).collect();
    let ord_up: Vec<(f64, f64)> = app.order_log.iter().filter(|o| o.side == "UP").filter_map(|o| snap(o.event_idx)).collect();
    let ord_dn: Vec<(f64, f64)> = app.order_log.iter().filter(|o| o.side == "DOWN").filter_map(|o| snap(o.event_idx)).collect();

    let all_y = data.iter().map(|d| d.1)
        .chain(vwap_data.iter().map(|d| d.1))
        .chain(sig_up.iter().map(|p| p.1))
        .chain(sig_dn.iter().map(|p| p.1))
        .chain(ord_up.iter().map(|p| p.1))
        .chain(ord_dn.iter().map(|p| p.1));
    let y_min = all_y.clone().fold(f64::MAX, f64::min).min(strike) - 50.0;
    let y_max = all_y.fold(f64::MIN, f64::max).max(strike) + 50.0;

    let mut datasets = vec![
        Dataset::default().name("BTC").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Yellow)).data(&data),
        Dataset::default().name("Strike").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Magenta)).data(&strike_data),
    ];

    if !vwap_data.is_empty() {
        datasets.push(Dataset::default().name("VWAP").marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line).style(Style::default().fg(Color::Cyan)).data(&vwap_data));
    }

    let scatter_sets: &[(&str, Color, symbols::Marker, &[(f64, f64)])] = &[
        ("Sig\u{25b2}", Color::DarkGray, symbols::Marker::Dot, &sig_up),
        ("Sig\u{25bc}", Color::White, symbols::Marker::Dot, &sig_dn),
        ("Ord\u{25b2}", Color::Cyan, symbols::Marker::Braille, &ord_up),
        ("Ord\u{25bc}", Color::LightRed, symbols::Marker::Braille, &ord_dn),
    ];
    for &(name, color, marker, pts) in scatter_sets {
        if !pts.is_empty() {
            datasets.push(Dataset::default().name(name).marker(marker)
                .graph_type(GraphType::Scatter).style(Style::default().fg(color)).data(pts));
        }
    }

    let chart = Chart::new(datasets)
        .block(Block::bordered().title("BTC Price").border_style(BORDER))
        .x_axis(Axis::default().bounds([x_min, x_max]).style(BORDER))
        .y_axis(Axis::default().bounds([y_min, y_max])
            .labels::<Vec<Line>>(vec![format!("{:.0}", y_min).into(), format!("{:.0}", strike).into(), format!("{:.0}", y_max).into()])
            .style(BORDER))
        .legend_position(Some(LegendPosition::TopRight))
        .hidden_legend_constraints((Constraint::Percentage(50), Constraint::Percentage(50)));

    Widget::render(chart, area, buf);
}

// ─── PM YES / NO Charts ───

fn render_pm_chart(app: &App, area: Rect, buf: &mut Buffer) {
    let [left, right] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .areas(area);

    render_pm_side(app, "YES (UP)", &app.up_bid_chart, &app.up_ask_chart, "UP", left, buf);
    render_pm_side(app, "NO (DN)", &app.down_bid_chart, &app.down_ask_chart, "DOWN", right, buf);
}

fn render_pm_side(
    app: &App,
    title: &str,
    bid_hist: &VecDeque<(f64, f64)>,
    ask_hist: &VecDeque<(f64, f64)>,
    side_filter: &str,
    area: Rect,
    buf: &mut Buffer,
) {
    if bid_hist.is_empty() && ask_hist.is_empty() {
        Widget::render(Block::bordered().title(title).border_style(BORDER), area, buf);
        return;
    }

    let bid_data: Vec<(f64, f64)> = bid_hist.iter().copied().collect();
    let ask_data: Vec<(f64, f64)> = ask_hist.iter().copied().collect();

    let x_min = bid_data.first().map(|d| d.0).into_iter()
        .chain(ask_data.first().map(|d| d.0)).fold(f64::MAX, f64::min);
    let x_max = bid_data.last().map(|d| d.0).into_iter()
        .chain(ask_data.last().map(|d| d.0)).fold(f64::MIN, f64::max);
    if x_min >= x_max { return; }

    // Fair value points per strategy
    let strat_names = ["latency_arb", "certainty_capture", "convexity_fade",
                       "cross_timeframe", "strike_misalign", "lp_extreme"];
    let fair_sets: Vec<(&str, Color, Vec<(f64, f64)>)> = strat_names.iter().filter_map(|&sname| {
        let pts: Vec<(f64, f64)> = app.signal_log.iter()
            .filter(|s| s.strategy == sname && s.side == side_filter)
            .filter(|s| (s.event_idx as f64) >= x_min && (s.event_idx as f64) <= x_max)
            .map(|s| (s.event_idx as f64, s.fair_value))
            .collect();
        if pts.is_empty() { None } else {
            let (label, color) = strategy_style(sname);
            Some((label, color, pts))
        }
    }).collect();

    let all_y = bid_data.iter().map(|d| d.1)
        .chain(ask_data.iter().map(|d| d.1))
        .chain(fair_sets.iter().flat_map(|(_, _, pts)| pts.iter().map(|p| p.1)));
    let y_min = (all_y.clone().fold(f64::MAX, f64::min) - 0.02).max(0.0);
    let y_max = (all_y.fold(f64::MIN, f64::max) + 0.02).min(1.0);

    let mut datasets = vec![
        Dataset::default().name("Bid").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Green)).data(&bid_data),
        Dataset::default().name("Ask").marker(symbols::Marker::Braille).graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Red)).data(&ask_data),
    ];

    for (label, color, ref pts) in &fair_sets {
        datasets.push(Dataset::default().name(*label).marker(symbols::Marker::Dot)
            .graph_type(GraphType::Scatter).style(Style::default().fg(*color)).data(pts));
    }

    let chart = Chart::new(datasets)
        .block(Block::bordered().title(title).border_style(BORDER))
        .x_axis(Axis::default().bounds([x_min, x_max]).style(BORDER))
        .y_axis(Axis::default().bounds([y_min, y_max])
            .labels::<Vec<Line>>(vec![format!("{:.2}", y_min).into(), format!("{:.2}", (y_min + y_max) / 2.0).into(), format!("{:.2}", y_max).into()])
            .style(BORDER));

    Widget::render(chart, area, buf);
}

// ─── Volume Sparklines ───

fn render_volume_sparklines(app: &App, area: Rect, buf: &mut Buffer) {
    let block = Block::bordered().title("BN Volume").border_style(BORDER);
    let inner = block.inner(area);
    Widget::render(block, area, buf);

    if inner.height < 2 { return; }

    let [top, bot] = Layout::vertical([
        Constraint::Length(inner.height / 2),
        Constraint::Min(1),
    ])
    .areas(inner);

    let buy: Vec<u64> = app.buy_vol_history.iter().copied().collect();
    let sell: Vec<u64> = app.sell_vol_history.iter().copied().collect();

    Sparkline::default().data(&buy).style(Style::default().fg(Color::Green)).render(top, buf);
    Sparkline::default().data(&sell).style(Style::default().fg(Color::Red)).render(bot, buf);
}

// ─── Metrics ───

fn render_metrics(app: &App, area: Rect, buf: &mut Buffer) {
    let s = &app.state;
    let now_ms = app.current_event_ts();

    let sigma = s.sigma_real();
    let tau = s.tau_eff_s(now_ms);
    let s_est = s.s_est();
    let k = s.info.strike;
    let can_compute = sigma > 0.0 && tau > 0.0 && s_est > 0.0 && k > 0.0;

    let pf    = if can_compute { p_fair(s_est, k, sigma, tau) } else { 0.0 };
    let z     = if can_compute { z_score(s_est, k, sigma, tau) } else { 0.0 };
    let delta = if can_compute { delta_bin(s_est, k, sigma, tau) } else { 0.0 };

    let regime = s.bn.regime.classify();
    let (regime_str, regime_color) = match regime {
        Regime::Range     => ("Range", Color::Cyan),
        Regime::Trend     => (if s.bn.regime.trend_direction_up() { "Trend UP" } else { "Trend DN" }, Color::Yellow),
        Regime::Ambiguous => ("Ambiguous", Color::DarkGray),
    };

    let dist = s.distance();
    let lines = vec![
        metric_line("σ    ", format!("{:.6}", sigma), Color::White),
        metric_line("z    ", format!("{:+.3}", z), if z.abs() > 2.0 { Color::Yellow } else { Color::White }),
        metric_line("fair ", format!("{:.4}", pf), Color::White),
        metric_line("Δ    ", format!("{:.6}", delta), Color::White),
        Line::from(vec![
            Span::styled("reg  ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{} ({:.0}%)", regime_str, s.bn.regime.dominant_frac() * 100.0), Style::default().fg(regime_color)),
        ]),
        metric_line("vwap ", format!("${:.1}", s.bn.vwap_tracker.vwap()), Color::White),
        Line::from(vec![
            Span::styled("dist ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("${:+.0}", dist), Style::default().fg(if dist >= 0.0 { Color::Green } else { Color::Red })),
        ]),
        Line::from(vec![
            Span::styled("ewma ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("n={}", s.bn.ewma_vol.n_samples()), Style::default().fg(if s.bn.ewma_vol.n_samples() >= 10 { Color::Green } else { Color::Red })),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("UP ", Style::default().fg(Color::Green).bold()),
            Span::styled(format!("s:{:.3} m:{:.3} i:{:.2}", s.up_book.spread(), s.up_book.microprice(), s.up_book.depth_imbalance(5)), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("DN ", Style::default().fg(Color::Red).bold()),
            Span::styled(format!("s:{:.3} m:{:.3} i:{:.2}", s.down_book.spread(), s.down_book.microprice(), s.down_book.depth_imbalance(5)), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("PM ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("U {:.2}/{:.2} D {:.2}/{:.2}", s.up_bid, s.up_ask, s.down_bid, s.down_ask), Style::default().fg(Color::White)),
        ]),
    ];

    Widget::render(
        Paragraph::new(lines).block(Block::bordered().title("Metrics").border_style(BORDER)),
        area, buf,
    );
}

fn metric_line<'a>(label: &'a str, value: String, color: Color) -> Line<'a> {
    Line::from(vec![
        Span::styled(label, Style::default().fg(Color::DarkGray)),
        Span::styled(value, Style::default().fg(color)),
    ])
}

// ─── Signal / Order tables ───

fn render_signals(app: &App, area: Rect, buf: &mut Buffer) {
    let start = app.signal_log.len().saturating_sub(50);
    let rows: Vec<Row> = app.signal_log[start..].iter().rev().map(|sig| {
        Row::new(vec![
            Cell::from(sig.strategy.as_str()).style(Style::default().fg(Color::White)),
            Cell::from(sig.side.as_str()).style(Style::default().fg(side_color(&sig.side))),
            Cell::from(format!("{:.3}", sig.edge)).style(Style::default().fg(Color::Yellow)),
            Cell::from(format!("{:.3}", sig.fair_value)),
            Cell::from(format!("{:.3}", sig.market_price)),
            Cell::from(format!("{:.0}s", sig.time_left_s)),
            Cell::from(if sig.is_passive { "P" } else { "A" }),
        ])
    }).collect();

    let widths = [
        Constraint::Length(16), Constraint::Length(5), Constraint::Length(6),
        Constraint::Length(6), Constraint::Length(6), Constraint::Length(6), Constraint::Length(2),
    ];
    let header = Row::new(vec!["Strategy", "Side", "Edge", "Fair", "Mkt", "T-left", "T"])
        .style(Style::default().fg(Color::Cyan).bold());

    Widget::render(
        Table::new(rows, widths).header(header)
            .block(Block::bordered().title(format!("Signals ({})", app.signal_log.len())).border_style(BORDER)),
        area, buf,
    );
}

fn render_orders(app: &App, area: Rect, buf: &mut Buffer) {
    let start = app.order_log.len().saturating_sub(30);
    let rows: Vec<Row> = app.order_log[start..].iter().rev().map(|ord| {
        Row::new(vec![
            Cell::from(format!("#{}", ord.id)).style(Style::default().fg(Color::DarkGray)),
            Cell::from(ord.strategy.as_str()).style(Style::default().fg(Color::White)),
            Cell::from(ord.side.as_str()).style(Style::default().fg(side_color(&ord.side))),
            Cell::from(format!("{:.3}", ord.price)),
            Cell::from(format!("${:.0}", ord.size)).style(Style::default().fg(Color::Yellow)),
            Cell::from(format!("{:.3}", ord.edge)),
            Cell::from(format!("{:.0}s", ord.time_left_s)),
            Cell::from(if ord.is_passive { "P" } else { "A" }),
        ])
    }).collect();

    let widths = [
        Constraint::Length(4), Constraint::Length(14), Constraint::Length(5),
        Constraint::Length(6), Constraint::Length(5), Constraint::Length(6),
        Constraint::Length(5), Constraint::Length(2),
    ];
    let header = Row::new(vec!["#", "Strategy", "Side", "Price", "Size", "Edge", "T-left", "T"])
        .style(Style::default().fg(Color::Cyan).bold());

    let house_str = match app.house_side {
        Some(Side::Up) => " house=UP",
        Some(Side::Down) => " house=DN",
        None => "",
    };

    Widget::render(
        Table::new(rows, widths).header(header)
            .block(Block::bordered().title(format!("Orders ({}){}", app.order_log.len(), house_str)).border_style(BORDER)),
        area, buf,
    );
}
