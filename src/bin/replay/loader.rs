use std::collections::BTreeMap;

use crate::types::{BinanceCsvRow, BookSnapshot, LoadedMarketInfo, PmCsvRow, ReplayEvent};

// ─── CSV loaders ───

pub fn load_binance_csv(path: &str) -> Vec<BinanceCsvRow> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to read {}: {}", path, e);
            return vec![];
        }
    };
    content
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').collect();
            if f.len() < 6 {
                return None;
            }
            let ts_ms = f[2].parse::<i64>().ok()?;
            let price = f[3].parse::<f64>().ok()?;
            if ts_ms <= 0 || price <= 0.0 {
                return None;
            }
            Some(BinanceCsvRow {
                ts_ms,
                price,
                qty: f[4].parse().unwrap_or(0.0),
                is_buy: f[5].trim().to_lowercase() != "sell",
            })
        })
        .collect()
}

pub fn load_polymarket_csv(path: &str) -> Vec<PmCsvRow> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to read {}: {}", path, e);
            return vec![];
        }
    };
    content
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').collect();
            if f.len() < 8 {
                return None;
            }
            let ts_ms = f[1].parse::<i64>().ok()?;
            if ts_ms <= 0 {
                return None;
            }
            Some(PmCsvRow {
                ts_ms,
                up_bid: f[4].parse().unwrap_or(0.0),
                up_ask: f[5].parse().unwrap_or(0.0),
                down_bid: f[6].parse().unwrap_or(0.0),
                down_ask: f[7].parse().unwrap_or(0.0),
            })
        })
        .collect()
}

pub fn load_book_csv(path: &str) -> Vec<BookSnapshot> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("No book.csv found ({}), proceeding without book depth", e);
            return vec![];
        }
    };

    let mut grouped: BTreeMap<(i64, bool), (Vec<(f64, f64)>, Vec<(f64, f64)>)> = BTreeMap::new();

    for line in content.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 7 {
            continue;
        }
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
        let entry = grouped
            .entry((ts_ms, is_up))
            .or_insert_with(|| (Vec::new(), Vec::new()));
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
                    if start_ms == 0 {
                        start_ms = dt.timestamp_millis();
                    }
                }
            }
            "end" | "end_date" => {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(val) {
                    if end_ms == 0 {
                        end_ms = dt.timestamp_millis();
                    }
                }
            }
            _ => {}
        }
    }
    if slug.is_empty() {
        slug = "unknown".to_string();
    }
    LoadedMarketInfo {
        slug,
        start_ms,
        end_ms,
        strike,
    }
}

// ─── Event merging ───

pub fn merge_events(
    binance: &[BinanceCsvRow],
    pm: &[PmCsvRow],
    books: &[BookSnapshot],
) -> Vec<ReplayEvent> {
    let mut tagged: Vec<(i64, u8, usize)> =
        Vec::with_capacity(binance.len() + pm.len() + books.len());

    for (i, b) in binance.iter().enumerate() {
        tagged.push((b.ts_ms, 0, i));
    }
    for (i, p) in pm.iter().enumerate() {
        tagged.push((p.ts_ms, 1, i));
    }
    for (i, b) in books.iter().enumerate() {
        tagged.push((b.ts_ms, 2, i));
    }

    // Sort by timestamp; books before quotes at same timestamp
    tagged.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    tagged
        .into_iter()
        .map(|(_, typ, idx)| match typ {
            0 => {
                let b = &binance[idx];
                ReplayEvent::Binance {
                    ts_ms: b.ts_ms,
                    price: b.price,
                    qty: b.qty,
                    is_buy: b.is_buy,
                }
            }
            1 => {
                let p = &pm[idx];
                ReplayEvent::Polymarket {
                    ts_ms: p.ts_ms,
                    up_bid: p.up_bid,
                    up_ask: p.up_ask,
                    down_bid: p.down_bid,
                    down_ask: p.down_ask,
                }
            }
            _ => {
                let b = &books[idx];
                ReplayEvent::Book {
                    ts_ms: b.ts_ms,
                    is_up: b.is_up,
                    bids: b.bids.clone(),
                    asks: b.asks.clone(),
                }
            }
        })
        .collect()
}
