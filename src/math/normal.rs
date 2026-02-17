/// Standard normal PDF: phi(x) = (1/sqrt(2*pi)) * exp(-x^2/2)
#[inline]
pub fn phi(x: f64) -> f64 {
    const INV_SQRT_2PI: f64 = 0.398_942_280_401_432_7; // 1/sqrt(2*pi)
    INV_SQRT_2PI * (-0.5 * x * x).exp()
}

/// Standard normal CDF: Phi(x) via Abramowitz & Stegun 26.2.17
/// Max error < 7.5e-8. Zero heap allocation.
#[inline]
pub fn cdf(x: f64) -> f64 {
    if x >= 0.0 {
        const P: f64 = 0.231_641_9;
        const B1: f64 = 0.319_381_530;
        const B2: f64 = -0.356_563_782;
        const B3: f64 = 1.781_477_937;
        const B4: f64 = -1.821_255_978;
        const B5: f64 = 1.330_274_429;

        let t = 1.0 / (1.0 + P * x);
        let t2 = t * t;
        let t3 = t2 * t;
        let t4 = t3 * t;
        let t5 = t4 * t;
        1.0 - phi(x) * (B1 * t + B2 * t2 + B3 * t3 + B4 * t4 + B5 * t5)
    } else {
        1.0 - cdf(-x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: Standard normal PDF evaluated at x=0 (the peak of the bell curve).
    /// Expected: Returns 1/sqrt(2*pi) ≈ 0.3989, the maximum value of phi(x), to within 1e-12.
    #[test]
    fn test_phi_zero() {
        let v = phi(0.0);
        assert!((v - 0.398_942_280_401_432_7).abs() < 1e-12);
    }

    /// Scenario: Standard normal PDF evaluated at x=1 and x=-1.
    /// Expected: phi(1) == phi(-1) since the standard normal PDF is symmetric about zero.
    #[test]
    fn test_phi_symmetry() {
        assert!((phi(1.0) - phi(-1.0)).abs() < 1e-15);
    }

    /// Scenario: Standard normal CDF evaluated at x=0.
    /// Expected: Returns 0.5 (half the distribution lies below the mean) to within 1e-7.
    #[test]
    fn test_cdf_zero() {
        assert!((cdf(0.0) - 0.5).abs() < 1e-7);
    }

    /// Scenario: CDF evaluated at well-known z-scores: +/-1.96, +/-1.0, and 3.0.
    /// Expected: Matches standard normal table values (e.g., Phi(1.96) ≈ 0.975, Phi(1.0) ≈ 0.8413) to within 1e-5.
    #[test]
    fn test_cdf_known_values() {
        // Phi(1.96) ≈ 0.97500
        assert!((cdf(1.96) - 0.975_002_1).abs() < 1e-5);
        // Phi(-1.96) ≈ 0.02500
        assert!((cdf(-1.96) - 0.024_997_9).abs() < 1e-5);
        // Phi(1.0) ≈ 0.84134
        assert!((cdf(1.0) - 0.841_344_7).abs() < 1e-5);
        // Phi(-1.0) ≈ 0.15866
        assert!((cdf(-1.0) - 0.158_655_3).abs() < 1e-5);
        // Phi(3.0) ≈ 0.99865
        assert!((cdf(3.0) - 0.998_650_1).abs() < 1e-5);
    }

    /// Scenario: CDF evaluated at pairs of +x and -x for x in {0.5, 1.0, 1.5, 2.0, 2.5, 3.0}.
    /// Expected: Phi(x) + Phi(-x) == 1.0 for all x, confirming the reflection symmetry identity.
    #[test]
    fn test_cdf_symmetry() {
        for &x in &[0.5, 1.0, 1.5, 2.0, 2.5, 3.0] {
            assert!((cdf(x) + cdf(-x) - 1.0).abs() < 1e-7);
        }
    }

    /// Scenario: CDF evaluated at extreme values x=10 and x=-10 (far into the tails).
    /// Expected: Phi(10) > 0.999999 and Phi(-10) < 1e-6, confirming correct tail behavior.
    #[test]
    fn test_cdf_extremes() {
        assert!(cdf(10.0) > 0.999_999);
        assert!(cdf(-10.0) < 1e-6);
    }
}
