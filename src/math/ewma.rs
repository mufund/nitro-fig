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

    #[test]
    fn test_ewma_basic() {
        let mut vol = EwmaVol::new(0.94, 5);
        // Feed constant price → sigma should be ~0
        for _ in 0..10 {
            vol.update(100.0);
        }
        assert!(vol.sigma() < 1e-10, "Constant price sigma = {}", vol.sigma());
    }

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
}
