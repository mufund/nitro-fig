// Shared test fixtures for strategy and risk manager tests.
// Only compiled under #[cfg(test)].

use crate::config::{Config, Interval};
use crate::engine::state::{BinanceState, MarketState};
use crate::math::oracle::OracleBasis;
use crate::types::{MarketInfo, Side};

/// Build a MarketState with the given parameters.
/// Returns (state, now_ms) where now_ms is the timestamp to pass to evaluate().
///
/// start_ms is set 10s before now_ms (inside StrikeMisalign's 15s window).
/// end_ms is set tau_s seconds after now_ms.
pub fn make_state(
    strike: f64,
    binance_price: f64,
    sigma: f64,
    tau_s: f64,
    up_ask: f64,
    down_ask: f64,
) -> (MarketState, i64) {
    let now_ms: i64 = 1_700_000_100_000;
    let info = MarketInfo {
        slug: "btc-updown-5m-1700000090".to_string(),
        start_ms: now_ms - 10_000,
        end_ms: now_ms + (tau_s * 1000.0) as i64,
        up_token_id: "up-token".to_string(),
        down_token_id: "down-token".to_string(),
        strike,
        tick_size: 0.01,
        neg_risk: false,
    };

    let oracle = OracleBasis::new(0.0, 2.0);
    let bn = BinanceState::new(0.94, 5, 0.30, 30_000, 60_000);
    let mut state = MarketState::new(info, bn, oracle);

    // Inject live data
    state.bn.binance_price = binance_price;
    state.bn.binance_ts = now_ms;
    state.bn.sigma_real_cached = sigma;
    state.pm_last_ts = now_ms;

    // Polymarket quotes
    state.up_ask = up_ask;
    state.down_ask = down_ask;
    state.up_bid = if up_ask > 0.02 { up_ask - 0.02 } else { 0.0 };
    state.down_bid = if down_ask > 0.02 { down_ask - 0.02 } else { 0.0 };

    (state, now_ms)
}

/// Inject an orderbook snapshot for the given side and sync scalar bid/ask fields.
pub fn inject_book(state: &mut MarketState, side: Side, bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)>) {
    match side {
        Side::Up => {
            state.up_book.apply_snapshot(bids, asks);
            state.up_bid = state.up_book.best_bid();
            state.up_ask = state.up_book.best_ask();
        }
        Side::Down => {
            state.down_book.apply_snapshot(bids, asks);
            state.down_bid = state.down_book.best_bid();
            state.down_ask = state.down_book.best_ask();
        }
    }
}

/// Feed alternating up/down ticks to force Range regime (< 60% dominant).
pub fn force_regime_range(state: &mut MarketState, now_ms: i64) {
    for i in 0..20 {
        state.bn.regime.update(now_ms - 20_000 + i * 100, i % 2 == 0);
    }
}

/// Feed 80% same-direction ticks to force Trend regime (>= 75% dominant).
pub fn force_regime_trend(state: &mut MarketState, now_ms: i64) {
    for i in 0..20 {
        state.bn.regime.update(now_ms - 20_000 + i * 100, i % 5 != 0); // 80% up
    }
}

/// Inject a VWAP data point.
pub fn inject_vwap(state: &mut MarketState, price: f64, qty: f64, now_ms: i64) {
    state.bn.vwap_tracker.update(now_ms, price, qty);
}

/// Build a minimal Config for risk manager tests.
pub fn make_config() -> Config {
    Config {
        asset: "btc".into(),
        interval: Interval::M5,
        binance_ws: String::new(),
        binance_ws_fallback: String::new(),
        polymarket_clob_ws: String::new(),
        gamma_api_url: String::new(),
        series_id: String::new(),
        tg_bot_token: None,
        tg_chat_id: None,
        max_position_usd: 100.0,
        max_orders_per_market: 10,
        cooldown_ms: 5000,
        bankroll: 1000.0,
        max_total_exposure_frac: 0.15,
        daily_loss_halt_frac: -0.03,
        weekly_loss_halt_frac: -0.08,
        oracle_beta: 0.0,
        oracle_delta_s: 2.0,
        ewma_lambda: 0.94,
        sigma_floor_annual: 0.30,
        strategy_latency_arb: true,
        strategy_certainty_capture: true,
        strategy_convexity_fade: true,
        strategy_strike_misalign: true,
        strategy_lp_extreme: true,
        strategy_cross_timeframe: false,
        dry_run: true,
        polymarket_private_key: None,
        polymarket_funder_address: None,
        polymarket_signature_type: 0,
    }
}
