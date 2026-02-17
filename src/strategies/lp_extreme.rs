use crate::engine::state::MarketState;
use crate::math::pricing::z_score;
use crate::math::regime::Regime;
use crate::strategies::Strategy;
use crate::types::{EvalTrigger, Side, Signal};

/// Edge 6: Extreme Probability Liquidity Provision
///
/// At P near 0 or 1, market makers retreat. Provide liquidity
/// on the losing side, earning wide spreads.
/// Requires |z| > 1.5, regime != Trend, τ_eff > 60s.
/// Places passive limit orders (is_passive = true).
pub struct LpExtreme;

const Z_MIN: f64 = 1.5;
const MIN_TAU_S: f64 = 60.0;
const MIN_EDGE: f64 = 0.02;

impl Strategy for LpExtreme {
    fn name(&self) -> &'static str {
        "lp_extreme"
    }

    fn trigger(&self) -> EvalTrigger {
        EvalTrigger::Both
    }

    #[inline]
    fn evaluate(&self, state: &MarketState, now_ms: i64) -> Option<Signal> {
        let sigma = state.sigma_real();
        if sigma <= 0.0 {
            return None;
        }

        let tau = state.tau_eff_s(now_ms);
        if tau < MIN_TAU_S {
            return None;
        }

        // Don't provide liquidity during trends (adverse selection too high)
        if state.bn.regime.classify() == Regime::Trend {
            return None;
        }

        let s = state.s_est();
        let k = state.info.strike;
        if s <= 0.0 || k <= 0.0 {
            return None;
        }

        let z = z_score(s, k, sigma, tau);
        let z_abs = z.abs();

        if z_abs < Z_MIN {
            return None;
        }

        // Provide liquidity on the LOSING side (the side that's near 0)
        // When z > 2.0: UP is near-certain, DOWN is near 0 → LP on DOWN (buy cheap)
        // When z < -2.0: DOWN is near-certain, UP is near 0 → LP on UP (buy cheap)
        let (side, market_ask) = if z > 0.0 {
            // Buy DOWN tokens cheap (they're near 0, likely won't pay out)
            (Side::Down, state.down_ask)
        } else {
            // Buy UP tokens cheap (they're near 0, likely won't pay out)
            (Side::Up, state.up_ask)
        };

        if market_ask <= 0.0 || market_ask >= 0.25 {
            return None; // Only LP when price is extreme (< 25 cents)
        }

        // True probability of the losing side winning
        let true_prob = if z > 0.0 {
            1.0 - crate::math::pricing::p_fair(s, k, sigma, tau)
        } else {
            crate::math::pricing::p_fair(s, k, sigma, tau)
        };

        // Only provide liquidity if we're getting positive EV
        // EV = true_prob * (1 - cost) - (1 - true_prob) * cost
        // Simplifies to: true_prob - cost  for unit payoff
        let edge = true_prob - market_ask;

        // For extreme LP, edge can be negative (the losing side SHOULD be cheap)
        // But we want to buy below fair — even if fair is very low
        if edge < MIN_EDGE {
            return None;
        }

        // Kelly sizing for binary: f* = (1-p) - p*(1-a)/a
        // where p = true probability of the winning side, a = our buy price
        let p_winning = 1.0 - true_prob;
        let a = market_ask;
        let f_star = if a > 0.0 && a < 1.0 {
            (true_prob - p_winning * (1.0 - a) / a).max(0.0)
        } else {
            0.0
        };

        let size_frac = (f_star * 0.5).clamp(0.0, 0.02); // half-Kelly, max 2%
        if size_frac < 0.001 {
            return None;
        }

        let confidence = (z_abs / 4.0).clamp(0.3, 0.8);

        Some(Signal {
            strategy: "lp_extreme",
            side,
            edge,
            fair_value: true_prob,
            market_price: market_ask,
            confidence,
            size_frac,
            is_passive: true, // passive limit orders
        })
    }
}
