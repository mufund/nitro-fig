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

    #[test]
    fn test_s_est() {
        let ob = OracleBasis::new(10.0, 2.0);
        assert_eq!(ob.s_est(100_000.0), 100_010.0);
    }

    #[test]
    fn test_tau_eff() {
        let ob = OracleBasis::new(0.0, 2.0);
        assert_eq!(ob.tau_eff(5.0), 7.0);
        // Floor at 0.001
        assert_eq!(ob.tau_eff(-5.0), 0.001);
    }

    #[test]
    fn test_zero_beta() {
        let ob = OracleBasis::new(0.0, 0.0);
        assert_eq!(ob.s_est(100_000.0), 100_000.0);
        assert_eq!(ob.tau_eff(300.0), 300.0);
    }
}
