//! Offline per-strategy performance analyzer.
//!
//! Scans `logs/{interval}/*/` directories, reads CSVs + `market_info.txt`,
//! and computes aggregated per-strategy metrics.
//!
//! Usage:
//!   analyzer [OPTIONS]
//!     --logs-dir <path>           Base logs dir (default: "logs")
//!     --interval <5m|15m|1h|4h>  Filter by interval (default: all)
//!     --asset <btc|eth|sol|xrp>  Filter by asset (default: all)
//!     --since <YYYY-MM-DD>       Only markets after this date
//!     --verbose                   Show per-market detail

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

// ─── CLI Args ───────────────────────────────────────────────────────

struct Args {
    logs_dir: String,
    interval_filter: Option<String>,
    asset_filter: Option<String>,
    since_ms: Option<i64>,
    verbose: bool,
}

impl Args {
    fn from_cli() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let mut a = Args {
            logs_dir: "logs".into(),
            interval_filter: None,
            asset_filter: None,
            since_ms: None,
            verbose: false,
        };
        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--logs-dir" => {
                    i += 1;
                    a.logs_dir = args[i].clone();
                }
                "--interval" => {
                    i += 1;
                    a.interval_filter = Some(args[i].clone());
                }
                "--asset" => {
                    i += 1;
                    a.asset_filter = Some(args[i].to_lowercase());
                }
                "--since" => {
                    i += 1;
                    a.since_ms = parse_date_to_ms(&args[i]);
                }
                "--verbose" | "-v" => {
                    a.verbose = true;
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => {
                    eprintln!("Unknown arg: {}", other);
                    print_usage();
                    std::process::exit(1);
                }
            }
            i += 1;
        }
        a
    }
}

fn print_usage() {
    eprintln!(
        "Usage: analyzer [OPTIONS]\n\
         \n\
         Options:\n\
         \x20 --logs-dir <path>           Base logs directory (default: \"logs\")\n\
         \x20 --interval <5m|15m|1h|4h>  Filter by interval\n\
         \x20 --asset <btc|eth|sol|xrp>  Filter by asset\n\
         \x20 --since <YYYY-MM-DD>       Only markets after this date\n\
         \x20 --verbose, -v               Show per-market detail\n\
         \x20 --help, -h                  Show this help"
    );
}

fn parse_date_to_ms(s: &str) -> Option<i64> {
    // Parse YYYY-MM-DD → midnight UTC ms
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        eprintln!("Warning: cannot parse date '{}', expected YYYY-MM-DD", s);
        return None;
    }
    let y: i32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let d: u32 = parts[2].parse().ok()?;
    let dt = chrono::NaiveDate::from_ymd_opt(y, m, d)?;
    let ts = dt
        .and_hms_opt(0, 0, 0)?
        .and_utc()
        .timestamp_millis();
    Some(ts)
}

// ─── Data Structures ────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct MarketMeta {
    slug: String,
    #[allow(dead_code)]
    dir: PathBuf,
    strike: Option<f64>,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    outcome: Option<String>, // "UP" or "DOWN"
    gross_pnl: Option<f64>,
}

#[derive(Debug, Clone)]
struct OrderRow {
    order_id: u64,
    side: String,
    #[allow(dead_code)]
    price: f64,
    #[allow(dead_code)]
    size: f64,
    strategy: String,
    edge: f64,
}

#[derive(Debug, Clone)]
struct FillRow {
    order_id: u64,
    strategy: String, // may be empty in old format
    status: String,
    filled_price: Option<f64>,
    filled_size: Option<f64>,
    #[allow(dead_code)]
    pnl_if_correct: Option<f64>,
}

#[derive(Debug, Clone)]
struct SignalRow {
    strategy: String,
    selected: Option<bool>, // None for old format
}

#[derive(Default, Debug)]
struct StrategyAgg {
    markets: u32,
    signals_total: u32,
    signals_selected: u32,
    orders: u32,
    fills: u32,
    wins: u32,
    total_invested: f64,
    realized_pnl: f64,
    total_edge: f64,
    // track which market dirs contributed
    market_set: std::collections::HashSet<String>,
}

impl StrategyAgg {
    fn win_rate(&self) -> f64 {
        if self.fills > 0 {
            self.wins as f64 / self.fills as f64 * 100.0
        } else {
            0.0
        }
    }

    fn roi(&self) -> f64 {
        if self.total_invested > 0.0 {
            self.realized_pnl / self.total_invested * 100.0
        } else {
            0.0
        }
    }

    fn avg_edge(&self) -> f64 {
        if self.orders > 0 {
            self.total_edge / self.orders as f64
        } else {
            0.0
        }
    }

    fn selectivity(&self) -> f64 {
        if self.signals_total > 0 {
            self.signals_selected as f64 / self.signals_total as f64 * 100.0
        } else {
            0.0
        }
    }
}

// ─── CSV Parsing ────────────────────────────────────────────────────

/// Read lines from a CSV file, return (header_fields, data_lines).
fn read_csv(path: &Path) -> Option<(Vec<String>, Vec<String>)> {
    let content = fs::read_to_string(path).ok()?;
    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    if lines.is_empty() {
        return None;
    }
    let header_line = lines.remove(0);
    let headers: Vec<String> = header_line.split(',').map(|s| s.trim().to_string()).collect();
    // Remove empty trailing lines
    while lines.last().map_or(false, |l| l.trim().is_empty()) {
        lines.pop();
    }
    Some((headers, lines))
}

fn col_index(headers: &[String], name: &str) -> Option<usize> {
    headers.iter().position(|h| h == name)
}

fn parse_market_info(dir: &Path) -> MarketMeta {
    let info_path = dir.join("market_info.txt");
    let slug = dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut meta = MarketMeta {
        slug,
        dir: dir.to_path_buf(),
        strike: None,
        start_ms: None,
        end_ms: None,
        outcome: None,
        gross_pnl: None,
    };
    if let Ok(content) = fs::read_to_string(&info_path) {
        for line in content.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("strike=") {
                meta.strike = v.parse().ok();
            } else if let Some(v) = line.strip_prefix("start_ms=") {
                meta.start_ms = v.parse().ok();
            } else if let Some(v) = line.strip_prefix("end_ms=") {
                meta.end_ms = v.parse().ok();
            } else if let Some(v) = line.strip_prefix("outcome=") {
                meta.outcome = Some(v.to_string());
            } else if let Some(v) = line.strip_prefix("gross_pnl=") {
                meta.gross_pnl = v.parse().ok();
            }
        }
    }
    meta
}

fn parse_orders(dir: &Path) -> Vec<OrderRow> {
    let path = dir.join("orders.csv");
    let (headers, lines) = match read_csv(&path) {
        Some(v) => v,
        None => return vec![],
    };
    let i_id = col_index(&headers, "order_id");
    let i_side = col_index(&headers, "side");
    let i_price = col_index(&headers, "price");
    let i_size = col_index(&headers, "size");
    let i_strat = col_index(&headers, "strategy");
    let i_edge = col_index(&headers, "edge");

    lines
        .iter()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split(',').collect();
            Some(OrderRow {
                order_id: cols.get(i_id?)?.parse().ok()?,
                side: cols.get(i_side?)?.to_string(),
                price: cols.get(i_price?)?.parse().ok()?,
                size: cols.get(i_size?)?.parse().ok()?,
                strategy: cols.get(i_strat?)?.to_string(),
                edge: cols.get(i_edge?)?.parse().unwrap_or(0.0),
            })
        })
        .collect()
}

fn parse_fills(dir: &Path) -> Vec<FillRow> {
    let path = dir.join("fills.csv");
    let (headers, lines) = match read_csv(&path) {
        Some(v) => v,
        None => return vec![],
    };

    let i_id = col_index(&headers, "order_id");
    let i_strat = col_index(&headers, "strategy"); // may be None in old format
    let i_status = col_index(&headers, "status");
    let i_fprice = col_index(&headers, "filled_price");
    let i_fsize = col_index(&headers, "filled_size");
    let i_pnl = col_index(&headers, "pnl_if_correct");

    lines
        .iter()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split(',').collect();
            Some(FillRow {
                order_id: cols.get(i_id?)?.parse().ok()?,
                strategy: i_strat
                    .and_then(|i| cols.get(i))
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                status: cols.get(i_status?)?.to_string(),
                filled_price: i_fprice
                    .and_then(|i| cols.get(i))
                    .and_then(|s| s.parse().ok()),
                filled_size: i_fsize
                    .and_then(|i| cols.get(i))
                    .and_then(|s| s.parse().ok()),
                pnl_if_correct: i_pnl
                    .and_then(|i| cols.get(i))
                    .and_then(|s| s.parse().ok()),
            })
        })
        .collect()
}

fn parse_signals(dir: &Path) -> Vec<SignalRow> {
    let path = dir.join("signals.csv");
    let (headers, lines) = match read_csv(&path) {
        Some(v) => v,
        None => return vec![],
    };
    let i_strat = col_index(&headers, "strategy");
    let i_selected = col_index(&headers, "selected");

    lines
        .iter()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split(',').collect();
            Some(SignalRow {
                strategy: cols.get(i_strat?)?.to_string(),
                selected: i_selected.and_then(|i| {
                    cols.get(i).map(|s| s.trim() == "1" || s.trim().eq_ignore_ascii_case("true"))
                }),
            })
        })
        .collect()
}

// ─── Discovery ──────────────────────────────────────────────────────

/// Find all market directories under the logs base.
/// Structure: logs/{interval}/{slug}/
fn discover_markets(args: &Args) -> Vec<PathBuf> {
    let base = Path::new(&args.logs_dir);
    let mut dirs: Vec<PathBuf> = Vec::new();

    let interval_dirs: Vec<PathBuf> = match fs::read_dir(base) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(e) => {
            eprintln!("Cannot read logs dir '{}': {}", args.logs_dir, e);
            return dirs;
        }
    };

    for idir in &interval_dirs {
        let interval_name = idir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        // Filter by interval if specified
        if let Some(ref filt) = args.interval_filter {
            if &interval_name != filt {
                continue;
            }
        }

        // Filter by asset if specified (check slug prefix)
        if let Ok(rd) = fs::read_dir(idir) {
            for entry in rd.filter_map(|e| e.ok()) {
                let p = entry.path();
                if !p.is_dir() {
                    continue;
                }
                let slug = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();

                if let Some(ref asset) = args.asset_filter {
                    if !slug.starts_with(asset) {
                        continue;
                    }
                }

                dirs.push(p);
            }
        }
    }

    dirs.sort();
    dirs
}

// ─── Analysis ───────────────────────────────────────────────────────

fn analyze_market(
    dir: &Path,
    strats: &mut HashMap<String, StrategyAgg>,
    args: &Args,
) -> Option<MarketMeta> {
    let meta = parse_market_info(dir);

    // Filter by date
    if let Some(since) = args.since_ms {
        if let Some(start) = meta.start_ms {
            if start < since {
                return None;
            }
        }
    }

    let orders = parse_orders(dir);
    let fills = parse_fills(dir);
    let signals = parse_signals(dir);

    // Build order_id → strategy lookup from orders.csv
    let order_strat: HashMap<u64, String> = orders
        .iter()
        .map(|o| (o.order_id, o.strategy.clone()))
        .collect();

    // Build order_id → side lookup
    let order_side: HashMap<u64, String> = orders
        .iter()
        .map(|o| (o.order_id, o.side.clone()))
        .collect();

    let dir_key = dir.to_string_lossy().to_string();

    // ── Signals ──
    for sig in &signals {
        let entry = strats.entry(sig.strategy.clone()).or_default();
        entry.signals_total += 1;
        match sig.selected {
            Some(true) => entry.signals_selected += 1,
            None => entry.signals_selected += 1, // old format: all were selected
            _ => {}
        }
    }

    // ── Orders ──
    for o in &orders {
        let entry = strats.entry(o.strategy.clone()).or_default();
        entry.orders += 1;
        entry.total_edge += o.edge;
        entry.market_set.insert(dir_key.clone());
    }

    // ── Fills ──
    for f in &fills {
        // Determine strategy: prefer fills.csv column, fallback to orders.csv join
        let strategy = if !f.strategy.is_empty() {
            f.strategy.clone()
        } else {
            order_strat
                .get(&f.order_id)
                .cloned()
                .unwrap_or_else(|| "unknown".into())
        };

        let entry = strats.entry(strategy).or_default();

        if f.status.contains("Filled") || f.status.contains("PartialFill") {
            entry.fills += 1;
            entry.market_set.insert(dir_key.clone());

            if let (Some(fp), Some(fs)) = (f.filled_price, f.filled_size) {
                entry.total_invested += fp * fs;

                // Determine if fill was a win
                if let Some(ref outcome) = meta.outcome {
                    let fill_side = order_side
                        .get(&f.order_id)
                        .map(|s| s.to_string())
                        .unwrap_or_default();

                    let is_win = side_matches(&fill_side, outcome);
                    if is_win {
                        entry.wins += 1;
                        entry.realized_pnl += (1.0 - fp) * fs;
                    } else {
                        entry.realized_pnl -= fp * fs;
                    }
                }
            }
        }
    }

    if args.verbose && (!orders.is_empty() || !signals.is_empty()) {
        print_market_detail(&meta, &orders, &fills, &signals);
    }

    Some(meta)
}

/// Check if an order side matches the market outcome.
/// Side in CSV can be "Up", "Down", "UP", "DOWN", or Debug format.
fn side_matches(side_str: &str, outcome: &str) -> bool {
    let s = side_str.to_uppercase().replace("\"", "");
    let o = outcome.to_uppercase();
    s == o
}

fn print_market_detail(
    meta: &MarketMeta,
    orders: &[OrderRow],
    fills: &[FillRow],
    signals: &[SignalRow],
) {
    let outcome_str = meta.outcome.as_deref().unwrap_or("?");
    let pnl_str = meta
        .gross_pnl
        .map_or("?".into(), |p| format!("{:+.2}", p));
    println!(
        "\n  {} │ outcome={} │ pnl=${} │ sigs={} ord={} fills={}",
        meta.slug,
        outcome_str,
        pnl_str,
        signals.len(),
        orders.len(),
        fills.len(),
    );

    // Group orders by strategy
    let mut by_strat: HashMap<&str, (u32, u32, f64)> = HashMap::new();
    for o in orders {
        let e = by_strat.entry(&o.strategy).or_insert((0, 0, 0.0));
        e.0 += 1;
        e.2 += o.edge;
    }
    for f in fills {
        let strat = if !f.strategy.is_empty() {
            &f.strategy
        } else {
            continue;
        };
        if let Some(e) = by_strat.get_mut(strat.as_str()) {
            e.1 += 1;
        }
    }
    for (strat, (ords, fls, tot_edge)) in &by_strat {
        let avg = if *ords > 0 {
            tot_edge / *ords as f64
        } else {
            0.0
        };
        println!(
            "    {:20} │ ord={} fill={} avg_edge={:.3}",
            strat, ords, fls, avg
        );
    }
}

// ─── Display ────────────────────────────────────────────────────────

fn print_report(
    strats: &HashMap<String, StrategyAgg>,
    total_markets: usize,
    args: &Args,
    date_range: &str,
) {
    let interval_label = args
        .interval_filter
        .as_deref()
        .unwrap_or("ALL");
    let asset_label = args
        .asset_filter
        .as_ref()
        .map(|s| s.to_uppercase())
        .unwrap_or_else(|| "ALL".into());

    println!();
    println!("╔═══════════════════════════════════════════════════════════════════════════╗");
    println!(
        "║  Strategy Performance Report                                              ║"
    );
    println!(
        "║  Interval: {:4} │ Asset: {:4} │ Markets: {:4} ({})  ║",
        interval_label, asset_label, total_markets, date_range,
    );
    println!("╚═══════════════════════════════════════════════════════════════════════════╝");
    println!();

    // Header
    println!(
        "{:20} │ {:>4} │ {:>6} │ {:>6} │ {:>5} │ {:>5} │ {:>8} │ {:>6} │ {:>7} │ {:>5}",
        "Strategy", "Mkts", "Sigs", "Orders", "Fills", "Win%", "PnL($)", "ROI%", "AvgEdge", "Sel%"
    );
    println!(
        "{}┼{}┼{}┼{}┼{}┼{}┼{}┼{}┼{}┼{}",
        "─".repeat(20),
        "─".repeat(6),
        "─".repeat(8),
        "─".repeat(8),
        "─".repeat(7),
        "─".repeat(7),
        "─".repeat(10),
        "─".repeat(8),
        "─".repeat(9),
        "─".repeat(7),
    );

    // Sort strategies by name for consistent output
    let mut strat_names: Vec<&String> = strats.keys().collect();
    strat_names.sort();

    let mut tot_mkts: u32 = 0;
    let mut tot_sigs: u32 = 0;
    let mut tot_orders: u32 = 0;
    let mut tot_fills: u32 = 0;
    let mut tot_wins: u32 = 0;
    let mut tot_invested: f64 = 0.0;
    let mut tot_pnl: f64 = 0.0;
    let mut tot_edge_sum: f64 = 0.0;
    let mut tot_sigs_sel: u32 = 0;

    for name in &strat_names {
        if name.as_str() == "unknown" {
            continue; // skip unattributed
        }
        let s = &strats[*name];
        let mkts = s.market_set.len() as u32;
        println!(
            "{:20} │ {:>4} │ {:>6} │ {:>6} │ {:>5} │ {:>4.1}% │ {:>+8.2} │ {:>5.1}% │ {:>7.3} │ {:>4.1}%",
            name,
            mkts,
            s.signals_total,
            s.orders,
            s.fills,
            s.win_rate(),
            s.realized_pnl,
            s.roi(),
            s.avg_edge(),
            s.selectivity(),
        );
        tot_mkts = tot_mkts.max(mkts); // markets overlap, so max not sum
        tot_sigs += s.signals_total;
        tot_orders += s.orders;
        tot_fills += s.fills;
        tot_wins += s.wins;
        tot_invested += s.total_invested;
        tot_pnl += s.realized_pnl;
        tot_edge_sum += s.total_edge;
        tot_sigs_sel += s.signals_selected;
    }

    // Total row
    println!(
        "{}┼{}┼{}┼{}┼{}┼{}┼{}┼{}┼{}┼{}",
        "─".repeat(20),
        "─".repeat(6),
        "─".repeat(8),
        "─".repeat(8),
        "─".repeat(7),
        "─".repeat(7),
        "─".repeat(10),
        "─".repeat(8),
        "─".repeat(9),
        "─".repeat(7),
    );
    let total_wr = if tot_fills > 0 {
        tot_wins as f64 / tot_fills as f64 * 100.0
    } else {
        0.0
    };
    let total_roi = if tot_invested > 0.0 {
        tot_pnl / tot_invested * 100.0
    } else {
        0.0
    };
    let total_avg_edge = if tot_orders > 0 {
        tot_edge_sum / tot_orders as f64
    } else {
        0.0
    };
    let total_sel = if tot_sigs > 0 {
        tot_sigs_sel as f64 / tot_sigs as f64 * 100.0
    } else {
        0.0
    };
    println!(
        "{:20} │ {:>4} │ {:>6} │ {:>6} │ {:>5} │ {:>4.1}% │ {:>+8.2} │ {:>5.1}% │ {:>7.3} │ {:>4.1}%",
        "TOTAL",
        total_markets,
        tot_sigs,
        tot_orders,
        tot_fills,
        total_wr,
        tot_pnl,
        total_roi,
        total_avg_edge,
        total_sel,
    );
    println!();

    // Warn about unattributed
    if let Some(unk) = strats.get("unknown") {
        if unk.fills > 0 {
            eprintln!(
                "⚠  {} fills could not be attributed to a strategy (old CSV format)",
                unk.fills
            );
        }
    }
}

// ─── Main ───────────────────────────────────────────────────────────

fn main() {
    let args = Args::from_cli();

    let market_dirs = discover_markets(&args);
    if market_dirs.is_empty() {
        eprintln!("No market directories found in '{}'", args.logs_dir);
        eprintln!("Expected structure: {}/{{interval}}/{{slug}}/", args.logs_dir);
        std::process::exit(1);
    }

    eprintln!("Found {} market directories", market_dirs.len());

    let mut strats: HashMap<String, StrategyAgg> = HashMap::new();
    let mut total_markets = 0usize;
    let mut skipped_no_outcome = 0usize;
    let mut earliest_ms: Option<i64> = None;
    let mut latest_ms: Option<i64> = None;

    for dir in &market_dirs {
        match analyze_market(dir, &mut strats, &args) {
            Some(meta) => {
                total_markets += 1;
                if meta.outcome.is_none() {
                    skipped_no_outcome += 1;
                }
                if let Some(start) = meta.start_ms {
                    earliest_ms = Some(earliest_ms.map_or(start, |e: i64| e.min(start)));
                    latest_ms = Some(latest_ms.map_or(start, |l: i64| l.max(start)));
                }
            }
            None => {} // filtered out
        }
    }

    // Date range string
    let date_range = match (earliest_ms, latest_ms) {
        (Some(e), Some(l)) => {
            let e_dt = chrono::DateTime::from_timestamp_millis(e)
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "?".into());
            let l_dt = chrono::DateTime::from_timestamp_millis(l)
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "?".into());
            format!("{}..{}", e_dt, l_dt)
        }
        _ => "no dates".into(),
    };

    // Finalize market counts from market_set
    for s in strats.values_mut() {
        s.markets = s.market_set.len() as u32;
    }

    print_report(&strats, total_markets, &args, &date_range);

    if skipped_no_outcome > 0 {
        eprintln!(
            "⚠  {} markets had no outcome data (old format or incomplete). \
             Win rate and PnL are based on markets with outcomes only.",
            skipped_no_outcome
        );
    }
}
