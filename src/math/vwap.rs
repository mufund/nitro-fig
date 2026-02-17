use std::collections::VecDeque;

/// Rolling VWAP tracker over a configurable time window.
/// O(1) amortized per update via incremental sum maintenance.
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

    #[test]
    fn test_vwap_basic() {
        let mut v = VwapTracker::new(10_000);
        v.update(1000, 100.0, 1.0);
        v.update(2000, 102.0, 1.0);
        // VWAP = (100 + 102) / 2 = 101
        assert!((v.vwap() - 101.0).abs() < 1e-10);
    }

    #[test]
    fn test_vwap_weighted() {
        let mut v = VwapTracker::new(10_000);
        v.update(1000, 100.0, 3.0);
        v.update(2000, 106.0, 1.0);
        // VWAP = (300 + 106) / 4 = 101.5
        assert!((v.vwap() - 101.5).abs() < 1e-10);
    }

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

    #[test]
    fn test_vwap_empty() {
        let v = VwapTracker::new(5000);
        assert_eq!(v.vwap(), 0.0);
        assert!(!v.has_data());
    }
}
