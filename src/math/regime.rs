use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Regime {
    Range,
    Trend,
    Ambiguous,
}

/// Tick direction tracker over a rolling window.
/// Classifies based on percentage of ticks moving in the dominant direction.
///   Range: < 60% same direction
///   Trend: >= 75% same direction
///   Ambiguous: 60-75%
pub struct RegimeClassifier {
    window_ms: i64,
    ticks: VecDeque<(i64, bool)>, // (ts_ms, is_up_tick)
    up_count: u32,
    total: u32,
}

impl RegimeClassifier {
    pub fn new(window_ms: i64) -> Self {
        Self {
            window_ms,
            ticks: VecDeque::with_capacity(2000),
            up_count: 0,
            total: 0,
        }
    }

    /// Record a tick direction. Evicts stale ticks.
    #[inline]
    pub fn update(&mut self, ts_ms: i64, is_up: bool) {
        self.ticks.push_back((ts_ms, is_up));
        if is_up {
            self.up_count += 1;
        }
        self.total += 1;

        let cutoff = ts_ms - self.window_ms;
        while self.ticks.front().map_or(false, |(t, _)| *t < cutoff) {
            if let Some((_, was_up)) = self.ticks.pop_front() {
                if was_up {
                    self.up_count -= 1;
                }
                self.total -= 1;
            }
        }
    }

    /// Classify current regime.
    #[inline]
    pub fn classify(&self) -> Regime {
        if self.total < 10 {
            return Regime::Ambiguous;
        }
        let dominant = self.up_count.max(self.total - self.up_count);
        let frac = dominant as f64 / self.total as f64;
        if frac >= 0.75 {
            Regime::Trend
        } else if frac < 0.60 {
            Regime::Range
        } else {
            Regime::Ambiguous
        }
    }

    /// Direction of dominant tick flow. Only meaningful when Trend.
    #[inline]
    pub fn trend_direction_up(&self) -> bool {
        self.up_count > self.total / 2
    }

    /// Fraction of ticks in the dominant direction.
    #[inline]
    pub fn dominant_frac(&self) -> f64 {
        if self.total == 0 { return 0.0; }
        let dominant = self.up_count.max(self.total - self.up_count);
        dominant as f64 / self.total as f64
    }

    /// Number of ticks in the window.
    #[inline]
    pub fn total_ticks(&self) -> u32 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scenario: 100 ticks with a 50/50 up/down split over a 30s window.
    /// Expected: Range classification because 50% dominant is well below the 60% threshold.
    #[test]
    fn test_range() {
        let mut rc = RegimeClassifier::new(30_000);
        // 50/50 split → Range
        for i in 0..100 {
            rc.update(i * 100, i % 2 == 0);
        }
        assert_eq!(rc.classify(), Regime::Range);
    }

    /// Scenario: 100 ticks where 80% are up-ticks (every 5th tick is down).
    /// Expected: Trend classification (80% >= 75% threshold) with upward direction.
    #[test]
    fn test_trend() {
        let mut rc = RegimeClassifier::new(30_000);
        // 80% up → Trend
        for i in 0..100 {
            rc.update(i * 100, i % 5 != 0); // 80% up
        }
        assert_eq!(rc.classify(), Regime::Trend);
        assert!(rc.trend_direction_up());
    }

    /// Scenario: 100 ticks where ~67% are up-ticks (every 3rd tick is down).
    /// Expected: Ambiguous classification because 67% falls between 60% and 75%.
    #[test]
    fn test_ambiguous() {
        let mut rc = RegimeClassifier::new(30_000);
        // ~65% up → Ambiguous
        for i in 0..100 {
            rc.update(i * 100, i % 3 != 0); // ~67% up
        }
        assert_eq!(rc.classify(), Regime::Ambiguous);
    }

    /// Scenario: Only 5 ticks recorded, all up-ticks.
    /// Expected: Ambiguous because fewer than 10 ticks, regardless of distribution.
    #[test]
    fn test_insufficient_data() {
        let mut rc = RegimeClassifier::new(30_000);
        for i in 0..5 {
            rc.update(i * 100, true);
        }
        assert_eq!(rc.classify(), Regime::Ambiguous);
    }

    // ── Boundary tests ──

    /// Scenario: Exactly 10 ticks with a 5/5 up/down split (minimum tick threshold).
    /// Expected: Range classification; 10 ticks meets the minimum and 50% dominant < 60%.
    #[test]
    fn test_exactly_10_ticks_range() {
        // Minimum tick count for non-Ambiguous classification
        let mut rc = RegimeClassifier::new(30_000);
        // 5 up, 5 down → 50% dominant → Range (< 60%)
        for i in 0..10 {
            rc.update(i * 100, i < 5);
        }
        assert_eq!(rc.total_ticks(), 10);
        assert_eq!(rc.classify(), Regime::Range);
    }

    /// Scenario: 9 ticks all in the up direction (100% dominant but below minimum count).
    /// Expected: Ambiguous because 9 < 10 tick minimum, even with perfect directional agreement.
    #[test]
    fn test_exactly_9_ticks_ambiguous() {
        // 9 ticks < 10 → always Ambiguous regardless of distribution
        let mut rc = RegimeClassifier::new(30_000);
        for i in 0..9 {
            rc.update(i * 100, true); // 100% up, but < 10 ticks
        }
        assert_eq!(rc.total_ticks(), 9);
        assert_eq!(rc.classify(), Regime::Ambiguous);
    }

    /// Scenario: 100 ticks with exactly 60 up / 40 down (60% dominant).
    /// Expected: Ambiguous because 60% is at the lower Ambiguous boundary (>= 60%, < 75%).
    #[test]
    fn test_boundary_60_percent() {
        // Exactly 60% dominant → should be Ambiguous (>= 60%, < 75%)
        let mut rc = RegimeClassifier::new(100_000);
        // 60 up, 40 down → 60% dominant
        for i in 0..100 {
            rc.update(i * 100, i < 60);
        }
        let frac = rc.dominant_frac();
        assert!((frac - 0.60).abs() < 0.01, "dominant_frac = {}", frac);
        assert_eq!(rc.classify(), Regime::Ambiguous);
    }

    /// Scenario: 100 ticks with 59 up / 41 down (59% dominant).
    /// Expected: Range classification because 59% is strictly below the 60% threshold.
    #[test]
    fn test_boundary_just_below_60() {
        // 59% dominant → Range
        let mut rc = RegimeClassifier::new(100_000);
        for i in 0..100 {
            rc.update(i * 100, i < 59);
        }
        assert_eq!(rc.classify(), Regime::Range);
    }

    /// Scenario: 100 ticks with exactly 75 up / 25 down (75% dominant).
    /// Expected: Trend classification because 75% meets the >= 75% threshold exactly.
    #[test]
    fn test_boundary_75_percent() {
        // Exactly 75% dominant → Trend (>= 75%)
        let mut rc = RegimeClassifier::new(100_000);
        for i in 0..100 {
            rc.update(i * 100, i < 75);
        }
        let frac = rc.dominant_frac();
        assert!((frac - 0.75).abs() < 0.01, "dominant_frac = {}", frac);
        assert_eq!(rc.classify(), Regime::Trend);
    }

    /// Scenario: 100 ticks with 74 up / 26 down (74% dominant).
    /// Expected: Ambiguous because 74% is just below the 75% Trend threshold.
    #[test]
    fn test_boundary_just_below_75() {
        // 74% dominant → Ambiguous (not Trend)
        let mut rc = RegimeClassifier::new(100_000);
        for i in 0..100 {
            rc.update(i * 100, i < 74);
        }
        assert_eq!(rc.classify(), Regime::Ambiguous);
    }

    // ── Downward trend ──

    /// Scenario: 100 ticks where only 20% are up-ticks (80% down).
    /// Expected: Trend classification (80% dominant) with downward direction.
    #[test]
    fn test_downward_trend() {
        let mut rc = RegimeClassifier::new(30_000);
        // 80% down ticks → Trend, but trend_direction_up() = false
        for i in 0..100 {
            rc.update(i * 100, i % 5 == 0); // only 20% up
        }
        assert_eq!(rc.classify(), Regime::Trend);
        assert!(!rc.trend_direction_up());
    }

    // ── Eviction tests ──

    /// Scenario: 50 range ticks (50/50) at ts 0..4900, then 20 all-up ticks at ts 10000+ with a 5s window.
    /// Expected: Regime shifts from Range to Trend after eviction removes all phase-1 ticks.
    #[test]
    fn test_eviction_changes_regime() {
        let mut rc = RegimeClassifier::new(5_000); // 5s window
        // Phase 1: 50 range ticks (50/50), ts=0..4900
        for i in 0..50 {
            rc.update(i * 100, i % 2 == 0);
        }
        assert_eq!(rc.classify(), Regime::Range);

        // Phase 2: 20 trend ticks (all up), ts=10000..11900
        // Pushes cutoff to 11900-5000=6900, evicting ALL phase 1 ticks (max ts=4900)
        for i in 0..20 {
            let ts = 10_000 + i * 100;
            rc.update(ts, true);
        }
        // After full eviction of range ticks, only 20 up-ticks remain → 100% → Trend
        assert_eq!(rc.classify(), Regime::Trend);
        assert!(rc.trend_direction_up());
    }

    /// Scenario: 50 up-ticks in a 5s window, then one tick far in the future (ts=100000).
    /// Expected: All 50 prior ticks evicted, leaving total_ticks == 1.
    #[test]
    fn test_total_ticks_after_eviction() {
        let mut rc = RegimeClassifier::new(5_000);
        for i in 0..50 {
            rc.update(i * 100, true); // ts=0..4900
        }
        assert_eq!(rc.total_ticks(), 50);

        // Add a tick far in the future — evicts everything
        rc.update(100_000, false);
        assert_eq!(rc.total_ticks(), 1);
    }

    // ── dominant_frac edge cases ──

    /// Scenario: Freshly constructed classifier with no ticks recorded.
    /// Expected: dominant_frac returns 0.0 to avoid division by zero.
    #[test]
    fn test_dominant_frac_empty() {
        let rc = RegimeClassifier::new(30_000);
        assert_eq!(rc.dominant_frac(), 0.0);
    }

    /// Scenario: 20 ticks all in the up direction.
    /// Expected: dominant_frac returns 1.0 (100% directional agreement).
    #[test]
    fn test_dominant_frac_all_up() {
        let mut rc = RegimeClassifier::new(30_000);
        for i in 0..20 {
            rc.update(i * 100, true);
        }
        assert_eq!(rc.dominant_frac(), 1.0);
    }

    /// Scenario: 20 ticks all in the down direction.
    /// Expected: dominant_frac returns 1.0; dominant direction is symmetric for up vs down.
    #[test]
    fn test_dominant_frac_all_down() {
        let mut rc = RegimeClassifier::new(30_000);
        for i in 0..20 {
            rc.update(i * 100, false);
        }
        assert_eq!(rc.dominant_frac(), 1.0);
    }

    // ── trend_direction_up correctness ──

    /// Scenario: 20 ticks with exactly 10 up and 10 down (50/50 split).
    /// Expected: trend_direction_up returns false because up_count == total/2 is not strictly greater.
    #[test]
    fn test_trend_direction_with_even_split() {
        // When exactly half up, half down: up_count == total/2 → not up
        let mut rc = RegimeClassifier::new(30_000);
        for i in 0..20 {
            rc.update(i * 100, i < 10);
        }
        // 10 up out of 20 → up_count(10) == total/2(10) → trend_direction_up = false
        assert!(!rc.trend_direction_up());
    }
}
