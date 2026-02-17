use crate::engine::state::MarketState;
use crate::math::pricing::{p_fair, z_score};
use crate::strategies::{kelly, Strategy};
use crate::types::{EvalTrigger, Side, Signal};

/// Edge 2: Certainty Capture (Settlement Convergence)
///
/// Near expiry, if BTC is far from strike, the outcome is near-deterministic.
/// Buy the near-certain side if PM still has residual mispricing.
/// z = ln(S_est/K) / (σ_real · √τ_eff)
pub struct CertaintyCapture;

const Z_MIN: f64 = 1.5;     // ~$130 from strike at typical vol
const MIN_EDGE: f64 = 0.02;

impl Strategy for CertaintyCapture {
    fn name(&self) -> &'static str {
        "certainty_capture"
    }

    fn trigger(&self) -> EvalTrigger {
        EvalTrigger::PolymarketQuote
    }

    #[inline]
    fn evaluate(&self, state: &MarketState, now_ms: i64) -> Option<Signal> {
        let sigma = state.sigma_real();
        if sigma <= 0.0 {
            return None;
        }

        let s = state.s_est();
        let k = state.info.strike;
        let tau = state.tau_eff_s(now_ms);
        if tau < 0.5 || s <= 0.0 || k <= 0.0 {
            return None;
        }

        let z = z_score(s, k, sigma, tau);
        let z_abs = z.abs();

        if z_abs < Z_MIN {
            return None;
        }

        // Model fair probability
        let fair_up = p_fair(s, k, sigma, tau);

        // Determine side and market price
        let (side, fair, market_ask) = if z > 0.0 {
            // S > K, UP is near-certain
            (Side::Up, fair_up, state.up_ask)
        } else {
            // S < K, DOWN is near-certain
            (Side::Down, 1.0 - fair_up, state.down_ask)
        };

        if market_ask <= 0.0 || market_ask >= 1.0 {
            return None;
        }

        let edge = fair - market_ask;
        if edge < MIN_EDGE {
            return None;
        }

        // Sizing tiers based on z-score
        let max_size_frac = if z_abs > 3.0 {
            0.05
        } else if z_abs > 2.5 {
            0.03
        } else {
            0.01
        };

        let confidence = (z_abs / 4.0).clamp(0.5, 0.99);

        Some(Signal {
            strategy: "certainty_capture",
            side,
            edge,
            fair_value: fair,
            market_price: market_ask,
            confidence,
            size_frac: kelly(edge, market_ask).min(max_size_frac),
            is_passive: false,
        })
    }
}
