use crate::config::Config;
use crate::types::MarketInfo;

/// Discover the current or next Up/Down market via Gamma API.
///
/// Strategy: compute the expected slug from current timestamp + config.
/// Slug format: {asset}-updown-{interval}-{unix_start}
/// where unix_start is the window boundary (divisible by window_secs).
///
/// We try the current window first, then the next window.
/// If neither exists in Gamma API, fall back to series_id search.
/// Note: 1h markets use human-readable slugs (e.g. "bitcoin-up-or-down-february-16-3am-et")
/// so slug-based discovery won't work — series_id fallback handles those.
pub async fn discover_next_market(
    client: &reqwest::Client,
    config: &Config,
) -> Result<MarketInfo, String> {
    let now_s = chrono::Utc::now().timestamp();
    let ws = config.interval.window_secs();

    // Compute the current and next window boundaries
    let current_window_start = (now_s / ws) * ws;
    let next_window_start = current_window_start + ws;

    // Try current window first (might be mid-trade)
    let candidates = [current_window_start, next_window_start];

    for &window_start in &candidates {
        let slug = format!("{}{}", config.slug_prefix(), window_start);
        eprintln!("[DISCOVERY] Trying slug: {}", slug);

        match fetch_event_by_slug(client, &config.gamma_api_url, &slug, config.interval.window_ms()).await {
            Ok(Some(market)) => {
                let now_ms = chrono::Utc::now().timestamp_millis();
                let wait_s = (market.start_ms - now_ms) as f64 / 1000.0;
                let remaining_s = (market.end_ms - now_ms) as f64 / 1000.0;
                eprintln!(
                    "[DISCOVERY] Found: {} | start in {:.0}s | remaining {:.0}s | UP={}... DOWN={}...",
                    market.slug,
                    wait_s,
                    remaining_s,
                    &market.up_token_id[..8.min(market.up_token_id.len())],
                    &market.down_token_id[..8.min(market.down_token_id.len())],
                );

                // Skip if market already ended
                if market.end_ms < now_ms {
                    eprintln!("[DISCOVERY] Market already ended, skipping");
                    continue;
                }

                return Ok(market);
            }
            Ok(None) => {
                eprintln!("[DISCOVERY] Slug {} not found", slug);
            }
            Err(e) => {
                eprintln!("[DISCOVERY] Error fetching {}: {}", slug, e);
            }
        }
    }

    // Fallback: try series_id search for pre-created future markets
    eprintln!("[DISCOVERY] Slug lookup failed, falling back to series_id={} search", config.series_id);
    discover_via_series(client, config).await
}

/// Fetch a single event by exact slug match
async fn fetch_event_by_slug(
    client: &reqwest::Client,
    gamma_api_url: &str,
    slug: &str,
    window_ms: i64,
) -> Result<Option<MarketInfo>, String> {
    let url = format!("{}/events?slug={}", gamma_api_url, slug);

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    let text = resp
        .text()
        .await
        .map_err(|e| format!("Body error: {}", e))?;

    let events: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("JSON error: {}", e))?;

    let events_arr = match events.as_array() {
        Some(arr) => arr,
        None => return Ok(None),
    };

    if events_arr.is_empty() {
        return Ok(None);
    }

    let event = &events_arr[0];
    parse_event_to_market_info(event, slug, window_ms)
}

/// Parse a Gamma API event JSON into MarketInfo
fn parse_event_to_market_info(
    event: &serde_json::Value,
    slug: &str,
    window_ms: i64,
) -> Result<Option<MarketInfo>, String> {
    let markets = match event.get("markets").and_then(|m| m.as_array()) {
        Some(m) if !m.is_empty() => m,
        _ => return Ok(None),
    };

    // Parse endDate
    let end_date = event
        .get("endDate")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let end_ms = parse_datetime_ms(end_date).unwrap_or(0);

    // Extract start_ms from slug (the unix timestamp at the end)
    let start_ms = slug
        .rsplit('-')
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .map(|ts| if ts > 1_000_000_000_000 { ts } else { ts * 1000 })
        .unwrap_or(0);

    // Fallback: derive start_ms from end_ms minus window duration
    let start_ms = if start_ms > 0 {
        start_ms
    } else if end_ms > 0 {
        end_ms - window_ms
    } else {
        0
    };

    if start_ms == 0 || end_ms == 0 {
        return Ok(None);
    }

    // Find UP and DOWN token IDs
    let (up_token, down_token) = extract_token_ids(markets);

    if up_token.is_empty() || down_token.is_empty() {
        eprintln!(
            "[DISCOVERY] Skipping {} — can't find UP/DOWN tokens",
            slug
        );
        return Ok(None);
    }

    Ok(Some(MarketInfo {
        slug: slug.to_string(),
        start_ms,
        end_ms,
        up_token_id: up_token,
        down_token_id: down_token,
        strike: 0.0,
    }))
}

/// Extract UP and DOWN token IDs from the markets array.
/// Handles both 2-market format and 1-market with JSON array tokens.
fn extract_token_ids(markets: &[serde_json::Value]) -> (String, String) {
    let mut up_token = String::new();
    let mut down_token = String::new();

    // Format 1: two separate markets with groupItemTitle
    if markets.len() == 2 {
        for market in markets {
            let outcome = market
                .get("groupItemTitle")
                .or_else(|| market.get("outcome"))
                .and_then(|o| o.as_str())
                .unwrap_or("")
                .to_lowercase();

            let token_id = extract_first_token_id(market);

            if outcome.contains("up") || outcome.contains("yes") || outcome.contains("higher") {
                up_token = token_id;
            } else if outcome.contains("down")
                || outcome.contains("no")
                || outcome.contains("lower")
            {
                down_token = token_id;
            }
        }
    }

    if up_token.is_empty() || down_token.is_empty() {
        // Format 2: single market with outcomes and clobTokenIds as JSON array strings
        // outcomes: ["Up", "Down"], clobTokenIds: ["token1", "token2"]
        // Token order matches outcome order
        for market in markets {
            let outcomes_str = market
                .get("outcomes")
                .and_then(|o| o.as_str())
                .unwrap_or("");
            let tokens_str = market
                .get("clobTokenIds")
                .and_then(|t| t.as_str())
                .unwrap_or("");

            if let (Ok(outcomes), Ok(tokens)) = (
                serde_json::from_str::<Vec<String>>(outcomes_str),
                serde_json::from_str::<Vec<String>>(tokens_str),
            ) {
                for (outcome, token) in outcomes.iter().zip(tokens.iter()) {
                    let lower = outcome.to_lowercase();
                    if lower.contains("up") || lower.contains("yes") || lower.contains("higher") {
                        up_token = token.clone();
                    } else if lower.contains("down")
                        || lower.contains("no")
                        || lower.contains("lower")
                    {
                        down_token = token.clone();
                    }
                }
            }
        }
    }

    (up_token, down_token)
}

/// Fallback discovery: search by series_id for pre-created future markets.
async fn discover_via_series(
    client: &reqwest::Client,
    config: &Config,
) -> Result<MarketInfo, String> {
    let url = format!(
        "{}/events?series_id={}&active=true&closed=false&limit=100&order=endDate&ascending=false",
        config.gamma_api_url, config.series_id,
    );

    eprintln!("[DISCOVERY] Fetching {}", url);

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    let text = resp
        .text()
        .await
        .map_err(|e| format!("Body error: {}", e))?;

    let events: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("JSON error: {}", e))?;

    let events_arr = events.as_array().ok_or("Expected array of events")?;

    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut best: Option<MarketInfo> = None;
    let mut best_start: i64 = i64::MAX;
    let window_ms = config.interval.window_ms();

    for event in events_arr {
        let slug = event
            .get("slug")
            .and_then(|s| s.as_str())
            .unwrap_or("");

        match parse_event_to_market_info(event, slug, window_ms) {
            Ok(Some(market)) if market.end_ms >= now_ms && market.start_ms < best_start => {
                best_start = market.start_ms;
                best = Some(market);
            }
            _ => continue,
        }
    }

    if let Some(ref m) = best {
        let wait_s = (m.start_ms - now_ms) as f64 / 1000.0;
        eprintln!(
            "[DISCOVERY] Fallback found: {} | start in {:.0}s",
            m.slug, wait_s
        );
    }

    best.ok_or_else(|| {
        format!(
            "No active {} {} market found",
            config.asset_label(),
            config.interval.label()
        )
    })
}

/// Extract the first token ID from a market object.
/// Handles both plain string and JSON array string formats.
fn extract_first_token_id(market: &serde_json::Value) -> String {
    let raw = market.get("clobTokenIds");
    if let Some(raw) = raw {
        if let Some(s) = raw.as_str() {
            if s.starts_with('[') {
                if let Ok(tokens) = serde_json::from_str::<Vec<String>>(s) {
                    return tokens.into_iter().next().unwrap_or_default();
                }
            }
            return s.to_string();
        }
        if let Some(arr) = raw.as_array() {
            return arr
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
        }
    }
    String::new()
}

fn parse_datetime_ms(s: &str) -> Option<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ") {
        return Some(dt.and_utc().timestamp_millis());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.fZ") {
        return Some(dt.and_utc().timestamp_millis());
    }
    if let Ok(ts) = s.parse::<i64>() {
        if ts > 1_000_000_000_000 {
            return Some(ts);
        } else {
            return Some(ts * 1000);
        }
    }
    None
}
