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

        // Extract implied vol from our own market
        let our_mid_up = if state.up_bid > 0.0 && state.up_ask > 0.0 {
            (state.up_bid + state.up_ask) / 2.0
        } else {
            return None;
        };

        let our_iv = implied_vol(our_mid_up, s, k, tau, 15)?;

        // Extract implied vols from cross markets
        let mut iv_points: Vec<(f64, f64)> = Vec::with_capacity(4); // (tau, sigma_imp)
        iv_points.push((tau, our_iv));

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
                iv_points.push((cm_tau, iv));
            }
        }

        if iv_points.len() < 2 {
            return None;
        }

        // Fit power law: σ(τ) = a · τ^b in log-log space
        // ln(σ) = ln(a) + b·ln(τ)
        // Simple OLS with 2+ points
        let n = iv_points.len() as f64;
        let mut sum_x = 0.0_f64;
        let mut sum_y = 0.0_f64;
        let mut sum_xy = 0.0_f64;
        let mut sum_xx = 0.0_f64;

        for &(t, iv) in &iv_points {
            if t <= 0.0 || iv <= 0.0 {
                continue;
            }
            let x = t.ln();
            let y = iv.ln();
            sum_x += x;
            sum_y += y;
            sum_xy += x * y;
            sum_xx += x * x;
        }

        let denom = n * sum_xx - sum_x * sum_x;
        if denom.abs() < 1e-10 {
            return None;
        }

        let b = (n * sum_xy - sum_x * sum_y) / denom;
        let ln_a = (sum_y - b * sum_x) / n;

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
            size_frac: kelly(edge, market_ask).min(0.005),
            is_passive: false,
        })
    }
}
