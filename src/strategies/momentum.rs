use crate::engine::state::MarketState;
use crate::strategies::{kelly, time_left_fraction, Strategy};
use crate::types::{Side, Signal};

/// S2: Momentum Regime
/// Use Binance trade flow (30s rolling window) to detect directional momentum.
/// Trade when momentum is strong but Polymarket hasn't caught up.
/// Active: start to T-15%. Min velocity: 0.000735%/sec of strike. Min edge: 5c.
///
/// All thresholds expressed as fraction-of-strike so they scale across assets.
pub struct Momentum;

// Calibrated from BTC at ~$68k:
// $0.50/sec velocity / $68000 = 0.00000735
const MIN_VELOCITY_FRAC: f64 = 0.00000735;
// $20 distance / $68000 = 0.000294
const MIN_DIST_FRAC: f64 = 0.000294;

impl Strategy for Momentum {
    fn name(&self) -> &'static str {
        "momentum"
    }

    #[inline]
    fn evaluate(&self, state: &MarketState, now_ms: i64) -> Option<Signal> {
        let time_left_frac = time_left_fraction(state, now_ms);
        if time_left_frac < 0.15 {
            return None; // skip last ~15% of market window
        }

        let buf = &state.trade_buffer;
        if buf.len() < 50 {
            return None;
        }

        let strike = state.info.strike;
        if strike <= 0.0 {
            return None;
        }

        let oldest = buf.front()?;
        let oldest_ts = oldest.exchange_ts_ms;
        let oldest_price = oldest.price;
        let span_s = (now_ms - oldest_ts).max(1) as f64 / 1000.0;

        let tick_velocity_frac = (state.binance_price - oldest_price) / span_s / strike;
        let net_volume: f64 = buf
            .iter()
            .map(|t| if t.is_buy { t.qty } else { -t.qty })
            .sum();
        let _intensity = buf.len() as f64 / span_s;

        let dist_frac = state.distance_frac();

        // Strong momentum: velocity + volume agree
        let (side, market_ask) = if tick_velocity_frac > MIN_VELOCITY_FRAC
            && net_volume > 0.0
            && dist_frac > MIN_DIST_FRAC
        {
            (Side::Up, state.up_ask)
        } else if tick_velocity_frac < -MIN_VELOCITY_FRAC
            && net_volume < 0.0
            && dist_frac < -MIN_DIST_FRAC
        {
            (Side::Down, state.down_ask)
        } else {
            return None;
        };

        if market_ask <= 0.0 || market_ask > 0.60 {
            return None; // only when PM is slow to react
        }

        let edge = 0.65 - market_ask;
        if edge < 0.05 {
            return None;
        }

        Some(Signal {
            strategy: "momentum",
            side,
            edge,
            fair_value: 0.65,
            market_price: market_ask,
            confidence: (_intensity / 40.0).clamp(0.3, 0.8),
            size_frac: kelly(edge, market_ask).min(0.05),
        })
    }
}
