use crate::engine::state::MarketState;
use crate::math::normal::phi;
use crate::math::pricing::d2;
use crate::strategies::{kelly, Strategy};
use crate::types::{EvalTrigger, Side, Signal};

/// Edge 5: Strike Misalignment (Opening Bias)
///
/// Strike K is set from a point-in-time snapshot at market open.
/// Microstructure noise creates a biased strike. Trade the bias
/// in the first 10-15 seconds before the market corrects.
pub struct StrikeMisalign;

const MAX_ACTIVE_MS: i64 = 15_000; // only first 15s
const MIN_DP: f64 = 0.02; // minimum probability shift to trade
const MIN_EDGE: f64 = 0.02;

impl Strategy for StrikeMisalign {
    fn name(&self) -> &'static str {
        "strike_misalign"
    }

    fn trigger(&self) -> EvalTrigger {
        EvalTrigger::MarketOpen
    }

    #[inline]
    fn evaluate(&self, state: &MarketState, now_ms: i64) -> Option<Signal> {
        // Only active in first 15 seconds of market
        let elapsed_ms = now_ms - state.info.start_ms;
        if elapsed_ms < 0 || elapsed_ms > MAX_ACTIVE_MS {
            return None;
        }

        let sigma = state.sigma_real();
        if sigma <= 0.0 {
            return None;
        }

        // Need VWAP data
        if !state.bn.vwap_tracker.has_data() {
            return None;
        }

        let s_ref = state.bn.vwap_tracker.vwap();
        if s_ref <= 0.0 {
            return None;
        }

        let k = state.info.strike;
        let epsilon = k - s_ref; // strike bias

        let tau = state.tau_eff_s(now_ms);
        if tau < 10.0 {
            return None;
        }

        // ΔP ≈ -phi(d2) / (S * sigma * sqrt(tau)) * epsilon
        let d = d2(state.s_est(), k, sigma, tau);
        let sensitivity = phi(d) / (s_ref * sigma * tau.sqrt());
        let dp = -sensitivity * epsilon;

        if dp.abs() < MIN_DP {
            return None;
        }

        // dp > 0 means UP is underpriced (K was set too high, S_ref < K)
        // dp < 0 means DOWN is underpriced (K was set too low, S_ref > K)
        let (side, market_ask) = if dp > 0.0 {
            (Side::Up, state.up_ask)
        } else {
            (Side::Down, state.down_ask)
        };

        if market_ask <= 0.0 || market_ask >= 1.0 {
            return None;
        }

        // Fair value based on the corrected VWAP reference
        let fair = if dp > 0.0 {
            crate::math::pricing::p_fair(s_ref, k, sigma, tau)
        } else {
            1.0 - crate::math::pricing::p_fair(s_ref, k, sigma, tau)
        };

        let edge = fair - market_ask;
        if edge < MIN_EDGE {
            return None;
        }

        let confidence = (dp.abs() / 0.10).clamp(0.4, 0.9);

        Some(Signal {
            strategy: "strike_misalign",
            side,
            edge,
            fair_value: fair,
            market_price: market_ask,
            confidence,
            size_frac: kelly(edge, market_ask).min(0.02),
            is_passive: false,
        })
    }
}
