pub mod distance_fade;
pub mod momentum;
pub mod settlement_sniper;

use crate::engine::state::MarketState;
use crate::types::Signal;

/// Strategy trait: stateless pure function of market state.
/// Same code runs in live engine and backtester.
pub trait Strategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn evaluate(&self, state: &MarketState, now_ms: i64) -> Option<Signal>;
}

/// Evaluate all strategies, filling pre-allocated buffer.
#[inline]
pub fn evaluate_all(
    strategies: &[Box<dyn Strategy>],
    state: &MarketState,
    now_ms: i64,
    buf: &mut Vec<Signal>,
) {
    buf.clear();
    for s in strategies {
        if let Some(sig) = s.evaluate(state, now_ms) {
            buf.push(sig);
        }
    }
}

/// Select best signal: highest edge × confidence.
pub fn select_best(signals: &[Signal]) -> Option<&Signal> {
    signals.iter().max_by(|a, b| {
        let sa = a.edge * a.confidence;
        let sb = b.edge * b.confidence;
        sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Half-Kelly position sizing.
pub fn kelly(edge: f64, price: f64) -> f64 {
    if price >= 1.0 || edge <= 0.0 {
        return 0.0;
    }
    ((edge / (1.0 - price)) * 0.5).clamp(0.0, 0.15)
}

/// Time left as fraction of total window (1.0 at start, 0.0 at end).
pub fn time_left_fraction(state: &MarketState, now_ms: i64) -> f64 {
    let total = (state.info.end_ms - state.info.start_ms).max(1) as f64;
    let left = (state.info.end_ms - now_ms).max(0) as f64;
    left / total
}
