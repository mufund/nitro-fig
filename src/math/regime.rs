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

    #[test]
    fn test_range() {
        let mut rc = RegimeClassifier::new(30_000);
        // 50/50 split → Range
        for i in 0..100 {
            rc.update(i * 100, i % 2 == 0);
        }
        assert_eq!(rc.classify(), Regime::Range);
    }

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

    #[test]
    fn test_ambiguous() {
        let mut rc = RegimeClassifier::new(30_000);
        // ~65% up → Ambiguous
        for i in 0..100 {
            rc.update(i * 100, i % 3 != 0); // ~67% up
        }
        assert_eq!(rc.classify(), Regime::Ambiguous);
    }

    #[test]
    fn test_insufficient_data() {
        let mut rc = RegimeClassifier::new(30_000);
        for i in 0..5 {
            rc.update(i * 100, true);
        }
        assert_eq!(rc.classify(), Regime::Ambiguous);
    }
}
