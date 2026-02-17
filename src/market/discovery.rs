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

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_datetime_ms tests ──

    /// Scenario: parse_datetime_ms receives a standard RFC3339 date string with Z suffix
    /// Expected: returns the correct millisecond timestamp (seconds * 1000)
    #[test]
    fn test_parse_datetime_rfc3339() {
        let ms = parse_datetime_ms("2024-01-15T12:00:00Z").unwrap();
        // 2024-01-15 12:00:00 UTC = 1705320000 seconds
        assert_eq!(ms, 1705320000 * 1000);
    }

    /// Scenario: parse_datetime_ms receives an RFC3339 date with explicit +00:00 offset
    /// Expected: returns the same timestamp as the Z suffix variant
    #[test]
    fn test_parse_datetime_rfc3339_with_offset() {
        let ms = parse_datetime_ms("2024-01-15T12:00:00+00:00").unwrap();
        assert_eq!(ms, 1705320000 * 1000);
    }

    /// Scenario: parse_datetime_ms receives a numeric string representing Unix seconds
    /// Expected: detects the value is in seconds (< 1 trillion) and converts to milliseconds
    #[test]
    fn test_parse_datetime_unix_seconds() {
        let ms = parse_datetime_ms("1700000000").unwrap();
        assert_eq!(ms, 1700000000 * 1000);
    }

    /// Scenario: parse_datetime_ms receives a numeric string representing Unix milliseconds
    /// Expected: detects the value is already in milliseconds (>= 1 trillion) and returns it as-is
    #[test]
    fn test_parse_datetime_unix_millis() {
        let ms = parse_datetime_ms("1700000000000").unwrap();
        assert_eq!(ms, 1700000000000);
    }

    /// Scenario: parse_datetime_ms receives a non-date, non-numeric string
    /// Expected: returns None because no parsing strategy can handle it
    #[test]
    fn test_parse_datetime_invalid() {
        assert!(parse_datetime_ms("not-a-date").is_none());
    }

    // ── extract_token_ids tests ──

    /// Scenario: two separate market objects with groupItemTitle "Up"/"Down" and JSON array clobTokenIds
    /// Expected: extracts the correct Up and Down token IDs from the multi-market format
    #[test]
    fn test_extract_token_ids_two_market_format() {
        let markets: Vec<serde_json::Value> = serde_json::from_str(r#"[
            {"groupItemTitle": "Up", "clobTokenIds": "[\"up-tok-123\"]"},
            {"groupItemTitle": "Down", "clobTokenIds": "[\"down-tok-456\"]"}
        ]"#).unwrap();
        let (up, down) = extract_token_ids(&markets);
        assert_eq!(up, "up-tok-123");
        assert_eq!(down, "down-tok-456");
    }

    /// Scenario: single market object with outcomes and clobTokenIds as JSON array strings
    /// Expected: parses paired outcomes/tokens arrays and maps Up/Down to correct token IDs
    #[test]
    fn test_extract_token_ids_single_market_format() {
        let markets: Vec<serde_json::Value> = serde_json::from_str(r#"[
            {
                "outcomes": "[\"Up\",\"Down\"]",
                "clobTokenIds": "[\"token-up\",\"token-down\"]"
            }
        ]"#).unwrap();
        let (up, down) = extract_token_ids(&markets);
        assert_eq!(up, "token-up");
        assert_eq!(down, "token-down");
    }

    /// Scenario: clobTokenIds is a JSON array encoded as a string (e.g. "[\"tok1\",\"tok2\"]")
    /// Expected: deserializes the string and returns the first token ID
    #[test]
    fn test_extract_first_token_id_json_array_string() {
        let market: serde_json::Value = serde_json::from_str(
            r#"{"clobTokenIds": "[\"tok1\",\"tok2\"]"}"#
        ).unwrap();
        assert_eq!(extract_first_token_id(&market), "tok1");
    }

    /// Scenario: clobTokenIds is a plain string value (not a JSON array)
    /// Expected: returns the raw string as the token ID
    #[test]
    fn test_extract_first_token_id_plain_string() {
        let market: serde_json::Value = serde_json::from_str(
            r#"{"clobTokenIds": "plain-token-id"}"#
        ).unwrap();
        assert_eq!(extract_first_token_id(&market), "plain-token-id");
    }

    /// Scenario: clobTokenIds is a native JSON array (not a string-encoded array)
    /// Expected: reads the first element from the array directly
    #[test]
    fn test_extract_first_token_id_json_array() {
        let market: serde_json::Value = serde_json::from_str(
            r#"{"clobTokenIds": ["arr-tok1", "arr-tok2"]}"#
        ).unwrap();
        assert_eq!(extract_first_token_id(&market), "arr-tok1");
    }

    /// Scenario: market object has no clobTokenIds field at all
    /// Expected: returns an empty string since there is no token to extract
    #[test]
    fn test_extract_first_token_id_missing() {
        let market: serde_json::Value = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(extract_first_token_id(&market), "");
    }

    // ── parse_event_to_market_info tests ──

    /// Scenario: valid event with endDate, two Up/Down markets, and a slug containing a Unix timestamp
    /// Expected: returns a MarketInfo with correct slug, start_ms from slug, end_ms from endDate, and token IDs
    #[test]
    fn test_parse_event_happy_path() {
        let event: serde_json::Value = serde_json::from_str(r#"{
            "endDate": "2024-01-15T12:05:00Z",
            "markets": [
                {"groupItemTitle": "Up", "clobTokenIds": "[\"up-abc\"]"},
                {"groupItemTitle": "Down", "clobTokenIds": "[\"down-xyz\"]"}
            ]
        }"#).unwrap();
        let slug = "btc-updown-5m-1705320000";
        let result = parse_event_to_market_info(&event, slug, 300_000).unwrap();
        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.slug, slug);
        assert_eq!(info.start_ms, 1705320000 * 1000);
        assert_eq!(info.up_token_id, "up-abc");
        assert_eq!(info.down_token_id, "down-xyz");
    }

    /// Scenario: event has a valid endDate but an empty markets array
    /// Expected: returns None because there are no markets to extract tokens from
    #[test]
    fn test_parse_event_no_markets() {
        let event: serde_json::Value = serde_json::from_str(r#"{
            "endDate": "2024-01-15T12:05:00Z",
            "markets": []
        }"#).unwrap();
        let result = parse_event_to_market_info(&event, "slug-123", 300_000).unwrap();
        assert!(result.is_none());
    }

    // ── parse_datetime_ms edge cases ──

    /// Scenario: parse_datetime_ms receives an RFC3339 date with fractional seconds (.500)
    /// Expected: preserves sub-second precision, returning base timestamp plus 500ms
    #[test]
    fn test_parse_datetime_fractional_seconds() {
        let ms = parse_datetime_ms("2024-01-15T12:00:00.500Z").unwrap();
        assert_eq!(ms, 1705320000 * 1000 + 500);
    }

    /// Scenario: parse_datetime_ms receives an empty string
    /// Expected: returns None because no format can match an empty input
    #[test]
    fn test_parse_datetime_empty_string() {
        assert!(parse_datetime_ms("").is_none());
    }

    /// Scenario: parse_datetime_ms receives an RFC3339 date with a negative UTC offset (-05:00)
    /// Expected: correctly converts to UTC, producing the same timestamp as the equivalent Z time
    #[test]
    fn test_parse_datetime_negative_timezone() {
        let ms = parse_datetime_ms("2024-01-15T07:00:00-05:00").unwrap();
        // 7am EST = 12:00 UTC = 1705320000
        assert_eq!(ms, 1705320000 * 1000);
    }

    /// Scenario: parse_datetime_ms receives a small Unix seconds value (year 2001)
    /// Expected: still treated as seconds (< 1 trillion threshold) and multiplied by 1000
    #[test]
    fn test_parse_datetime_small_unix_seconds() {
        // Small unix timestamp (year 2001)
        let ms = parse_datetime_ms("1000000000").unwrap();
        assert_eq!(ms, 1000000000 * 1000);
    }

    // ── extract_token_ids edge cases ──

    /// Scenario: two-market format with Down listed before Up in the array
    /// Expected: matches by keyword regardless of array order, assigning tokens correctly
    #[test]
    fn test_extract_token_ids_reversed_order() {
        // Down market listed before Up — should still parse correctly
        let markets: Vec<serde_json::Value> = serde_json::from_str(r#"[
            {"groupItemTitle": "Down", "clobTokenIds": "[\"down-first\"]"},
            {"groupItemTitle": "Up", "clobTokenIds": "[\"up-second\"]"}
        ]"#).unwrap();
        let (up, down) = extract_token_ids(&markets);
        assert_eq!(up, "up-second");
        assert_eq!(down, "down-first");
    }

    /// Scenario: markets use "Higher"/"Lower" as groupItemTitle instead of "Up"/"Down"
    /// Expected: keyword matching recognizes "higher" as Up and "lower" as Down
    #[test]
    fn test_extract_token_ids_higher_lower_keywords() {
        let markets: Vec<serde_json::Value> = serde_json::from_str(r#"[
            {"groupItemTitle": "Higher", "clobTokenIds": "[\"higher-tok\"]"},
            {"groupItemTitle": "Lower", "clobTokenIds": "[\"lower-tok\"]"}
        ]"#).unwrap();
        let (up, down) = extract_token_ids(&markets);
        assert_eq!(up, "higher-tok");
        assert_eq!(down, "lower-tok");
    }

    /// Scenario: markets use "Yes"/"No" as groupItemTitle instead of "Up"/"Down"
    /// Expected: keyword matching recognizes "yes" as Up and "no" as Down
    #[test]
    fn test_extract_token_ids_yes_no_keywords() {
        let markets: Vec<serde_json::Value> = serde_json::from_str(r#"[
            {"groupItemTitle": "Yes", "clobTokenIds": "[\"yes-tok\"]"},
            {"groupItemTitle": "No", "clobTokenIds": "[\"no-tok\"]"}
        ]"#).unwrap();
        let (up, down) = extract_token_ids(&markets);
        assert_eq!(up, "yes-tok");
        assert_eq!(down, "no-tok");
    }

    /// Scenario: two markets with groupItemTitles that don't match any known keyword (Up/Down/Yes/No/Higher/Lower)
    /// Expected: both Up and Down tokens remain empty since no outcome is recognized
    #[test]
    fn test_extract_token_ids_unrecognized_outcomes() {
        let markets: Vec<serde_json::Value> = serde_json::from_str(r#"[
            {"groupItemTitle": "Foo", "clobTokenIds": "[\"tok-a\"]"},
            {"groupItemTitle": "Bar", "clobTokenIds": "[\"tok-b\"]"}
        ]"#).unwrap();
        let (up, down) = extract_token_ids(&markets);
        assert_eq!(up, "", "Unrecognized outcome should not match");
        assert_eq!(down, "", "Unrecognized outcome should not match");
    }

    /// Scenario: single-market format with outcomes/clobTokenIds arrays where Down appears before Up
    /// Expected: zipped iteration maps each outcome to its paired token regardless of order
    #[test]
    fn test_extract_token_ids_single_market_reversed() {
        let markets: Vec<serde_json::Value> = serde_json::from_str(r#"[
            {
                "outcomes": "[\"Down\",\"Up\"]",
                "clobTokenIds": "[\"down-tok\",\"up-tok\"]"
            }
        ]"#).unwrap();
        let (up, down) = extract_token_ids(&markets);
        assert_eq!(up, "up-tok");
        assert_eq!(down, "down-tok");
    }

    // ── extract_first_token_id edge cases ──

    /// Scenario: clobTokenIds is a string-encoded empty JSON array ("[]")
    /// Expected: deserializes to an empty Vec, so returns empty string
    #[test]
    fn test_extract_first_token_id_empty_array_string() {
        let market: serde_json::Value = serde_json::from_str(
            r#"{"clobTokenIds": "[]"}"#
        ).unwrap();
        assert_eq!(extract_first_token_id(&market), "");
    }

    /// Scenario: clobTokenIds is a native JSON empty array ([])
    /// Expected: array has no first element, so returns empty string
    #[test]
    fn test_extract_first_token_id_empty_native_array() {
        let market: serde_json::Value = serde_json::from_str(
            r#"{"clobTokenIds": []}"#
        ).unwrap();
        assert_eq!(extract_first_token_id(&market), "");
    }

    // ── parse_event_to_market_info edge cases ──

    /// Scenario: human-readable slug (1h market format) with no Unix timestamp suffix
    /// Expected: falls back to computing start_ms as endDate minus window_ms
    #[test]
    fn test_parse_event_slug_without_unix_suffix() {
        // Human-readable slug (1h format) — no unix timestamp at end
        // Should fall back to endDate - window_ms for start_ms
        let event: serde_json::Value = serde_json::from_str(r#"{
            "endDate": "2024-01-15T13:00:00Z",
            "markets": [
                {"groupItemTitle": "Up", "clobTokenIds": "[\"up-tok\"]"},
                {"groupItemTitle": "Down", "clobTokenIds": "[\"down-tok\"]"}
            ]
        }"#).unwrap();
        let slug = "bitcoin-up-or-down-january-15-12pm-et";
        let result = parse_event_to_market_info(&event, slug, 3_600_000).unwrap();
        assert!(result.is_some(), "Should fall back to endDate - window for start");
        let info = result.unwrap();
        // end_ms = 2024-01-15 13:00 UTC = 1705323600000
        // start_ms = end_ms - 3_600_000 = 1705320000000
        assert_eq!(info.end_ms, 1705323600 * 1000);
        assert_eq!(info.start_ms, 1705323600 * 1000 - 3_600_000);
    }

    /// Scenario: event JSON has markets but no endDate field
    /// Expected: returns None because end_ms resolves to 0, making the market invalid
    #[test]
    fn test_parse_event_missing_end_date() {
        let event: serde_json::Value = serde_json::from_str(r#"{
            "markets": [
                {"groupItemTitle": "Up", "clobTokenIds": "[\"up-tok\"]"},
                {"groupItemTitle": "Down", "clobTokenIds": "[\"down-tok\"]"}
            ]
        }"#).unwrap();
        let result = parse_event_to_market_info(&event, "slug-1705320000", 300_000).unwrap();
        // end_ms = 0 (missing endDate) → returns None
        assert!(result.is_none(), "Missing endDate should return None");
    }

    /// Scenario: event has endDate and markets with groupItemTitles but no clobTokenIds
    /// Expected: returns None because both Up and Down token IDs are empty
    #[test]
    fn test_parse_event_missing_tokens() {
        // Markets present but no clobTokenIds → both empty → returns None
        let event: serde_json::Value = serde_json::from_str(r#"{
            "endDate": "2024-01-15T12:05:00Z",
            "markets": [
                {"groupItemTitle": "Up"},
                {"groupItemTitle": "Down"}
            ]
        }"#).unwrap();
        let result = parse_event_to_market_info(&event, "slug-1705320000", 300_000).unwrap();
        assert!(result.is_none(), "Missing tokens should return None");
    }

    /// Scenario: slug ends with a millisecond timestamp instead of the usual seconds timestamp
    /// Expected: detects value > 1 trillion and treats it as milliseconds without multiplying
    #[test]
    fn test_parse_event_millis_in_slug() {
        // Slug ends with millisecond timestamp instead of seconds
        let event: serde_json::Value = serde_json::from_str(r#"{
            "endDate": "2024-01-15T12:05:00Z",
            "markets": [
                {"groupItemTitle": "Up", "clobTokenIds": "[\"up-abc\"]"},
                {"groupItemTitle": "Down", "clobTokenIds": "[\"down-xyz\"]"}
            ]
        }"#).unwrap();
        let slug = "btc-updown-5m-1705320000000"; // millis
        let result = parse_event_to_market_info(&event, slug, 300_000).unwrap();
        assert!(result.is_some());
        let info = result.unwrap();
        // > 1_000_000_000_000 → treated as millis already
        assert_eq!(info.start_ms, 1705320000000);
    }

    /// Scenario: event JSON has an endDate but no "markets" key at all
    /// Expected: returns None because there are no markets to parse
    #[test]
    fn test_parse_event_no_markets_key() {
        let event: serde_json::Value = serde_json::from_str(r#"{
            "endDate": "2024-01-15T12:05:00Z"
        }"#).unwrap();
        let result = parse_event_to_market_info(&event, "slug-1705320000", 300_000).unwrap();
        assert!(result.is_none(), "Missing markets key should return None");
    }
}
