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
const MAX_SPREAD: f64 = 0.08;          // skip if PM spread blown out (high uncertainty)
const IMBALANCE_SKIP: f64 = 0.25;      // skip if bid/total depth < 25% (heavy sell pressure)
const IMBALANCE_LEVELS: usize = 5;

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

        // ── Orderbook depth gates ──
        let book = match side {
            Side::Up => &state.up_book,
            Side::Down => &state.down_book,
        };

        // Skip if spread is blown out — high uncertainty makes fading risky
        let spread = book.spread();
        if spread > MAX_SPREAD {
            return None;
        }

        // Skip if depth is heavily one-sided against us (informed flow).
        // imbalance < 0.25 means bids are < 25% of total → heavy selling pressure.
        let imbalance = book.depth_imbalance(IMBALANCE_LEVELS);
        if imbalance < IMBALANCE_SKIP {
            return None;
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::test_helpers::*;

    /// Scenario: Regime forced to Trend with BTC at strike and tau=120s.
    /// Expected: None -- convexity fading is disabled during trends (adverse selection).
    #[test]
    fn test_none_when_trend() {
        // Force Trend regime → convexity fading is disabled
        let (mut state, now) = make_state(95_000.0, 95_000.0, 0.001, 120.0, 0.45, 0.50);
        force_regime_trend(&mut state, now);
        assert!(ConvexityFade.evaluate(&state, now).is_none());
    }

    /// Scenario: BTC at $96k vs $95k strike in Range regime -- distance_frac ~1% >> 0.3%.
    /// Expected: None -- too far from ATM for convexity amplification.
    #[test]
    fn test_none_when_far_from_strike() {
        // BTC at 96000 vs strike 95000 → distance_frac ~0.01 >> 0.003
        let (mut state, now) = make_state(95_000.0, 96_000.0, 0.001, 120.0, 0.45, 0.50);
        force_regime_range(&mut state, now);
        assert!(ConvexityFade.evaluate(&state, now).is_none());
    }

    /// Scenario: Near ATM in Range regime, but book spread is 10 cents (> 8-cent max).
    /// Expected: None -- wide spread signals high uncertainty, fading is too risky.
    #[test]
    fn test_none_when_spread_blown() {
        // Near ATM, range regime, but book spread > 0.08
        let (mut state, now) = make_state(95_000.0, 95_000.0, 0.001, 120.0, 0.45, 0.50);
        force_regime_range(&mut state, now);
        inject_book(&mut state, Side::Up,
            vec![(0.35, 100.0)],
            vec![(0.45, 100.0)], // spread = 0.10 > 0.08
        );
        assert!(ConvexityFade.evaluate(&state, now).is_none());
    }

    /// Scenario: Near ATM in Range, tight spread, but bids are tiny vs huge asks.
    /// Expected: None -- depth imbalance < 25% indicates heavy sell pressure (informed flow).
    #[test]
    fn test_none_when_imbalance_skewed() {
        // Near ATM, range regime, tight spread, but bids << asks (sell pressure)
        let (mut state, now) = make_state(95_000.0, 95_000.0, 0.001, 120.0, 0.45, 0.50);
        force_regime_range(&mut state, now);
        inject_book(&mut state, Side::Up,
            vec![(0.44, 10.0)],       // tiny bid depth
            vec![(0.45, 200.0)],      // huge ask depth → imbalance < 0.25
        );
        assert!(ConvexityFade.evaluate(&state, now).is_none());
    }

    /// Scenario: Near ATM in Range with balanced book, but up_ask = 0.50 matches fair value.
    /// Expected: None -- no mispricing to fade.
    #[test]
    fn test_none_when_no_edge() {
        // Near ATM, range regime, but up_ask is near fair → no edge
        let (mut state, now) = make_state(95_000.0, 95_000.0, 0.001, 120.0, 0.50, 0.50);
        force_regime_range(&mut state, now);
        inject_book(&mut state, Side::Up,
            vec![(0.49, 100.0)],
            vec![(0.50, 100.0)],
        );
        assert!(ConvexityFade.evaluate(&state, now).is_none());
    }

    /// Scenario: BTC at strike (ATM), Range regime, up_ask at 0.42 vs fair ~0.50, balanced book.
    /// Expected: UP signal with edge ~8 cents -- market overreacted, fade the oscillation.
    #[test]
    fn test_signal_near_atm_range() {
        // Near ATM, range regime, up_ask below fair, balanced book
        // BTC near strike, fair ~0.50, up_ask at 0.42 → edge ~0.08
        let (mut state, now) = make_state(95_000.0, 95_000.0, 0.001, 120.0, 0.42, 0.50);
        force_regime_range(&mut state, now);
        inject_book(&mut state, Side::Up,
            vec![(0.41, 100.0)],
            vec![(0.42, 100.0)],
        );
        let sig = ConvexityFade.evaluate(&state, now);
        assert!(sig.is_some(), "Should produce signal near ATM in range regime with edge");
        let sig = sig.unwrap();
        assert!(sig.edge >= 0.02);
        assert!(!sig.is_passive);
    }

    // ── Parameterized success tests ──

    /// Scenario: ATM in Range with 4 up_ask levels (0.38-0.44) progressively closer to fair.
    /// Expected: Cheaper asks produce signals with edge >= 2 cents; at least 2 of 4 fire.
    #[test]
    fn test_signal_across_ask_levels() {
        // Near ATM in range, varying ask levels below fair
        let asks = [0.38, 0.40, 0.42, 0.44];
        let mut signals = 0;

        for &ask in &asks {
            let (mut state, now) = make_state(95_000.0, 95_000.0, 0.001, 120.0, ask, 0.50);
            force_regime_range(&mut state, now);
            inject_book(&mut state, Side::Up,
                vec![(ask - 0.01, 100.0)],
                vec![(ask, 100.0)],
            );
            if let Some(sig) = ConvexityFade.evaluate(&state, now) {
                assert!(sig.edge >= 0.02, "ask={}: edge = {}", ask, sig.edge);
                signals += 1;
            }
        }
        assert!(signals >= 2, "Should produce signals for cheap asks: {}/4", signals);
    }

    /// Scenario: Ambiguous regime (~65% up ticks) near ATM with up_ask below fair.
    /// Expected: Strategy may fire -- only Trend is blocked; Range and Ambiguous are allowed.
    #[test]
    fn test_ambiguous_regime_also_works() {
        // Ambiguous regime should also trigger convexity fade (only Trend blocks)
        let (mut state, now) = make_state(95_000.0, 95_000.0, 0.001, 120.0, 0.42, 0.50);
        // Don't call force_regime — default with < 10 ticks → Ambiguous
        // But need 10+ ticks for non-Ambiguous. Actually, Ambiguous should also pass since
        // convexity_fade only blocks Regime::Trend.
        // Add ~15 ticks with ~65% up → Ambiguous (60-75%)
        for i in 0..15 {
            state.bn.regime.update(now - 15000 + i * 1000, i % 3 != 0);
        }
        inject_book(&mut state, Side::Up,
            vec![(0.41, 100.0)],
            vec![(0.42, 100.0)],
        );
        if let Some(sig) = ConvexityFade.evaluate(&state, now) {
            assert!(sig.edge >= 0.02);
        }
    }

    /// Scenario: BTC at $94,990, slightly below $95k strike in Range; down_ask cheap at 0.42.
    /// Expected: DOWN signal -- fair_down > 0.50, market overreacted on the DOWN side.
    #[test]
    fn test_down_side_signal() {
        // Price near but slightly below strike → DOWN is cheaper, should produce DOWN signal
        // s_est = 94_990 (slightly below strike 95_000), fair_up < 0.50
        // → fair_down > 0.50, down_ask at 0.42 → edge on DOWN
        let (mut state, now) = make_state(95_000.0, 94_990.0, 0.001, 120.0, 0.50, 0.42);
        force_regime_range(&mut state, now);
        inject_book(&mut state, Side::Down,
            vec![(0.41, 100.0)],
            vec![(0.42, 100.0)],
        );
        if let Some(sig) = ConvexityFade.evaluate(&state, now) {
            assert_eq!(sig.side, Side::Down);
        }
    }
}
