use crate::engine::state::MarketState;
use crate::strategies::{kelly, time_left_fraction, Strategy};
use crate::types::{Side, Signal};

/// S1: Distance Fade
/// If Binance is clearly above/below strike, compute fair value via sigmoid
/// and buy the underpriced Polymarket side.
/// Active: entire market window. Min distance: 0.044% of strike. Min edge: 8c.
///
/// All thresholds expressed as fraction-of-strike so they scale across assets:
/// BTC $68k, ETH $3.5k, SOL $150, XRP $0.60 — same percentage = same signal.
pub struct DistanceFade;

// Calibrated from BTC at ~$68k: $30/$68000 = 0.000441
const MIN_DIST_FRAC: f64 = 0.000441;
// Sigmoid normalization: $80/$68000 = 0.001176
const SIGMOID_SCALE_FRAC: f64 = 0.001176;
// Confidence normalization: $100/$68000 = 0.001471
const CONFIDENCE_NORM_FRAC: f64 = 0.001471;

impl Strategy for DistanceFade {
    fn name(&self) -> &'static str {
        "distance_fade"
    }

    #[inline]
    fn evaluate(&self, state: &MarketState, now_ms: i64) -> Option<Signal> {
        let distance = state.distance();
        let dist_frac = state.distance_frac();

        if dist_frac.abs() < MIN_DIST_FRAC {
            return None;
        }

        let time_left_frac = time_left_fraction(state, now_ms);

        // Fair value: sigmoid of distance fraction, boosted as time runs out
        let base = 0.5 + 0.4 * (dist_frac / SIGMOID_SCALE_FRAC).tanh();
        let time_boost = (1.0 - time_left_frac) * 0.15 * dist_frac.signum();
        let fair_up = (base + time_boost).clamp(0.02, 0.98);

        // Pick side
        let (side, fair, market_ask) = if distance > 0.0 {
            (Side::Up, fair_up, state.up_ask)
        } else {
            (Side::Down, 1.0 - fair_up, state.down_ask)
        };

        if market_ask <= 0.0 || market_ask >= 1.0 {
            return None;
        }

        let edge = fair - market_ask;
        if edge < 0.08 {
            return None;
        }

        Some(Signal {
            strategy: "distance_fade",
            side,
            edge,
            fair_value: fair,
            market_price: market_ask,
            confidence: (dist_frac.abs() / CONFIDENCE_NORM_FRAC).clamp(0.3, 1.0),
            size_frac: kelly(edge, market_ask).min(0.08),
        })
    }
}
