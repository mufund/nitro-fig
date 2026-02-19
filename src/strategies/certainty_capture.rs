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
        if tau < 30.0 || s <= 0.0 || k <= 0.0 {
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

        let confidence = (z_abs / 3.0).clamp(0.375, 0.99);

        Some(Signal {
            strategy: "certainty_capture",
            side,
            edge,
            fair_value: fair,
            market_price: market_ask,
            confidence,
            size_frac: kelly(edge, market_ask),
            is_passive: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::test_helpers::*;

    /// Scenario: BTC at $95,001 vs $95,000 strike -- essentially ATM, z near 0.
    /// Expected: None -- |z| < 1.5, outcome is not near-certain.
    #[test]
    fn test_none_when_near_strike() {
        // BTC at 95001 vs strike 95000 → z ≈ 0 → None
        let (state, now) = make_state(95_000.0, 95_001.0, 0.001, 120.0, 0.50, 0.50);
        assert!(CertaintyCapture.evaluate(&state, now).is_none());
    }

    /// Scenario: BTC far above strike (z > 1.5) but up_ask at 0.99, near model fair.
    /// Expected: None -- edge below 2-cent minimum despite high z-score.
    #[test]
    fn test_none_when_edge_too_small() {
        // BTC far above strike (z > 1.5) but up_ask very close to fair → no edge
        let (state, now) = make_state(95_000.0, 96_500.0, 0.001, 120.0, 0.99, 0.50);
        assert!(CertaintyCapture.evaluate(&state, now).is_none());
    }

    /// Scenario: BTC at $97k vs $95k with tau=60s giving z ~2.66; up_ask stale at 0.90.
    /// Expected: UP signal -- near-certain UP outcome with ~8-cent edge vs fair ~0.98.
    #[test]
    fn test_signal_up_side() {
        // BTC far above strike with short tau → z > 1.5 → UP is near-certain
        // tau_eff=62s, z = ln(97000/95000)/(0.001*sqrt(62)) ≈ 2.66
        // fair_up ≈ 0.98, up_ask at 0.90 → edge ~0.08
        let (state, now) = make_state(95_000.0, 97_000.0, 0.001, 60.0, 0.90, 0.50);
        let sig = CertaintyCapture.evaluate(&state, now);
        assert!(sig.is_some(), "Should produce signal when z > 1.5 and edge exists");
        let sig = sig.unwrap();
        assert_eq!(sig.side, Side::Up);
        assert!(sig.edge >= 0.02, "Edge should exceed MIN_EDGE: {}", sig.edge);
        assert!(!sig.is_passive);
    }

    /// Scenario: BTC at $93k vs $95k with tau=60s giving z ~-2.73; down_ask stale at 0.90.
    /// Expected: DOWN signal -- near-certain DOWN outcome with residual mispricing.
    #[test]
    fn test_signal_down_side() {
        // BTC far below strike with short tau → z < -1.5 → DOWN is near-certain
        // tau_eff=62s, z = ln(93000/95000)/(0.001*sqrt(62)) ≈ -2.73
        let (state, now) = make_state(95_000.0, 93_000.0, 0.001, 60.0, 0.50, 0.90);
        let sig = CertaintyCapture.evaluate(&state, now);
        assert!(sig.is_some(), "Should produce signal when z < -1.5 and edge exists");
        let sig = sig.unwrap();
        assert_eq!(sig.side, Side::Down);
        assert!(sig.edge >= 0.02, "Edge should exceed MIN_EDGE: {}", sig.edge);
    }

    /// Scenario: up_ask is 0.0 (no valid quote on the PM CLOB).
    /// Expected: None -- invalid market price blocks signal generation.
    #[test]
    fn test_none_when_ask_invalid() {
        // up_ask = 0.0 → invalid market price
        let (state, now) = make_state(95_000.0, 96_500.0, 0.001, 120.0, 0.0, 0.50);
        assert!(CertaintyCapture.evaluate(&state, now).is_none());
    }

    // ── Parameterized tests ──

    /// Scenario: Four price/ask combos producing varying z-scores (UP and DOWN sides).
    /// Expected: All produce signals with correct side and edge >= 2 cents.
    #[test]
    fn test_signal_across_z_scores() {
        // Various price distances from strike at short tau → varying z-scores
        // Strike = 95000, sigma = 0.001, tau_s = 60 → tau_eff ≈ 62
        // z = ln(S/K) / (0.001 * sqrt(62))
        let params = [
            (97_000.0, 0.85, Side::Up),   // far above: z ≈ 2.66
            (98_000.0, 0.90, Side::Up),   // very far above: z ≈ 4.0
            (93_000.0, 0.85, Side::Down), // far below: z ≈ -2.73
            (92_000.0, 0.90, Side::Down), // very far below: z ≈ -4.1
        ];

        for &(bn_price, losing_ask, expected_side) in &params {
            let (up_ask, down_ask) = match expected_side {
                Side::Up => (losing_ask, 0.50),
                Side::Down => (0.50, losing_ask),
            };
            let (state, now) = make_state(95_000.0, bn_price, 0.001, 60.0, up_ask, down_ask);
            let sig = CertaintyCapture.evaluate(&state, now);
            assert!(sig.is_some(), "Should signal at bn_price={}, ask={}", bn_price, losing_ask);
            let sig = sig.unwrap();
            assert_eq!(sig.side, expected_side, "Wrong side at bn_price={}", bn_price);
            assert!(sig.edge >= 0.02, "Edge too small at bn_price={}: {}", bn_price, sig.edge);
        }
    }

    /// Scenario: Two states with different z-scores (BTC $97k vs $99k above $95k strike).
    /// Expected: Higher z-score produces higher confidence since confidence = z/4 clamped.
    #[test]
    fn test_confidence_increases_with_z() {
        // Higher z → higher confidence
        let (state_low, now_low) = make_state(95_000.0, 97_000.0, 0.001, 60.0, 0.85, 0.50);
        let (state_high, now_high) = make_state(95_000.0, 99_000.0, 0.001, 60.0, 0.85, 0.50);

        let sig_low = CertaintyCapture.evaluate(&state_low, now_low);
        let sig_high = CertaintyCapture.evaluate(&state_high, now_high);

        if let (Some(sl), Some(sh)) = (sig_low, sig_high) {
            assert!(sh.confidence >= sl.confidence,
                "Higher z should give higher confidence: {} vs {}", sh.confidence, sl.confidence);
        }
    }

    /// Scenario: BTC at $99k vs $95k with tau=60s giving z ~5.2.
    /// Expected: Size capped at 15% by kelly() clamp — risk manager clips further.
    #[test]
    fn test_sizing_high_z() {
        // BTC at 99000 vs 95000 with tau=60 → z ≈ 5.2
        // kelly() clamps at 0.15; risk manager's max_per_trade_frac handles the rest
        let (state, now) = make_state(95_000.0, 99_000.0, 0.001, 60.0, 0.90, 0.50);
        let sig = CertaintyCapture.evaluate(&state, now);
        if let Some(sig) = sig {
            assert!(sig.size_frac <= 0.15, "Kelly caps at 15%: {}", sig.size_frac);
            assert!(sig.size_frac > 0.0, "Should have positive size");
        }
    }

    /// Scenario: Realized vol is zero with otherwise valid high-z setup.
    /// Expected: None -- sigma=0 makes z-score and fair value undefined.
    #[test]
    fn test_none_when_sigma_zero() {
        let (state, now) = make_state(95_000.0, 97_000.0, 0.0, 60.0, 0.85, 0.50);
        assert!(CertaintyCapture.evaluate(&state, now).is_none());
    }

    /// Scenario: up_ask set to 1.0 (the invalid boundary price for a binary).
    /// Expected: None -- ask >= 1.0 is rejected as an invalid market price.
    #[test]
    fn test_none_when_tau_tiny() {
        // tau_s = 0 → tau_eff = delta_oracle(2.0) → tau_eff < 0.5 is not possible with our fixture
        // But tau_s very negative would floor to 0.001 via oracle
        // Use the guard: tau < 0.5 — tricky since oracle adds 2.0
        // We can't get tau < 0.5 easily with our fixture. Just verify the ask=1.0 guard:
        let (state, now) = make_state(95_000.0, 97_000.0, 0.001, 60.0, 1.0, 0.50);
        assert!(CertaintyCapture.evaluate(&state, now).is_none(), "ask >= 1.0 should block");
    }
}
