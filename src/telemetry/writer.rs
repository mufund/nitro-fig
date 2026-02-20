use std::fs::{self, File, OpenOptions};
use std::io::Write;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::telemetry::telegram::TelegramClient;
use crate::types::*;

/// Simple CSV writer that buffers writes.
struct CsvWriter {
    file: File,
}

impl CsvWriter {
    fn new(path: &str, header: &str) -> Self {
        let mut file = File::create(path).expect(&format!("Failed to create {}", path));
        writeln!(file, "{}", header).ok();
        Self { file }
    }

    fn flush(&mut self) {
        self.file.flush().ok();
    }
}

/// Single background task that handles ALL telemetry:
/// signals CSV, latency CSV, orders CSV, fills CSV, raw CLOB CSV, AND Telegram alerts.
/// Consolidates all I/O into one task that never touches the hot path.
///
/// Telegram sends are fire-and-forget (tokio::spawn) — a slow TG response
/// never blocks CSV writes or delays the next event.
pub async fn telemetry_writer(
    mut rx: mpsc::Receiver<TelemetryEvent>,
    config: Config,
    slug: String,
) {
    let dir = format!("logs/{}/{}", config.interval.label(), slug);
    fs::create_dir_all(&dir).ok();

    let signals_header = format!(
        "ts_ms,strategy,side,edge,fair,mkt,conf,size_frac,{},dist,time_left_s,eval_us,selected,sig_delta,sig_gamma,port_delta,port_gamma",
        config.asset
    );
    let mut signals_csv = CsvWriter::new(
        &format!("{}/signals.csv", dir),
        &signals_header,
    );
    let mut latency_csv = CsvWriter::new(
        &format!("{}/latency.csv", dir),
        "ts_ms,event,latency_us",
    );
    let mut orders_csv = CsvWriter::new(
        &format!("{}/orders.csv", dir),
        "ts_ms,order_id,side,price,size,strategy,edge,btc,time_left_s",
    );
    let mut fills_csv = CsvWriter::new(
        &format!("{}/fills.csv", dir),
        "ts_ms,order_id,strategy,status,filled_price,filled_size,submit_to_ack_ms,pnl_if_correct",
    );
    let mut clob_raw_csv = CsvWriter::new(
        &format!("{}/clob_raw.csv", dir),
        "ts_ms,order_id,direction,raw_json",
    );

    let tg = match (&config.tg_bot_token, &config.tg_chat_id) {
        (Some(token), Some(chat)) => {
            eprintln!("[TELEM] Telegram alerts enabled");
            Some(TelegramClient::new(token, chat, &config.asset_label()))
        }
        _ => {
            eprintln!("[TELEM] Telegram not configured, skipping alerts");
            None
        }
    };

    while let Some(event) = rx.recv().await {
        match event {
            TelemetryEvent::Signal(s) => {
                writeln!(
                    signals_csv.file,
                    "{},{},{:?},{:.4},{:.4},{:.4},{:.4},{:.4},{:.2},{:.2},{:.1},{},{},{:.6},{:.6},{:.6},{:.6}",
                    s.ts_ms, s.strategy, s.side, s.edge, s.fair_value, s.market_price,
                    s.confidence, s.size_frac, s.binance_price, s.distance,
                    s.time_left_s, s.eval_latency_us,
                    if s.selected { 1 } else { 0 },
                    s.signal_delta, s.signal_gamma,
                    s.portfolio_delta, s.portfolio_gamma,
                ).ok();
            }
            TelemetryEvent::Latency(l) => {
                writeln!(
                    latency_csv.file,
                    "{},{},{}",
                    l.ts_ms, l.event, l.latency_us,
                ).ok();
            }
            TelemetryEvent::OrderSent(o) => {
                writeln!(
                    orders_csv.file,
                    "{},{},{:?},{:.4},{:.2},{},{:.4},{:.2},{:.1}",
                    o.ts_ms, o.order_id, o.side, o.price, o.size,
                    o.strategy, o.edge_at_submit, o.binance_price, o.time_left_s,
                ).ok();
                // Fire-and-forget TG: never blocks CSV writes
                if let Some(tg) = &tg {
                    let tg = tg.clone();
                    let record = o.clone();
                    tokio::spawn(async move { tg.send_order_alert(&record).await; });
                }
            }
            TelemetryEvent::OrderResult(f) => {
                writeln!(
                    fills_csv.file,
                    "{},{},{},{},{},{},{:.3},{}",
                    f.ts_ms, f.order_id, f.strategy, f.status,
                    f.filled_price.map_or("".to_string(), |p| format!("{:.4}", p)),
                    f.filled_size.map_or("".to_string(), |s| format!("{:.2}", s)),
                    f.submit_to_ack_ms,
                    f.pnl_if_correct.map_or("".to_string(), |p| format!("{:.4}", p)),
                ).ok();
                // Fire-and-forget TG
                if let Some(tg) = &tg {
                    let tg = tg.clone();
                    let record = f.clone();
                    tokio::spawn(async move { tg.send_fill_alert(&record).await; });
                }
            }
            TelemetryEvent::MarketStart(m) => {
                eprintln!("[TELEM] Market started: {} strike=${:.0}", m.slug, m.strike);
                // Write market_info.txt
                let info_path = format!("{}/market_info.txt", dir);
                if let Ok(mut f) = File::create(&info_path) {
                    writeln!(f, "slug={}", m.slug).ok();
                    writeln!(f, "strike={:.2}", m.strike).ok();
                    writeln!(f, "start_ms={}", m.start_ms).ok();
                    writeln!(f, "end_ms={}", m.end_ms).ok();
                }
                // Fire-and-forget TG
                if let Some(tg) = &tg {
                    let tg = tg.clone();
                    let record = m.clone();
                    tokio::spawn(async move { tg.send_market_start(&record).await; });
                }
            }
            TelemetryEvent::StrategyMetrics(sm) => {
                // Log per-strategy metrics (non-blocking, off hot path)
                eprintln!(
                    "[METRICS] {} fills={} rate={:.2} adv_sel={:.3} win={:.2} avg_edge={:.4}",
                    sm.strategy, sm.fill_count, sm.fill_rate,
                    sm.adverse_selection, sm.win_rate, sm.avg_edge,
                );
                // Fire-and-forget TG
                if let Some(tg) = &tg {
                    let tg = tg.clone();
                    let record = sm.clone();
                    tokio::spawn(async move { tg.send_strategy_metrics(&record).await; });
                }
            }
            TelemetryEvent::MarketEnd(m) => {
                eprintln!(
                    "[TELEM] Market ended: {} outcome={:?} pnl=${:.2}",
                    m.slug, m.outcome, m.gross_pnl
                );

                // Append outcome + per-strategy data to market_info.txt
                let info_path = format!("{}/market_info.txt", dir);
                if let Ok(mut f) = OpenOptions::new().append(true).open(&info_path) {
                    writeln!(f, "final_binance_price={:.2}", m.final_binance_price).ok();
                    writeln!(f, "final_distance={:.2}", m.final_distance).ok();
                    writeln!(f, "outcome={}", m.outcome).ok();
                    writeln!(f, "total_signals={}", m.total_signals).ok();
                    writeln!(f, "total_orders={}", m.total_orders).ok();
                    writeln!(f, "total_filled={}", m.total_filled).ok();
                    writeln!(f, "gross_pnl={:.4}", m.gross_pnl).ok();
                    for ps in &m.per_strategy {
                        writeln!(
                            f, "strat_{}=sig:{},ord:{},fill:{},pnl:{:.4},avg_edge:{:.4}",
                            ps.strategy, ps.signals, ps.orders, ps.filled,
                            ps.gross_pnl, ps.avg_edge,
                        ).ok();
                    }
                }

                // Fire-and-forget TG
                if let Some(tg) = &tg {
                    let tg = tg.clone();
                    let record = m.clone();
                    tokio::spawn(async move { tg.send_market_summary(&record).await; });
                }
            }
            TelemetryEvent::RawClobResponse(r) => {
                // Escape raw_json for CSV (double-quote any commas/newlines)
                let escaped = r.raw_json.replace('"', "\"\"");
                writeln!(
                    clob_raw_csv.file,
                    "{},{},{},\"{}\"",
                    r.ts_ms, r.order_id, r.direction, escaped,
                ).ok();
            }
            TelemetryEvent::OrderRejectedLocal(r) => {
                eprintln!(
                    "[TELEM] Order #{} rejected locally: {} ({})",
                    r.order_id, r.reason, r.strategy
                );
                // Fire-and-forget TG alert
                if let Some(tg) = &tg {
                    let tg = tg.clone();
                    let record = r.clone();
                    tokio::spawn(async move {
                        tg.send_rejection_alert(record.order_id, &record.strategy, &record.reason).await;
                    });
                }
            }
        }
    }

    // Flush on shutdown
    signals_csv.flush();
    latency_csv.flush();
    orders_csv.flush();
    fills_csv.flush();
    clob_raw_csv.flush();
    eprintln!("[TELEM] Writer stopped, files flushed");
}
