/// Oracle basis model.
///
/// S_est(t) = S_binance(t) + beta
/// tau_eff  = tau + delta_oracle_s
///
/// beta: expected difference (oracle_price - binance_price) at settlement.
///   Calibrate from historical settlements. Typical: $0-$30 for BTC.
///
/// delta_oracle_s: oracle timestamp uncertainty in seconds.
///   Prevents z/d2 from going to infinity as tau → 0.
///   Calibrate from historical oracle timestamp jitter. Typical: 1-5s.
#[derive(Clone)]
pub struct OracleBasis {
    pub beta: f64,
    pub delta_oracle_s: f64,
}

impl OracleBasis {
    pub fn new(beta: f64, delta_oracle_s: f64) -> Self {
        Self {
            beta,
            delta_oracle_s,
        }
    }

    /// Estimate oracle-consistent price from Binance spot.
    #[inline]
    pub fn s_est(&self, binance_price: f64) -> f64 {
        binance_price + self.beta
    }

    /// Effective time to expiry incorporating oracle uncertainty.
    /// Floor at 0.001s to avoid division by zero.
    #[inline]
    pub fn tau_eff(&self, tau_s: f64) -> f64 {
        (tau_s + self.delta_oracle_s).max(0.001)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: OracleBasis with beta=10 applied to a Binance price of 100,000.
    /// Expected: s_est = 100,000 + 10 = 100,010, adding the oracle-vs-Binance basis offset.
    #[test]
    fn test_s_est() {
        let ob = OracleBasis::new(10.0, 2.0);
        assert_eq!(ob.s_est(100_000.0), 100_010.0);
    }

    /// Scenario: OracleBasis with delta_oracle_s=2 applied to tau=5s and tau=-5s.
    /// Expected: tau_eff(5) = 7.0 (adds jitter buffer); tau_eff(-5) = 0.001 (floor prevents zero/negative).
    #[test]
    fn test_tau_eff() {
        let ob = OracleBasis::new(0.0, 2.0);
        assert_eq!(ob.tau_eff(5.0), 7.0);
        // Floor at 0.001
        assert_eq!(ob.tau_eff(-5.0), 0.001);
    }

    /// Scenario: OracleBasis with beta=0 and delta_oracle_s=0 (no basis offset, no timestamp jitter).
    /// Expected: s_est is pass-through (100,000), tau_eff(300) = 300 (no adjustment).
    #[test]
    fn test_zero_beta() {
        let ob = OracleBasis::new(0.0, 0.0);
        assert_eq!(ob.s_est(100_000.0), 100_000.0);
        assert_eq!(ob.tau_eff(300.0), 300.0);
    }

    // ── Additional edge cases ──

    /// Scenario: OracleBasis with beta=-25 (oracle consistently settles below Binance).
    /// Expected: s_est(100,000) = 99,975, correctly subtracting the negative basis.
    #[test]
    fn test_negative_beta() {
        // Oracle consistently settles below Binance
        let ob = OracleBasis::new(-25.0, 2.0);
        assert_eq!(ob.s_est(100_000.0), 99_975.0);
    }

    /// Scenario: OracleBasis with delta_oracle_s=10 (very uncertain oracle timestamp, ~10s jitter).
    /// Expected: tau_eff(5) = 15.0 and tau_eff(0) = 10.0, dominated by the large jitter buffer.
    #[test]
    fn test_large_delta_oracle() {
        // Very uncertain oracle timestamp (e.g., 10 second jitter)
        let ob = OracleBasis::new(0.0, 10.0);
        assert_eq!(ob.tau_eff(5.0), 15.0);
        assert_eq!(ob.tau_eff(0.0), 10.0);
    }

    /// Scenario: OracleBasis with delta_oracle_s=0 and tau_s=0 (both zero).
    /// Expected: tau_eff floors to 0.001s to prevent division by zero in downstream calculations.
    #[test]
    fn test_tau_eff_floor_with_zero_delta() {
        // delta_oracle_s = 0, tau_s = 0 → should floor to 0.001
        let ob = OracleBasis::new(0.0, 0.0);
        assert_eq!(ob.tau_eff(0.0), 0.001);
    }

    /// Scenario: OracleBasis with delta_oracle_s=2 and a large negative tau_s=-100 (past expiry).
    /// Expected: tau_eff floors to 0.001s because -100 + 2 = -98 is still negative.
    #[test]
    fn test_tau_eff_floor_with_large_negative_tau() {
        // Even with large negative tau, floor applies
        let ob = OracleBasis::new(0.0, 2.0);
        assert_eq!(ob.tau_eff(-100.0), 0.001);
    }

    /// Scenario: OracleBasis with beta=10 applied to a Binance price of 0.
    /// Expected: s_est(0) = 10.0, showing beta is added unconditionally regardless of input price.
    #[test]
    fn test_s_est_zero_price() {
        let ob = OracleBasis::new(10.0, 2.0);
        assert_eq!(ob.s_est(0.0), 10.0);
    }

    /// Scenario: OracleBasis with a large beta=500 applied to a Binance price of 95,000.
    /// Expected: s_est = 95,000 + 500 = 95,500, confirming linear addition even with large offsets.
    #[test]
    fn test_s_est_large_beta() {
        let ob = OracleBasis::new(500.0, 2.0);
        assert_eq!(ob.s_est(95_000.0), 95_500.0);
    }
}
