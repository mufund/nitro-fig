use crate::engine::state::MarketState;
use crate::strategies::{kelly, Strategy};
use crate::types::{Side, Signal};

/// S3: Settlement Sniper
/// In the last 90 seconds, if Binance is clearly on one side,
/// buy any remaining mispricing. Highest conviction strategy.
/// Active: T-90s to T-5s. Min distance: 0.059% of strike. Min edge: 5c. Max size: 10%.
///
/// All thresholds expressed as fraction-of-strike so they scale across assets.
pub struct SettlementSniper;

// Calibrated from BTC at ~$68k:
// $40/$68000 = 0.000588
const MIN_DIST_FRAC: f64 = 0.000588;
// $50/$68000 = 0.000735
const MID_DIST_FRAC: f64 = 0.000735;
// $100/$68000 = 0.001471
const HIGH_DIST_FRAC: f64 = 0.001471;
// $2.0/sec / $68000 = 0.0000294
const REVERSAL_VELOCITY_FRAC: f64 = 0.0000294;

impl Strategy for SettlementSniper {
    fn name(&self) -> &'static str {
        "settlement_sniper"
    }

    #[inline]
    fn evaluate(&self, state: &MarketState, now_ms: i64) -> Option<Signal> {
        let time_left_s = state.time_left_s(now_ms);
        if time_left_s > 90.0 || time_left_s < 5.0 {
            return None;
        }

        let strike = state.info.strike;
        if strike <= 0.0 {
            return None;
        }

        let distance = state.distance();
        let dist_frac = (distance / strike).abs();

        if dist_frac < MIN_DIST_FRAC {
            return None;
        }

        // Check momentum isn't reversing hard
        if let Some(first) = state.trade_buffer.front() {
            let span_s = (now_ms - first.exchange_ts_ms).max(1) as f64 / 1000.0;
            let velocity_frac = (state.binance_price - first.price) / span_s / strike;
            if distance > 0.0 && velocity_frac < -REVERSAL_VELOCITY_FRAC {
                return None;
            }
            if distance < 0.0 && velocity_frac > REVERSAL_VELOCITY_FRAC {
                return None;
            }
        }

        let t = time_left_s as i64;

        // Fair value lookup table using fractional distance thresholds
        let fair = if dist_frac > HIGH_DIST_FRAC && t < 30 {
            0.97
        } else if dist_frac > HIGH_DIST_FRAC && t < 60 {
            0.95
        } else if dist_frac > HIGH_DIST_FRAC {
            0.93
        } else if dist_frac > MID_DIST_FRAC && t < 30 {
            0.95
        } else if dist_frac > MID_DIST_FRAC && t < 60 {
            0.92
        } else if dist_frac > MID_DIST_FRAC {
            0.88
        } else if dist_frac > MIN_DIST_FRAC && t < 30 {
            0.90
        } else if dist_frac > MIN_DIST_FRAC {
            0.85
        } else {
            return None;
        };

        let (side, market_ask) = if distance > 0.0 {
            (Side::Up, state.up_ask)
        } else {
            (Side::Down, state.down_ask)
        };

        if market_ask <= 0.0 || market_ask >= 1.0 {
            return None;
        }

        let edge = fair - market_ask;
        if edge < 0.05 {
            return None;
        }

        Some(Signal {
            strategy: "settlement_sniper",
            side,
            edge,
            fair_value: fair,
            market_price: market_ask,
            confidence: 0.95,
            size_frac: kelly(edge, market_ask).min(0.10),
        })
    }
}
