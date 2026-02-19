use crate::engine::state::MarketState;
use crate::math::pricing::implied_vol;
use crate::strategies::{kelly, Strategy};
use crate::types::{EvalTrigger, Side, Signal};

/// Edge 4: Cross-Timeframe Relative Value
///
/// Extract implied vol from multiple expiry windows (5m, 15m, 1h).
/// Fit σ(τ) = a · τ^b (power law), trade outliers vs fitted curve.
/// Self-disables if < 2 cross-markets available.
pub struct CrossTimeframe;

const MIN_VOL_DEVIATION: f64 = 0.05; // 5 vol points minimum outlier
const MIN_EDGE: f64 = 0.01;
const DEPTH_WEIGHT_LEVELS: usize = 3; // levels for depth confidence weighting in OLS

impl Strategy for CrossTimeframe {
    fn name(&self) -> &'static str {
        "cross_timeframe"
    }

    fn trigger(&self) -> EvalTrigger {
        EvalTrigger::PolymarketQuote
    }

    #[inline]
    fn evaluate(&self, state: &MarketState, now_ms: i64) -> Option<Signal> {
        // Need at least 2 cross-market data points
        if state.cross_markets.len() < 1 {
            return None;
        }

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

        // Extract implied vol from our own market using microprice (size-weighted mid)
        // for more accurate IV extraction when book is asymmetric.
        // Falls back to naive mid if book data not populated.
        let our_mid_up = {
            let mp = state.up_book.microprice();
            if mp > 0.0 {
                mp
            } else if state.up_bid > 0.0 && state.up_ask > 0.0 {
                (state.up_bid + state.up_ask) / 2.0
            } else {
                return None;
            }
        };

        let our_iv = implied_vol(our_mid_up, s, k, tau, 15)?;

        // Depth confidence weight for our market: thicker books → more reliable IV.
        // Cross-market points get weight 1.0 (no book data in CrossMarketState).
        let our_weight = state
            .up_book
            .bid_depth(DEPTH_WEIGHT_LEVELS)
            .min(state.up_book.ask_depth(DEPTH_WEIGHT_LEVELS))
            .max(1.0);

        // Extract implied vols from cross markets: (tau, sigma_imp, weight)
        let mut iv_points: Vec<(f64, f64, f64)> = Vec::with_capacity(4);
        iv_points.push((tau, our_iv, our_weight));

        for (_interval, cm) in &state.cross_markets {
            let cm_tau = ((cm.end_ms - now_ms).max(1)) as f64 / 1000.0;
            if cm_tau < 10.0 {
                continue;
            }
            let cm_mid = if cm.up_bid > 0.0 && cm.up_ask > 0.0 {
                (cm.up_bid + cm.up_ask) / 2.0
            } else {
                continue;
            };
            if let Some(iv) = implied_vol(cm_mid, s, cm.strike, cm_tau, 15) {
                iv_points.push((cm_tau, iv, 1.0)); // no book data for cross markets
            }
        }

        if iv_points.len() < 2 {
            return None;
        }

        // Fit power law: σ(τ) = a · τ^b in log-log space
        // ln(σ) = ln(a) + b·ln(τ)
        // Weighted OLS: deeper books contribute more to the fit.
        let mut sum_w = 0.0_f64;
        let mut sum_wx = 0.0_f64;
        let mut sum_wy = 0.0_f64;
        let mut sum_wxy = 0.0_f64;
        let mut sum_wxx = 0.0_f64;

        for &(t, iv, w) in &iv_points {
            if t <= 0.0 || iv <= 0.0 {
                continue;
            }
            let x = t.ln();
            let y = iv.ln();
            sum_w += w;
            sum_wx += w * x;
            sum_wy += w * y;
            sum_wxy += w * x * y;
            sum_wxx += w * x * x;
        }

        let denom = sum_w * sum_wxx - sum_wx * sum_wx;
        if denom.abs() < 1e-10 {
            return None;
        }

        let b = (sum_w * sum_wxy - sum_wx * sum_wy) / denom;
        let ln_a = (sum_wy - b * sum_wx) / sum_w;

        // Compute fitted vol for our market
        let fitted_iv = (ln_a + b * tau.ln()).exp();
        let deviation = our_iv - fitted_iv;

        if deviation.abs() < MIN_VOL_DEVIATION {
            return None;
        }

        // Our market is overpriced (positive deviation) → sell
        // Our market is underpriced (negative deviation) → buy
        let (side, market_ask, fair) = if deviation > 0.0 {
            // Our implied vol is too high → probability overpriced → sell YES (buy DOWN if S>K)
            // Or more precisely: the YES side is overpriced if S > K
            if state.distance() > 0.0 {
                // UP overpriced → buy DOWN
                if state.down_ask <= 0.0 || state.down_ask >= 1.0 {
                    return None;
                }
                let fair_down = 1.0 - crate::math::pricing::p_fair(s, k, fitted_iv, tau);
                (Side::Down, state.down_ask, fair_down)
            } else {
                if state.up_ask <= 0.0 || state.up_ask >= 1.0 {
                    return None;
                }
                let fair_up = crate::math::pricing::p_fair(s, k, fitted_iv, tau);
                (Side::Up, state.up_ask, fair_up)
            }
        } else {
            // Our implied vol is too low → probability underpriced → buy
            if state.distance() > 0.0 {
                if state.up_ask <= 0.0 || state.up_ask >= 1.0 {
                    return None;
                }
                let fair_up = crate::math::pricing::p_fair(s, k, fitted_iv, tau);
                (Side::Up, state.up_ask, fair_up)
            } else {
                if state.down_ask <= 0.0 || state.down_ask >= 1.0 {
                    return None;
                }
                let fair_down = 1.0 - crate::math::pricing::p_fair(s, k, fitted_iv, tau);
                (Side::Down, state.down_ask, fair_down)
            }
        };

        let edge = fair - market_ask;
        if edge < MIN_EDGE {
            return None;
        }

        let confidence = (deviation.abs() / 0.15).clamp(0.3, 0.7);

        Some(Signal {
            strategy: "cross_timeframe",
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
    use crate::config::Interval;
    use crate::engine::state::CrossMarketState;
    use crate::strategies::test_helpers::*;

    /// Scenario: Default state with no cross-market data points.
    /// Expected: None -- needs at least 1 cross market to compare vol surfaces.
    #[test]
    fn test_none_when_no_cross_markets() {
        // Default state has no cross_markets → len < 1 → None
        let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.55, 0.45);
        assert!(CrossTimeframe.evaluate(&state, now).is_none());
    }

    /// Scenario: Cross market present but own bid/ask are both zero.
    /// Expected: None -- microprice and naive mid both fail, can't extract our IV.
    #[test]
    fn test_none_when_no_bid_ask_data() {
        // Cross market present but our own bid/ask are zero → microprice fails
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.0, 0.0);
        state.up_bid = 0.0;
        state.up_ask = 0.0;
        state.cross_markets.insert(Interval::M15, CrossMarketState {
            interval: Interval::M15,
            up_bid: 0.55,
            up_ask: 0.57,
            down_bid: 0.43,
            down_ask: 0.45,
            strike: 95_000.0,
            end_ms: now + 900_000,
        });
        assert!(CrossTimeframe.evaluate(&state, now).is_none());
    }

    /// Scenario: One cross market with only 5s remaining (tau < 10s threshold).
    /// Expected: None -- cross market skipped, leaving only 1 IV point; need >= 2 for OLS fit.
    #[test]
    fn test_none_when_insufficient_iv_points() {
        // Cross market with very small tau (< 10s) → skipped → only 1 IV point → None
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.55, 0.45);
        inject_book(&mut state, Side::Up,
            vec![(0.54, 50.0)],
            vec![(0.55, 50.0)],
        );
        state.cross_markets.insert(Interval::M15, CrossMarketState {
            interval: Interval::M15,
            up_bid: 0.55,
            up_ask: 0.57,
            down_bid: 0.43,
            down_ask: 0.45,
            strike: 95_000.0,
            end_ms: now + 5_000, // only 5s left → cm_tau < 10 → skipped
        });
        assert!(CrossTimeframe.evaluate(&state, now).is_none());
    }

    /// Scenario: Two cross markets (M15 and H1) with valid bid/ask and sufficient tau.
    /// Expected: No panics in OLS power-law fitting path; signal depends on IV deviation.
    #[test]
    fn test_with_valid_cross_markets() {
        // Two cross markets with different taus — OLS fitting should work
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.55, 0.45);
        inject_book(&mut state, Side::Up,
            vec![(0.54, 50.0)],
            vec![(0.55, 50.0)],
        );
        state.cross_markets.insert(Interval::M15, CrossMarketState {
            interval: Interval::M15,
            up_bid: 0.60,
            up_ask: 0.62,
            down_bid: 0.38,
            down_ask: 0.40,
            strike: 95_000.0,
            end_ms: now + 900_000,
        });
        state.cross_markets.insert(Interval::H1, CrossMarketState {
            interval: Interval::H1,
            up_bid: 0.58,
            up_ask: 0.60,
            down_bid: 0.40,
            down_ask: 0.42,
            strike: 95_000.0,
            end_ms: now + 3_600_000,
        });
        // May or may not produce signal — depends on IV deviation
        // This test verifies no panics in the OLS path with real data
        let _result = CrossTimeframe.evaluate(&state, now);
    }

    // ── Parameterized tests ──

    /// Scenario: All three cross intervals (M5, M15, H1) present giving 4 IV points for OLS.
    /// Expected: No panics with full vol surface; weighted OLS fits power law across all expiries.
    #[test]
    fn test_with_many_cross_markets() {
        // All three cross intervals present — should successfully run OLS with 4 IV points
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.55, 0.45);
        inject_book(&mut state, Side::Up,
            vec![(0.54, 50.0)],
            vec![(0.55, 50.0)],
        );
        state.cross_markets.insert(Interval::M5, CrossMarketState {
            interval: Interval::M5,
            up_bid: 0.53,
            up_ask: 0.55,
            down_bid: 0.45,
            down_ask: 0.47,
            strike: 95_000.0,
            end_ms: now + 300_000,
        });
        state.cross_markets.insert(Interval::M15, CrossMarketState {
            interval: Interval::M15,
            up_bid: 0.56,
            up_ask: 0.58,
            down_bid: 0.42,
            down_ask: 0.44,
            strike: 95_000.0,
            end_ms: now + 900_000,
        });
        state.cross_markets.insert(Interval::H1, CrossMarketState {
            interval: Interval::H1,
            up_bid: 0.54,
            up_ask: 0.56,
            down_bid: 0.44,
            down_ask: 0.46,
            strike: 95_000.0,
            end_ms: now + 3_600_000,
        });
        // Should not panic with 4 IV points in OLS fitting
        let _result = CrossTimeframe.evaluate(&state, now);
    }

    /// Scenario: Cross market with extreme UP price near 1.0 (up_ask=0.99, down_ask=0.02).
    /// Expected: No panics -- IV extraction may fail for extreme prices but handled gracefully.
    #[test]
    fn test_no_panic_with_extreme_prices() {
        // Cross market with very high UP price (near 1.0) — IV extraction may fail
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.55, 0.45);
        inject_book(&mut state, Side::Up,
            vec![(0.54, 50.0)],
            vec![(0.55, 50.0)],
        );
        state.cross_markets.insert(Interval::M15, CrossMarketState {
            interval: Interval::M15,
            up_bid: 0.98,
            up_ask: 0.99, // extreme price
            down_bid: 0.01,
            down_ask: 0.02,
            strike: 95_000.0,
            end_ms: now + 900_000,
        });
        // IV extraction may fail for extreme prices — but should not panic
        let _result = CrossTimeframe.evaluate(&state, now);
    }

    /// Scenario: Realized vol is zero with a cross market present.
    /// Expected: None -- sigma=0 blocks IV extraction and fair value computation.
    #[test]
    fn test_none_when_sigma_zero() {
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.0, 120.0, 0.55, 0.45);
        state.cross_markets.insert(Interval::M15, CrossMarketState {
            interval: Interval::M15,
            up_bid: 0.55, up_ask: 0.57, down_bid: 0.43, down_ask: 0.45,
            strike: 95_000.0, end_ms: now + 900_000,
        });
        assert!(CrossTimeframe.evaluate(&state, now).is_none());
    }

    /// Scenario: tau=20s (below 30s minimum) with a cross market present.
    /// Expected: None -- too close to expiry for cross-timeframe vol surface analysis.
    #[test]
    fn test_none_when_tau_short() {
        // tau < 30s → None
        let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 20.0, 0.55, 0.45);
        state.cross_markets.insert(Interval::M15, CrossMarketState {
            interval: Interval::M15,
            up_bid: 0.55, up_ask: 0.57, down_bid: 0.43, down_ask: 0.45,
            strike: 95_000.0, end_ms: now + 900_000,
        });
        assert!(CrossTimeframe.evaluate(&state, now).is_none());
    }
}
