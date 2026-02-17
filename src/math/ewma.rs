/// EWMA realized volatility tracker.
/// sigma_sq(t) = lambda * sigma_sq(t-1) + (1-lambda) * r(t)^2
/// where r(t) = ln(price_t / price_{t-1})
///
/// The raw sigma is in "return per tick" units. Callers must convert
/// to per-second sigma using the observed trade rate.
pub struct EwmaVol {
    lambda: f64,
    sigma_sq: f64,
    last_price: f64,
    n_updates: u32,
    min_updates: u32,
}

impl EwmaVol {
    pub fn new(lambda: f64, min_updates: u32) -> Self {
        Self {
            lambda,
            sigma_sq: 0.0,
            last_price: 0.0,
            n_updates: 0,
            min_updates,
        }
    }

    /// Update with new trade price. O(1), zero allocation.
    #[inline]
    pub fn update(&mut self, price: f64) {
        if self.last_price > 0.0 && price > 0.0 {
            let r = (price / self.last_price).ln();
            self.sigma_sq = self.lambda * self.sigma_sq + (1.0 - self.lambda) * r * r;
            self.n_updates += 1;
        }
        self.last_price = price;
    }

    /// Raw EWMA variance (per-tick).
    #[inline]
    pub fn sigma_sq(&self) -> f64 {
        self.sigma_sq
    }

    /// Realized vol in return-per-tick units: sqrt(sigma_sq).
    #[inline]
    pub fn sigma(&self) -> f64 {
        self.sigma_sq.sqrt()
    }

    /// Whether we have enough data points for reliable estimates.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.n_updates >= self.min_updates
    }

    #[inline]
    pub fn n_updates(&self) -> u32 {
        self.n_updates
    }
}

/// 1-second sampled EWMA realized vol.
/// Instead of updating on every tick (~100/s, mostly identical prices),
/// samples once per second, computing log-returns between samples.
/// sigma is directly in per-second units — no trades_per_sec conversion needed.
#[derive(Clone)]
pub struct SampledEwmaVol {
    lambda: f64,
    sigma_sq: f64,
    last_sample_price: f64,
    last_sample_ts: i64,
    seeded: bool,
    n_samples: u32,
    min_samples: u32,
}

impl SampledEwmaVol {
    pub fn new(lambda: f64, min_samples: u32) -> Self {
        Self {
            lambda,
            sigma_sq: 0.0,
            last_sample_price: 0.0,
            last_sample_ts: 0,
            seeded: false,
            n_samples: 0,
            min_samples,
        }
    }

    /// Update with a new trade. Only computes a return when ≥1000ms have elapsed.
    /// Returns true if a new sample was taken (sigma_sq updated).
    #[inline]
    pub fn update(&mut self, price: f64, ts_ms: i64) -> bool {
        if price <= 0.0 {
            return false;
        }
        if !self.seeded {
            // First ever price — seed, no return yet
            self.last_sample_price = price;
            self.last_sample_ts = ts_ms;
            self.seeded = true;
            return false;
        }
        let elapsed = ts_ms - self.last_sample_ts;
        if elapsed < 1000 {
            return false;
        }
        // Compute log-return normalized to per-second
        let dt_s = elapsed as f64 / 1000.0;
        let r = (price / self.last_sample_price).ln();
        let r_sq_per_sec = (r * r) / dt_s;

        self.sigma_sq = self.lambda * self.sigma_sq + (1.0 - self.lambda) * r_sq_per_sec;
        self.n_samples += 1;
        self.last_sample_price = price;
        self.last_sample_ts = ts_ms;
        true
    }

    /// Per-second realized vol. Directly usable — no conversion needed.
    #[inline]
    pub fn sigma(&self) -> f64 {
        self.sigma_sq.sqrt()
    }

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.n_samples >= self.min_samples
    }

    #[inline]
    pub fn n_samples(&self) -> u32 {
        self.n_samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: EwmaVol with lambda=0.94 fed 10 identical prices of 100.0.
    /// Expected: sigma ≈ 0 because constant prices produce zero log-returns.
    #[test]
    fn test_ewma_basic() {
        let mut vol = EwmaVol::new(0.94, 5);
        // Feed constant price → sigma should be ~0
        for _ in 0..10 {
            vol.update(100.0);
        }
        assert!(vol.sigma() < 1e-10, "Constant price sigma = {}", vol.sigma());
    }

    /// Scenario: EwmaVol with lambda=0.94 fed 100 prices alternating between 100.0 and 101.0.
    /// Expected: sigma > 0 (non-zero vol from price oscillation) but < 0.1 (a ~1% move is small).
    #[test]
    fn test_ewma_volatile() {
        let mut vol = EwmaVol::new(0.94, 5);
        // Alternating prices → sigma should be > 0
        for i in 0..100 {
            let price = if i % 2 == 0 { 100.0 } else { 101.0 };
            vol.update(price);
        }
        assert!(vol.is_valid());
        assert!(vol.sigma() > 0.0, "Volatile sigma should be > 0");
        assert!(vol.sigma() < 0.1, "sigma = {}", vol.sigma());
    }

    /// Scenario: EwmaVol with min_updates=50 fed 50 prices (yielding 49 returns), then one more.
    /// Expected: is_valid() is false after 49 returns, true after the 50th return.
    #[test]
    fn test_ewma_validity() {
        let mut vol = EwmaVol::new(0.94, 50);
        // First price sets last_price, no return computed.
        // Need 51 prices total to get 50 returns.
        for i in 0..50 {
            vol.update(100.0 + i as f64 * 0.01);
        }
        assert!(!vol.is_valid()); // 49 returns, not enough
        vol.update(100.5);        // 50th return
        assert!(vol.is_valid());
    }

    /// Scenario: EwmaVol with min_updates=300 fed 300 prices (yielding 299 returns), then one more.
    /// Expected: is_valid() is false after 299 returns, true after the 300th, simulating ~3s of Binance trade data.
    #[test]
    fn test_ewma_validity_300() {
        let mut vol = EwmaVol::new(0.94, 300);
        // Simulate ~3s of Binance trades
        for i in 0..300 {
            vol.update(100_000.0 + (i as f64 * 0.01).sin());
        }
        assert!(!vol.is_valid()); // 299 returns
        vol.update(100_000.5);     // 300th return
        assert!(vol.is_valid());
    }

    /// Scenario: SampledEwmaVol fed constant price 100,000 at 1-second intervals for 10 ticks.
    /// Expected: First update seeds only (returns false); subsequent updates sample (return true); sigma ≈ 0.
    #[test]
    fn test_sampled_ewma_basic() {
        let mut vol = SampledEwmaVol::new(0.94, 5);
        let base_price = 100_000.0;
        // Feed prices at 1-second intervals
        for i in 0..10 {
            let sampled = vol.update(base_price, i * 1000);
            if i == 0 {
                assert!(!sampled, "First price should just seed");
            } else {
                assert!(sampled, "Each subsequent 1s gap should sample");
            }
        }
        // Constant price → sigma ≈ 0
        assert!(vol.sigma() < 1e-10, "Constant price sigma = {}", vol.sigma());
    }

    /// Scenario: SampledEwmaVol fed prices alternating between 100,000 and 100,100 at 1-second intervals.
    /// Expected: sigma > 0 (real volatility from 0.1% swings) and < 0.01 per second, and is_valid after 5+ samples.
    #[test]
    fn test_sampled_ewma_volatile() {
        let mut vol = SampledEwmaVol::new(0.94, 5);
        // Alternating prices at 1-second intervals → actual vol
        for i in 0..20 {
            let price = if i % 2 == 0 { 100_000.0 } else { 100_100.0 };
            vol.update(price, i * 1000);
        }
        assert!(vol.is_valid());
        assert!(vol.sigma() > 0.0, "Volatile sigma should be > 0");
        // sigma should be roughly 0.001/s (0.1% per second)
        assert!(vol.sigma() < 0.01, "sigma = {}", vol.sigma());
    }

    /// Scenario: SampledEwmaVol seeded at t=0, then 99 rapid ticks spaced 10ms apart (all < 1s from seed).
    /// Expected: All sub-second updates return false and n_samples stays at 0 (no sampling occurs).
    #[test]
    fn test_sampled_ewma_skips_sub_second() {
        let mut vol = SampledEwmaVol::new(0.94, 5);
        vol.update(100_000.0, 0);
        // Rapid-fire ticks within 1 second — should all be ignored
        for i in 1..100 {
            let sampled = vol.update(100_010.0, i * 10); // 10ms apart
            assert!(!sampled, "Sub-second tick at {}ms should not sample", i * 10);
        }
        assert_eq!(vol.n_samples(), 0);
    }

    /// Scenario: SampledEwmaVol with min_samples=10 fed 11 prices at 1-second intervals (first seeds, 9 sample).
    /// Expected: is_valid() is false after 9 samples, true after the 10th sample at t=10s.
    #[test]
    fn test_sampled_ewma_validity() {
        let mut vol = SampledEwmaVol::new(0.94, 10);
        for i in 0..10 {
            vol.update(100_000.0 + i as f64, i * 1000);
        }
        assert!(!vol.is_valid()); // 9 samples (first is seed)
        vol.update(100_010.0, 10_000);
        assert!(vol.is_valid()); // 10th sample
    }

    // ── EwmaVol edge cases ──

    /// Scenario: EwmaVol receives a zero price after seeding, then recovers with valid prices.
    /// Expected: Zero prices are skipped (no return computed); n_updates stays 0 until two consecutive valid prices appear.
    #[test]
    fn test_ewma_zero_price_ignored() {
        let mut vol = EwmaVol::new(0.94, 5);
        vol.update(100.0); // seeds last_price
        vol.update(0.0);   // price=0 → guard skips
        // n_updates should still be 0 since ln(0/100) would be -inf
        assert_eq!(vol.n_updates(), 0);
        // last_price is set to 0.0 though
        vol.update(100.0); // last_price was 0 → guard skips again
        assert_eq!(vol.n_updates(), 0);
        vol.update(101.0); // now last_price=100 > 0, price=101 > 0 → return computed
        assert_eq!(vol.n_updates(), 1);
    }

    /// Scenario: EwmaVol with lambda=0.94 updated with prices 100 then 101 (r = ln(1.01)).
    /// Expected: sigma_sq = (1-0.94) * r^2 = 0.06 * ln(1.01)^2, and sigma = sqrt(sigma_sq).
    #[test]
    fn test_ewma_sigma_sq_accessor() {
        let mut vol = EwmaVol::new(0.94, 1);
        vol.update(100.0);
        vol.update(101.0); // r = ln(1.01) ≈ 0.00995
        let expected_r_sq = (101.0_f64 / 100.0).ln().powi(2);
        // sigma_sq = lambda * 0 + (1-lambda) * r^2 = 0.06 * r^2
        let expected_sq = 0.06 * expected_r_sq;
        assert!((vol.sigma_sq() - expected_sq).abs() < 1e-15, "sigma_sq = {}", vol.sigma_sq());
        assert!((vol.sigma() - expected_sq.sqrt()).abs() < 1e-15);
    }

    /// Scenario: EwmaVol seeded with one price, then fed 10 more identical prices.
    /// Expected: n_updates is 0 after the seed (no return yet), then increments to 10 after 10 return computations.
    #[test]
    fn test_ewma_n_updates_counter() {
        let mut vol = EwmaVol::new(0.94, 5);
        assert_eq!(vol.n_updates(), 0);
        vol.update(100.0); // seed only
        assert_eq!(vol.n_updates(), 0);
        for _ in 0..10 {
            vol.update(100.5);
        }
        // 10 returns computed (100→100.5 x10)
        assert_eq!(vol.n_updates(), 10);
    }

    /// Scenario: EwmaVol with lambda=0 (no memory) fed two price moves.
    /// Expected: sigma_sq always equals the latest r^2, fully forgetting previous returns.
    #[test]
    fn test_ewma_lambda_zero() {
        // lambda=0 → sigma_sq = r^2 (no memory)
        let mut vol = EwmaVol::new(0.0, 1);
        vol.update(100.0);
        vol.update(110.0); // r = ln(1.1)
        let r1 = (110.0_f64 / 100.0).ln();
        assert!((vol.sigma_sq() - r1 * r1).abs() < 1e-15);
        vol.update(111.0); // r = ln(111/110)
        let r2 = (111.0_f64 / 110.0).ln();
        // lambda=0 → fully forgets previous: sigma_sq = r2^2
        assert!((vol.sigma_sq() - r2 * r2).abs() < 1e-15);
    }

    /// Scenario: EwmaVol with lambda=1 (infinite memory, zero weight on new data) fed a 100% price move.
    /// Expected: sigma_sq remains 0 because (1-lambda)=0 gives zero weight to new returns.
    #[test]
    fn test_ewma_lambda_one() {
        // lambda=1 → sigma_sq never changes from initial (always multiplied by 1, new term weight = 0)
        let mut vol = EwmaVol::new(1.0, 1);
        vol.update(100.0);
        vol.update(200.0); // huge move, but (1-lambda)=0 so no effect
        assert_eq!(vol.sigma_sq(), 0.0);
    }

    // ── SampledEwmaVol edge cases ──

    /// Scenario: SampledEwmaVol receives price=0 first (rejected), then valid prices at 1-second intervals.
    /// Expected: Zero price does not seed; the first valid price seeds, and the second valid price produces the first sample.
    #[test]
    fn test_sampled_ewma_zero_price_rejected() {
        let mut vol = SampledEwmaVol::new(0.94, 5);
        let sampled = vol.update(0.0, 0);
        assert!(!sampled);
        // Should not seed — next valid price should seed instead
        let sampled = vol.update(100.0, 1000);
        assert!(!sampled); // seeds, doesn't sample
        let sampled = vol.update(101.0, 2000);
        assert!(sampled); // now computes return
        assert_eq!(vol.n_samples(), 1);
    }

    /// Scenario: SampledEwmaVol seeded at t=0, next update at t=5s with a small price change.
    /// Expected: r_sq_per_sec = ln(100100/100000)^2 / 5.0, and sigma_sq = (1-lambda) * r_sq_per_sec, normalizing for the multi-second gap.
    #[test]
    fn test_sampled_ewma_multi_second_gap() {
        let mut vol = SampledEwmaVol::new(0.94, 1);
        vol.update(100_000.0, 0);       // seed
        vol.update(100_100.0, 5_000);   // 5 second gap
        // r = ln(100100/100000), dt_s = 5.0
        // r_sq_per_sec = r^2 / 5.0
        let r = (100_100.0_f64 / 100_000.0).ln();
        let expected_sq = (1.0 - 0.94) * (r * r / 5.0);
        assert!((vol.sigma_sq - expected_sq).abs() < 1e-15, "sigma_sq = {}, expected = {}", vol.sigma_sq, expected_sq);
    }

    /// Scenario: SampledEwmaVol seeded, then updated at 1s, 0.5s (sub-second, skipped), and 2s.
    /// Expected: n_samples increments only on updates >= 1s apart; sub-second tick does not count.
    #[test]
    fn test_sampled_ewma_n_samples_accessor() {
        let mut vol = SampledEwmaVol::new(0.94, 5);
        assert_eq!(vol.n_samples(), 0);
        vol.update(100.0, 0); // seed
        assert_eq!(vol.n_samples(), 0);
        vol.update(101.0, 1000); // sample 1
        assert_eq!(vol.n_samples(), 1);
        vol.update(102.0, 500); // < 1000ms since last sample → skipped
        assert_eq!(vol.n_samples(), 1);
        vol.update(103.0, 2000); // sample 2
        assert_eq!(vol.n_samples(), 2);
    }

    /// Scenario: Two SampledEwmaVol instances fed calm (+/-$10) vs wild (+/-$500) price oscillations.
    /// Expected: The wild tracker produces a higher sigma than the calm tracker, confirming monotonicity.
    #[test]
    fn test_sampled_ewma_sigma_increases_with_vol() {
        // Higher price swings → higher sigma
        let mut vol_calm = SampledEwmaVol::new(0.94, 1);
        let mut vol_wild = SampledEwmaVol::new(0.94, 1);

        for i in 0..20 {
            let ts = i * 1000;
            let calm_price = 100_000.0 + (i % 2) as f64 * 10.0;  // ±$10
            let wild_price = 100_000.0 + (i % 2) as f64 * 500.0; // ±$500
            vol_calm.update(calm_price, ts);
            vol_wild.update(wild_price, ts);
        }

        assert!(vol_wild.sigma() > vol_calm.sigma(),
            "Wild sigma ({}) should exceed calm sigma ({})", vol_wild.sigma(), vol_calm.sigma());
    }
}
