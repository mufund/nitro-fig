use crate::engine::state::MarketState;
use crate::math::normal::phi;
use crate::math::pricing::{d2, p_fair};
use crate::math::regime::Regime;
use crate::strategies::{kelly, Strategy};
use crate::types::{EvalTrigger, Side, Signal};

/// Edge 3: Convexity Fading (Near-Strike Oscillation Trading)
///
/// Near ATM and near expiry, binary delta amplifies small BTC oscillations
/// into large probability swings. Fade these swings in range-bound conditions.
/// Requires regime == Range. Disabled when Trend or τ_eff < 30s.
pub struct ConvexityFade;

const MAX_DIST_FRAC: f64 = 0.003; // within 0.3% of strike
const MIN_TAU_S: f64 = 30.0;     // hand off to Edge 2 below this
const MIN_EDGE: f64 = 0.02;
const SQRT_2_OVER_PI: f64 = 0.797_884_560_802_865_4;

impl Strategy for ConvexityFade {
    fn name(&self) -> &'static str {
        "convexity_fade"
    }

    fn trigger(&self) -> EvalTrigger {
        EvalTrigger::PolymarketQuote
    }

    #[inline]
    fn evaluate(&self, state: &MarketState, now_ms: i64) -> Option<Signal> {
        // Require non-trending regime (Range or Ambiguous ok, Trend blocked)
        let regime = state.bn.regime.classify();
        if regime == Regime::Trend {
            return None;
        }

        let sigma = state.sigma_real();
        if sigma <= 0.0 {
            return None;
        }

        let tau = state.tau_eff_s(now_ms);
        if tau < MIN_TAU_S {
            return None;
        }

        let dist_frac = state.distance_frac().abs();
        if dist_frac > MAX_DIST_FRAC {
            return None;
        }

        let s = state.s_est();
        let k = state.info.strike;
        if s <= 0.0 || k <= 0.0 {
            return None;
        }

        // Compute model fair probability
        let fair_up = p_fair(s, k, sigma, tau);

        // Compute expected probability swing: E[|ΔP|] = phi(d2) * sqrt(Δt/tau) * sqrt(2/pi)
        // Using Δt ≈ 10s (typical inter-update interval)
        let d = d2(s, k, sigma, tau);
        let _expected_swing = phi(d) * (10.0 / tau).sqrt() * SQRT_2_OVER_PI;

        // Fade: if PM overreacted (price moved away from fair), trade back
        // Buy UP if pm_ask < fair (market thinks UP is too cheap after oscillation)
        // Buy DOWN if (1-fair) > down_ask
        let edge_up = fair_up - state.up_ask;
        let edge_down = (1.0 - fair_up) - state.down_ask;

        let (side, edge, fair, market_ask) = if state.up_ask > 0.0
            && state.up_ask < 1.0
            && edge_up > edge_down
            && edge_up > MIN_EDGE
        {
            (Side::Up, edge_up, fair_up, state.up_ask)
        } else if state.down_ask > 0.0
            && state.down_ask < 1.0
            && edge_down > MIN_EDGE
        {
            (Side::Down, edge_down, 1.0 - fair_up, state.down_ask)
        } else {
            return None;
        };

        // Low confidence, high frequency strategy
        let confidence = 0.4;

        Some(Signal {
            strategy: "convexity_fade",
            side,
            edge,
            fair_value: fair,
            market_price: market_ask,
            confidence,
            size_frac: kelly(edge, market_ask).min(0.005),
            is_passive: false,
        })
    }
}
