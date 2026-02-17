use std::collections::VecDeque;

/// Rolling VWAP tracker over a configurable time window.
/// O(1) amortized per update via incremental sum maintenance.
#[derive(Clone)]
pub struct VwapTracker {
    window_ms: i64,
    buffer: VecDeque<(i64, f64, f64)>, // (ts_ms, price, qty)
    sum_pq: f64,
    sum_q: f64,
}

impl VwapTracker {
    pub fn new(window_ms: i64) -> Self {
        Self {
            window_ms,
            buffer: VecDeque::with_capacity(5000),
            sum_pq: 0.0,
            sum_q: 0.0,
        }
    }

    /// Add a trade. Evicts stale entries from the window.
    #[inline]
    pub fn update(&mut self, ts_ms: i64, price: f64, qty: f64) {
        self.buffer.push_back((ts_ms, price, qty));
        self.sum_pq += price * qty;
        self.sum_q += qty;
        let cutoff = ts_ms - self.window_ms;
        while self
            .buffer
            .front()
            .map_or(false, |(t, _, _)| *t < cutoff)
        {
            if let Some((_, p, q)) = self.buffer.pop_front() {
                self.sum_pq -= p * q;
                self.sum_q -= q;
            }
        }
    }

    /// Current VWAP. Returns 0.0 if no data.
    #[inline]
    pub fn vwap(&self) -> f64 {
        if self.sum_q > 0.0 {
            self.sum_pq / self.sum_q
        } else {
            0.0
        }
    }

    /// Whether we have any data in the window.
    #[inline]
    pub fn has_data(&self) -> bool {
        self.sum_q > 0.0
    }

    /// Number of trades in the window.
    #[inline]
    pub fn len(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: Two trades with equal quantity (1.0 each) at prices 100.0 and 102.0.
    /// Expected: VWAP = (100*1 + 102*1) / (1+1) = 101.0 (simple average when quantities are equal).
    #[test]
    fn test_vwap_basic() {
        let mut v = VwapTracker::new(10_000);
        v.update(1000, 100.0, 1.0);
        v.update(2000, 102.0, 1.0);
        // VWAP = (100 + 102) / 2 = 101
        assert!((v.vwap() - 101.0).abs() < 1e-10);
    }

    /// Scenario: Two trades with unequal quantities: 3.0 units at 100.0 and 1.0 unit at 106.0.
    /// Expected: VWAP = (300 + 106) / 4 = 101.5, skewed toward the larger trade.
    #[test]
    fn test_vwap_weighted() {
        let mut v = VwapTracker::new(10_000);
        v.update(1000, 100.0, 3.0);
        v.update(2000, 106.0, 1.0);
        // VWAP = (300 + 106) / 4 = 101.5
        assert!((v.vwap() - 101.5).abs() < 1e-10);
    }

    /// Scenario: Three trades in a 5-second window; the third arrives at t=7s, evicting the first (t=1s).
    /// Expected: VWAP > 110 because the stale $100 trade is evicted, leaving only $110 and $120 trades.
    #[test]
    fn test_vwap_eviction() {
        let mut v = VwapTracker::new(5000);
        v.update(1000, 100.0, 1.0);
        v.update(2000, 110.0, 1.0);
        v.update(7000, 120.0, 1.0);
        // First trade (ts=1000) should be evicted (cutoff = 7000 - 5000 = 2000)
        // Second trade (ts=2000) is exactly at cutoff, may or may not be evicted
        // VWAP should be close to (110 + 120) / 2 or just 120
        assert!(v.vwap() > 110.0);
    }

    /// Scenario: Freshly constructed VwapTracker with no trades added.
    /// Expected: vwap() returns 0.0 and has_data() returns false.
    #[test]
    fn test_vwap_empty() {
        let v = VwapTracker::new(5000);
        assert_eq!(v.vwap(), 0.0);
        assert!(!v.has_data());
    }

    // ── len() tests ──

    /// Scenario: Freshly constructed VwapTracker with no trades.
    /// Expected: len() returns 0.
    #[test]
    fn test_len_empty() {
        let v = VwapTracker::new(5000);
        assert_eq!(v.len(), 0);
    }

    /// Scenario: Three trades added within the 10-second window, no evictions triggered.
    /// Expected: len() returns 3, reflecting all trades still in the buffer.
    #[test]
    fn test_len_after_updates() {
        let mut v = VwapTracker::new(10_000);
        v.update(1000, 100.0, 1.0);
        v.update(2000, 101.0, 2.0);
        v.update(3000, 102.0, 0.5);
        assert_eq!(v.len(), 3);
    }

    /// Scenario: Three trades in a 5-second window, then a fourth at t=7s that evicts the first (t=1s < cutoff=2s).
    /// Expected: len() drops from 3 to 3 again (one evicted, one added), containing ts=2000, 3000, 7000.
    #[test]
    fn test_len_after_eviction() {
        let mut v = VwapTracker::new(5000);
        v.update(1000, 100.0, 1.0);
        v.update(2000, 101.0, 1.0);
        v.update(3000, 102.0, 1.0);
        assert_eq!(v.len(), 3);

        // ts=7000 → cutoff=2000 → evicts ts=1000 (< 2000)
        v.update(7000, 103.0, 1.0);
        assert_eq!(v.len(), 3); // ts=2000, 3000, 7000
    }

    // ── has_data after eviction ──

    /// Scenario: One trade added, then a second trade far in the future (t=100s) evicts the first.
    /// Expected: has_data() remains true (new trade is in window), len()=1, vwap equals the new trade's price.
    #[test]
    fn test_has_data_after_full_eviction() {
        let mut v = VwapTracker::new(5000);
        v.update(1000, 100.0, 1.0);
        assert!(v.has_data());

        // Single trade far in the future — old trade evicted, new one remains
        v.update(100_000, 200.0, 1.0);
        assert!(v.has_data());
        assert_eq!(v.len(), 1);
        assert!((v.vwap() - 200.0).abs() < 1e-10);
    }

    // ── Precise eviction boundary ──

    /// Scenario: Two trades at t=1s and t=2s; a third at t=6s (cutoff=1000, exact match) and a fourth at t=6.001s (cutoff=1001).
    /// Expected: Strict-less-than eviction keeps t=1000 when cutoff=1000, but evicts it when cutoff=1001.
    #[test]
    fn test_eviction_exact_boundary() {
        let mut v = VwapTracker::new(5000);
        v.update(1000, 100.0, 1.0); // ts=1000
        v.update(2000, 110.0, 1.0); // ts=2000

        // cutoff = 6000 - 5000 = 1000. Eviction is `*t < cutoff` so ts=1000 is NOT evicted
        v.update(6000, 120.0, 1.0);
        // ts=1000 is at cutoff exactly → kept (< is strict)
        assert_eq!(v.len(), 3);

        // Now cutoff = 6001 - 5000 = 1001 → ts=1000 IS evicted
        v.update(6001, 121.0, 1.0);
        assert_eq!(v.len(), 3); // ts=2000, 6000, 6001
    }

    // ── Zero quantity ──

    /// Scenario: A single trade with qty=0.0 added to the tracker.
    /// Expected: sum_q remains 0, so has_data() is false and vwap() returns 0.0 (no meaningful data).
    #[test]
    fn test_zero_quantity_trade() {
        let mut v = VwapTracker::new(10_000);
        v.update(1000, 100.0, 0.0);
        // Zero quantity → sum_q = 0 → no data
        assert!(!v.has_data());
        assert_eq!(v.vwap(), 0.0);
    }

    // ── Large dataset ──

    /// Scenario: 1000 trades all at the same price (95,000) with equal quantity, within the window.
    /// Expected: VWAP equals 95,000 exactly, verifying numerical stability with many identical-price trades.
    #[test]
    fn test_vwap_many_trades() {
        let mut v = VwapTracker::new(10_000);
        // 1000 trades at the same price → VWAP should equal that price
        for i in 0..1000 {
            v.update(i * 10, 95_000.0, 0.1);
        }
        assert!((v.vwap() - 95_000.0).abs() < 1e-6);
    }

    /// Scenario: 100 trades with ascending prices 100..199, all with qty=1.0, within the window.
    /// Expected: VWAP = arithmetic mean of 100..199 = 149.5, since equal quantities reduce VWAP to a simple average.
    #[test]
    fn test_vwap_ascending_prices() {
        let mut v = VwapTracker::new(100_000);
        // Ascending prices with equal qty → VWAP = average price
        for i in 0..100 {
            v.update(i * 100, 100.0 + i as f64, 1.0);
        }
        // Average of 100..199 = 149.5
        assert!((v.vwap() - 149.5).abs() < 1e-10);
    }
}
