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

    /// Scenario: S equals K (ATM) with sigma=0.001/s and tau=300s.
    /// Expected: p_fair near 0.50 (slightly below due to -sigma^2*tau/2 drift in d2).
    #[test]
    fn test_p_fair_atm() {
        // S = K should give ~0.50 (slightly less due to drift term)
        let p = p_fair(100_000.0, 100_000.0, 0.001, 300.0);
        assert!((p - 0.5).abs() < 0.01, "ATM p_fair = {}", p);
    }

    /// Scenario: S=105000 well above K=100000; ln(S/K)~0.049 dwarfs sigma*sqrt(tau)~0.017.
    /// Expected: d2 >> 1 so Phi(d2) > 0.95, confirming deep ITM pricing.
    #[test]
    fn test_p_fair_deep_itm() {
        // S well above K with realistic vol: sigma*sqrt(300) ≈ 0.017
        // ln(105000/100000) ≈ 0.049 >> 0.017 → d2 >> 1 → p near 1
        let p = p_fair(105_000.0, 100_000.0, 0.001, 300.0);
        assert!(p > 0.95, "Deep ITM p_fair = {}", p);
    }

    /// Scenario: S=95000 well below K=100000; ln(S/K)~-0.051 makes d2 << -1.
    /// Expected: Phi(d2) < 0.05, confirming deep OTM pricing near zero.
    #[test]
    fn test_p_fair_deep_otm() {
        // S well below K: ln(95000/100000) ≈ -0.051 → d2 << -1 → p near 0
        let p = p_fair(95_000.0, 100_000.0, 0.001, 300.0);
        assert!(p < 0.05, "Deep OTM p_fair = {}", p);
    }

    /// Scenario: ATM binary call with S=K=100000, sigma=0.001/s, tau=300s.
    /// Expected: delta_bin > 0 because binary call value always increases with spot price.
    #[test]
    fn test_delta_positive() {
        let d = delta_bin(100_000.0, 100_000.0, 0.001, 300.0);
        assert!(d > 0.0, "Delta should be positive: {}", d);
    }

    /// Scenario: Compute z_score for S slightly above and below K (100200 vs 99800).
    /// Expected: Positive z when S > K (ITM), negative z when S < K (OTM).
    #[test]
    fn test_z_score_basic() {
        let z = z_score(100_200.0, 100_000.0, 0.001, 300.0);
        assert!(z > 0.0, "z should be positive when S > K");
        let z2 = z_score(99_800.0, 100_000.0, 0.001, 300.0);
        assert!(z2 < 0.0, "z should be negative when S < K");
    }

    /// Scenario: Generate price from sigma_true=0.002, then recover sigma via Newton-Raphson.
    /// Expected: implied_vol converges and recovers sigma within 0.001 of the true value.
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

    /// Scenario: Market prices at extremes (0.005 and 0.995), outside the [0.01, 0.99] guard.
    /// Expected: implied_vol returns None immediately for prices that are too extreme to invert.
    #[test]
    fn test_implied_vol_extreme_prices() {
        assert!(implied_vol(0.005, 100_000.0, 100_000.0, 300.0, 20).is_none());
        assert!(implied_vol(0.995, 100_000.0, 100_000.0, 300.0, 20).is_none());
    }

    // ── d2 direct tests ──

    /// Scenario: ATM with S=K=100000; ln(S/K)=0 so d2 reduces to -0.5*sigma*sqrt(tau).
    /// Expected: d2 equals -0.5 * 0.001 * sqrt(300) exactly (pure drift term).
    #[test]
    fn test_d2_atm() {
        let d = d2(100_000.0, 100_000.0, 0.001, 300.0);
        let expected = -0.5 * 0.001 * 300.0_f64.sqrt();
        assert!((d - expected).abs() < 1e-10, "d2 ATM = {}, expected {}", d, expected);
    }

    /// Scenario: Call d2 with each invalid input (sigma=0, tau=0, S<0, K<0) one at a time.
    /// Expected: Each returns 0.0 via the guard clause, avoiding division by zero or log of negative.
    #[test]
    fn test_d2_guard_clauses() {
        assert_eq!(d2(100_000.0, 100_000.0, 0.0, 300.0), 0.0);
        assert_eq!(d2(100_000.0, 100_000.0, 0.001, 0.0), 0.0);
        assert_eq!(d2(-1.0, 100_000.0, 0.001, 300.0), 0.0);
        assert_eq!(d2(100_000.0, -1.0, 0.001, 300.0), 0.0);
    }

    // ── z_score edge cases ──

    /// Scenario: S exactly equals K so ln(S/K)=0.
    /// Expected: z_score is exactly 0.0 regardless of sigma and tau.
    #[test]
    fn test_z_score_at_money() {
        assert_eq!(z_score(100_000.0, 100_000.0, 0.001, 300.0), 0.0);
    }

    /// Scenario: Call z_score with sigma=0, tau=0, and S=0 respectively.
    /// Expected: Each returns 0.0 via guard clause, preventing division by zero.
    #[test]
    fn test_z_score_guard_clauses() {
        assert_eq!(z_score(100_000.0, 100_000.0, 0.0, 300.0), 0.0);
        assert_eq!(z_score(100_000.0, 100_000.0, 0.001, 0.0), 0.0);
        assert_eq!(z_score(0.0, 100_000.0, 0.001, 300.0), 0.0);
    }

    /// Scenario: S=110000 vs K=100000 with tiny sigma=0.0001 and short tau=60s.
    /// Expected: z > 10 because ln(1.1)~0.095 divided by 0.0001*sqrt(60)~0.00077 is huge.
    #[test]
    fn test_z_score_large_move() {
        let z = z_score(110_000.0, 100_000.0, 0.0001, 60.0);
        assert!(z > 10.0, "Large move should give huge z: {}", z);
    }

    // ── delta_bin edge cases ──

    /// Scenario: Call delta_bin with S=0, sigma=0, and tau=0 respectively.
    /// Expected: Each returns 0.0 via guard clause to avoid division by zero in phi(d2)/(S*sigma*sqrt(tau)).
    #[test]
    fn test_delta_bin_guard_clauses() {
        assert_eq!(delta_bin(0.0, 100_000.0, 0.001, 300.0), 0.0);
        assert_eq!(delta_bin(100_000.0, 100_000.0, 0.0, 300.0), 0.0);
        assert_eq!(delta_bin(100_000.0, 100_000.0, 0.001, 0.0), 0.0);
    }

    /// Scenario: Compare delta_bin at ATM (S=K=100000) vs deep ITM (S=105000, K=100000).
    /// Expected: ATM delta exceeds ITM delta because phi(d2) peaks near d2=0 (ATM).
    #[test]
    fn test_delta_bin_peaks_atm() {
        let d_atm = delta_bin(100_000.0, 100_000.0, 0.001, 300.0);
        let d_itm = delta_bin(105_000.0, 100_000.0, 0.001, 300.0);
        assert!(d_atm > d_itm, "ATM delta {} should exceed ITM delta {}", d_atm, d_itm);
    }

    // ── gamma_bin tests ──

    /// Scenario: Call gamma_bin with S=0, sigma=0, and tau=0 respectively.
    /// Expected: Each returns 0.0 via guard clause to avoid NaN in the gamma formula.
    #[test]
    fn test_gamma_bin_guard_clauses() {
        assert_eq!(gamma_bin(0.0, 100_000.0, 0.001, 300.0), 0.0);
        assert_eq!(gamma_bin(100_000.0, 100_000.0, 0.0, 300.0), 0.0);
        assert_eq!(gamma_bin(100_000.0, 100_000.0, 0.001, 0.0), 0.0);
    }

    /// Scenario: ATM binary call with S=K=100000, sigma=0.001/s, tau=300s.
    /// Expected: gamma_bin is nonzero because the curvature of Phi(d2) is maximal near ATM.
    #[test]
    fn test_gamma_bin_nonzero_atm() {
        let g = gamma_bin(100_000.0, 100_000.0, 0.001, 300.0);
        assert!(g != 0.0, "Gamma at ATM should be nonzero: {}", g);
    }

    /// Scenario: Deep ITM with S=110000 >> K=100000; d2 is large and positive.
    /// Expected: gamma_bin < 0 because the binary payoff curve is concave above the strike.
    #[test]
    fn test_gamma_bin_deep_itm_negative() {
        let g = gamma_bin(110_000.0, 100_000.0, 0.001, 300.0);
        assert!(g < 0.0, "Deep ITM gamma should be negative: {}", g);
    }

    // ── vega_bin tests ──

    /// Scenario: Call vega_bin with sigma=0 and tau=0 respectively.
    /// Expected: Each returns 0.0 via guard clause to avoid division by zero in d2/sigma.
    #[test]
    fn test_vega_bin_guard_clauses() {
        assert_eq!(vega_bin(100_000.0, 100_000.0, 0.0, 300.0), 0.0);
        assert_eq!(vega_bin(100_000.0, 100_000.0, 0.001, 0.0), 0.0);
    }

    /// Scenario: ATM binary call with S=K=100000, sigma=0.001/s, tau=300s.
    /// Expected: vega_bin is nonzero because price sensitivity to vol is highest near ATM.
    #[test]
    fn test_vega_bin_nonzero_atm() {
        let v = vega_bin(100_000.0, 100_000.0, 0.001, 300.0);
        assert!(v != 0.0, "Vega at ATM should be nonzero: {}", v);
    }

    // ── p_fair edge cases ──

    /// Scenario: sigma=0 so d2 returns 0.0 via its guard clause, regardless of S > K.
    /// Expected: p_fair = Phi(0) = 0.5 within CDF approximation error (~7.5e-8).
    #[test]
    fn test_p_fair_zero_sigma() {
        // sigma=0 → d2=0 → cdf(0) ≈ 0.5 (CDF approximation has ~7.5e-8 max error)
        let p = p_fair(105_000.0, 100_000.0, 0.0, 300.0);
        assert!((p - 0.5).abs() < 1e-8, "Zero sigma → p=0.5: {}", p);
    }

    // ── implied_vol edge cases ──

    /// Scenario: tau=0 with a valid market price of 0.50.
    /// Expected: implied_vol returns None because tau <= 0 triggers the early guard.
    #[test]
    fn test_implied_vol_tau_zero() {
        assert!(implied_vol(0.50, 100_000.0, 100_000.0, 0.0, 20).is_none());
    }

    /// Scenario: ITM case (S=105000 > K=100000) with sigma_true=0.001; generate price then invert.
    /// Expected: Newton-Raphson recovers sigma within 0.01 if the price is in the invertible range.
    #[test]
    fn test_implied_vol_roundtrip_itm() {
        let sigma_true = 0.001;
        let s = 105_000.0;
        let k = 100_000.0;
        let tau = 300.0;
        let p = p_fair(s, k, sigma_true, tau);
        if p > 0.01 && p < 0.99 {
            let sigma_inv = implied_vol(p, s, k, tau, 30);
            assert!(sigma_inv.is_some(), "Should converge for ITM: p={}", p);
            let err = (sigma_inv.unwrap() - sigma_true).abs();
            assert!(err < 0.01, "IV error for ITM = {}", err);
        }
    }

    // ── Larger dataset: p_fair monotonicity across price range ──

    /// Scenario: Sweep S from 95000 to 105000 with fixed K=100000, sigma=0.001, tau=300.
    /// Expected: p_fair is strictly monotonically increasing in spot price S.
    #[test]
    fn test_p_fair_monotonic_in_price() {
        let k = 100_000.0;
        let sigma = 0.001;
        let tau = 300.0;
        let prices = [95_000.0, 97_000.0, 99_000.0, 100_000.0, 101_000.0, 103_000.0, 105_000.0];
        let mut prev_p = 0.0;
        for &s in &prices {
            let p = p_fair(s, k, sigma, tau);
            assert!(p >= prev_p, "p_fair should be monotonic: s={}, p={}, prev={}", s, p, prev_p);
            prev_p = p;
        }
    }

    /// Scenario: Slightly ITM (S=100500, K=100000) across tau values from 30s to 600s.
    /// Expected: p_fair stays in (0, 1) for all time horizons, confirming valid probabilities.
    #[test]
    fn test_p_fair_across_tau_values() {
        // At ATM, shorter tau → p_fair closer to 0.5 (less time to drift)
        let s = 100_500.0;
        let k = 100_000.0;
        let sigma = 0.001;
        let taus = [30.0, 60.0, 120.0, 300.0, 600.0];
        for &tau in &taus {
            let p = p_fair(s, k, sigma, tau);
            assert!(p > 0.0 && p < 1.0, "p_fair in bounds: tau={}, p={}", tau, p);
        }
    }

    /// Scenario: ATM roundtrip for sigma in [0.0005, 0.001, 0.002, 0.005] with tau=300s.
    /// Expected: Newton-Raphson recovers each sigma within 0.001 when the price is invertible.
    #[test]
    fn test_implied_vol_roundtrip_across_sigma() {
        // Test IV roundtrip for various true sigma values
        let s = 100_000.0;
        let k = 100_000.0;
        let tau = 300.0;
        let sigmas = [0.0005, 0.001, 0.002, 0.005];
        for &sigma_true in &sigmas {
            let p = p_fair(s, k, sigma_true, tau);
            if p > 0.01 && p < 0.99 {
                let sigma_inv = implied_vol(p, s, k, tau, 30);
                assert!(sigma_inv.is_some(), "IV should converge for sigma={}", sigma_true);
                let err = (sigma_inv.unwrap() - sigma_true).abs();
                assert!(err < 0.001, "IV roundtrip error for sigma={}: err={}", sigma_true, err);
            }
        }
    }
}
