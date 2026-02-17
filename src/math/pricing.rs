use super::normal::{cdf, phi};

/// d2 = [ln(S/K) - sigma^2 * tau / 2] / (sigma * sqrt(tau))
/// tau is in seconds, sigma is in per-second units.
#[inline]
pub fn d2(s: f64, k: f64, sigma: f64, tau: f64) -> f64 {
    if sigma <= 0.0 || tau <= 0.0 || s <= 0.0 || k <= 0.0 {
        return 0.0;
    }
    let sqrt_tau = tau.sqrt();
    ((s / k).ln() - 0.5 * sigma * sigma * tau) / (sigma * sqrt_tau)
}

/// Fair price of binary call: P(S_T > K) = Phi(d2)
#[inline]
pub fn p_fair(s: f64, k: f64, sigma: f64, tau: f64) -> f64 {
    cdf(d2(s, k, sigma, tau))
}

/// z-score for certainty measurement: z = ln(S/K) / (sigma * sqrt(tau))
/// Omits the drift term for simplicity (negligible for short tau).
#[inline]
pub fn z_score(s: f64, k: f64, sigma: f64, tau: f64) -> f64 {
    if sigma <= 0.0 || tau <= 0.0 || s <= 0.0 || k <= 0.0 {
        return 0.0;
    }
    (s / k).ln() / (sigma * tau.sqrt())
}

/// Binary delta: dP/dS = phi(d2) / (S * sigma * sqrt(tau))
#[inline]
pub fn delta_bin(s: f64, k: f64, sigma: f64, tau: f64) -> f64 {
    if s <= 0.0 || sigma <= 0.0 || tau <= 0.0 {
        return 0.0;
    }
    let d = d2(s, k, sigma, tau);
    phi(d) / (s * sigma * tau.sqrt())
}

/// Binary gamma: d^2P/dS^2
/// = -phi(d2) / (S^2 * sigma * sqrt(tau)) * [1 + d2 / (sigma * sqrt(tau))]
#[inline]
pub fn gamma_bin(s: f64, k: f64, sigma: f64, tau: f64) -> f64 {
    if s <= 0.0 || sigma <= 0.0 || tau <= 0.0 {
        return 0.0;
    }
    let sqrt_tau = tau.sqrt();
    let sig_sqrt_tau = sigma * sqrt_tau;
    let d = d2(s, k, sigma, tau);
    -phi(d) / (s * s * sig_sqrt_tau) * (1.0 + d / sig_sqrt_tau)
}

/// Binary vega: dP/dsigma = phi(d2) * (-sqrt(tau) - d2/sigma)
#[inline]
pub fn vega_bin(s: f64, k: f64, sigma: f64, tau: f64) -> f64 {
    if sigma <= 0.0 || tau <= 0.0 {
        return 0.0;
    }
    let sqrt_tau = tau.sqrt();
    let d = d2(s, k, sigma, tau);
    phi(d) * (-sqrt_tau - d / sigma)
}

/// Newton-Raphson implied vol from market price.
/// Returns None if convergence fails.
/// Not on hot path — called during cross-timeframe analysis.
pub fn implied_vol(market_price: f64, s: f64, k: f64, tau: f64, max_iter: u32) -> Option<f64> {
    if market_price <= 0.01 || market_price >= 0.99 || tau <= 0.0 {
        return None;
    }
    let mut sigma = 0.60; // initial guess (annualized-ish)
    for _ in 0..max_iter {
        let p = p_fair(s, k, sigma, tau);
        let v = vega_bin(s, k, sigma, tau);
        if v.abs() < 1e-12 {
            break;
        }
        let diff = p - market_price;
        if diff.abs() < 1e-6 {
            return Some(sigma);
        }
        sigma -= diff / v;
        sigma = sigma.clamp(0.001, 50.0);
    }
    let final_p = p_fair(s, k, sigma, tau);
    if (final_p - market_price).abs() < 0.01 {
        Some(sigma)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_p_fair_atm() {
        // S = K should give ~0.50 (slightly less due to drift term)
        let p = p_fair(100_000.0, 100_000.0, 0.001, 300.0);
        assert!((p - 0.5).abs() < 0.01, "ATM p_fair = {}", p);
    }

    #[test]
    fn test_p_fair_deep_itm() {
        // S well above K with realistic vol: sigma*sqrt(300) ≈ 0.017
        // ln(105000/100000) ≈ 0.049 >> 0.017 → d2 >> 1 → p near 1
        let p = p_fair(105_000.0, 100_000.0, 0.001, 300.0);
        assert!(p > 0.95, "Deep ITM p_fair = {}", p);
    }

    #[test]
    fn test_p_fair_deep_otm() {
        // S well below K: ln(95000/100000) ≈ -0.051 → d2 << -1 → p near 0
        let p = p_fair(95_000.0, 100_000.0, 0.001, 300.0);
        assert!(p < 0.05, "Deep OTM p_fair = {}", p);
    }

    #[test]
    fn test_delta_positive() {
        let d = delta_bin(100_000.0, 100_000.0, 0.001, 300.0);
        assert!(d > 0.0, "Delta should be positive: {}", d);
    }

    #[test]
    fn test_z_score_basic() {
        let z = z_score(100_200.0, 100_000.0, 0.001, 300.0);
        assert!(z > 0.0, "z should be positive when S > K");
        let z2 = z_score(99_800.0, 100_000.0, 0.001, 300.0);
        assert!(z2 < 0.0, "z should be negative when S < K");
    }

    #[test]
    fn test_implied_vol_roundtrip() {
        let sigma_true = 0.002;
        let s = 100_000.0;
        let k = 100_000.0;
        let tau = 300.0;
        let p = p_fair(s, k, sigma_true, tau);
        let sigma_inv = implied_vol(p, s, k, tau, 20);
        assert!(sigma_inv.is_some(), "Newton should converge");
        let err = (sigma_inv.unwrap() - sigma_true).abs();
        assert!(err < 0.001, "Implied vol error = {}", err);
    }

    #[test]
    fn test_implied_vol_extreme_prices() {
        assert!(implied_vol(0.005, 100_000.0, 100_000.0, 300.0, 20).is_none());
        assert!(implied_vol(0.995, 100_000.0, 100_000.0, 300.0, 20).is_none());
    }
}
