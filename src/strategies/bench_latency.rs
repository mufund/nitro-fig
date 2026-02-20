// End-to-end calculation latency benchmarks for each strategy.
// Measures wall-clock time for evaluate() calls in hot loops.
// Only compiled under #[cfg(test)].

use std::time::Instant;

use crate::strategies::test_helpers::*;
use crate::strategies::{evaluate_filtered, Strategy};
use crate::strategies::latency_arb::LatencyArb;
use crate::strategies::certainty_capture::CertaintyCapture;
use crate::strategies::convexity_fade::ConvexityFade;
use crate::strategies::strike_misalign::StrikeMisalign;
use crate::strategies::lp_extreme::LpExtreme;
use crate::engine::risk::StrategyRiskManager;
use crate::types::{Side, Signal};

const ITERATIONS: u32 = 1000;
/// Maximum allowed time for 1000 evaluate() calls (10ms = 10μs per call).
/// This is very conservative — actual calls should be <1μs.
const MAX_TOTAL_US: u128 = 10_000;

/// Scenario: LatencyArb.evaluate() called 1000 times with a realistic state
///   including orderbook data, valid sigma, and sufficient tau.
/// Expected: Total time under 10ms (< 10μs per call), confirming zero-alloc fast path.
#[test]
fn test_latency_latency_arb() {
    let strat = LatencyArb;
    let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
    inject_book(
        &mut state,
        Side::Up,
        vec![(0.48, 200.0), (0.47, 300.0)],
        vec![(0.50, 200.0), (0.51, 300.0)],
    );

    // Warmup
    for _ in 0..10 {
        let _ = strat.evaluate(&state, now);
    }

    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let _ = strat.evaluate(&state, now);
    }
    let elapsed_us = start.elapsed().as_micros();

    eprintln!(
        "[BENCH] latency_arb: {}μs total, {:.2}μs/call ({} iters)",
        elapsed_us, elapsed_us as f64 / ITERATIONS as f64, ITERATIONS,
    );
    assert!(
        elapsed_us < MAX_TOTAL_US,
        "latency_arb too slow: {}μs for {} calls (max {}μs)",
        elapsed_us, ITERATIONS, MAX_TOTAL_US,
    );
}

/// Scenario: CertaintyCapture.evaluate() called 1000 times with BTC far above
///   strike (z > 1.5), creating a high-certainty scenario where signal is produced.
/// Expected: Total time under 10ms even when computing z-score and p_fair each call.
#[test]
fn test_latency_certainty_capture() {
    let strat = CertaintyCapture;
    let (state, now) = make_state(95_000.0, 96_000.0, 0.001, 120.0, 0.80, 0.20);

    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let _ = strat.evaluate(&state, now);
    }
    let elapsed_us = start.elapsed().as_micros();

    eprintln!(
        "[BENCH] certainty_capture: {}μs total, {:.2}μs/call ({} iters)",
        elapsed_us, elapsed_us as f64 / ITERATIONS as f64, ITERATIONS,
    );
    assert!(
        elapsed_us < MAX_TOTAL_US,
        "certainty_capture too slow: {}μs for {} calls",
        elapsed_us, ITERATIONS,
    );
}

/// Scenario: ConvexityFade.evaluate() called 1000 times in Range regime with BTC
///   near strike (ATM) where gamma is highest.
/// Expected: Total time under 10ms — regime check and p_fair computation are fast.
#[test]
fn test_latency_convexity_fade() {
    let strat = ConvexityFade;
    let (mut state, now) = make_state(95_000.0, 95_010.0, 0.001, 120.0, 0.48, 0.48);
    force_regime_range(&mut state, now);
    inject_book(
        &mut state,
        Side::Up,
        vec![(0.46, 200.0), (0.45, 200.0)],
        vec![(0.48, 200.0), (0.49, 200.0)],
    );

    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let _ = strat.evaluate(&state, now);
    }
    let elapsed_us = start.elapsed().as_micros();

    eprintln!(
        "[BENCH] convexity_fade: {}μs total, {:.2}μs/call ({} iters)",
        elapsed_us, elapsed_us as f64 / ITERATIONS as f64, ITERATIONS,
    );
    assert!(
        elapsed_us < MAX_TOTAL_US,
        "convexity_fade too slow: {}μs for {} calls",
        elapsed_us, ITERATIONS,
    );
}

/// Scenario: StrikeMisalign.evaluate() called 1000 times with VWAP data injected
///   and within the 15-second opening window.
/// Expected: Total time under 10ms — VWAP lookup and delta computation are O(1).
#[test]
fn test_latency_strike_misalign() {
    let strat = StrikeMisalign;
    let (mut state, now) = make_state(95_000.0, 94_800.0, 0.001, 120.0, 0.50, 0.50);
    for i in 0..20 {
        inject_vwap(&mut state, 94_800.0 + (i as f64) * 2.0, 1.0, now - 60_000 + i * 3000);
    }

    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let _ = strat.evaluate(&state, now);
    }
    let elapsed_us = start.elapsed().as_micros();

    eprintln!(
        "[BENCH] strike_misalign: {}μs total, {:.2}μs/call ({} iters)",
        elapsed_us, elapsed_us as f64 / ITERATIONS as f64, ITERATIONS,
    );
    assert!(
        elapsed_us < MAX_TOTAL_US,
        "strike_misalign too slow: {}μs for {} calls",
        elapsed_us, ITERATIONS,
    );
}

/// Scenario: LpExtreme.evaluate() called 1000 times with extreme z-score (BTC far
///   above strike) and losing side at low price, in Range regime.
/// Expected: Total time under 10ms — z-score computation and regime check are fast.
#[test]
fn test_latency_lp_extreme() {
    let strat = LpExtreme;
    let (mut state, now) = make_state(95_000.0, 96_000.0, 0.001, 120.0, 0.92, 0.08);
    force_regime_range(&mut state, now);
    inject_book(
        &mut state,
        Side::Down,
        vec![(0.06, 100.0)],
        vec![(0.08, 100.0)],
    );

    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let _ = strat.evaluate(&state, now);
    }
    let elapsed_us = start.elapsed().as_micros();

    eprintln!(
        "[BENCH] lp_extreme: {}μs total, {:.2}μs/call ({} iters)",
        elapsed_us, elapsed_us as f64 / ITERATIONS as f64, ITERATIONS,
    );
    assert!(
        elapsed_us < MAX_TOTAL_US,
        "lp_extreme too slow: {}μs for {} calls",
        elapsed_us, ITERATIONS,
    );
}

/// Scenario: evaluate_filtered() called 1000 times with all 5 active strategies,
///   simulating the hot-path batch evaluation in the engine event loop.
/// Expected: Total time under 50ms (< 50μs per batch of 5 strategies).
#[test]
fn test_latency_evaluate_filtered_all() {
    let latency_arb = LatencyArb;
    let certainty_capture = CertaintyCapture;
    let convexity_fade = ConvexityFade;
    let strike_misalign = StrikeMisalign;
    let lp_extreme = LpExtreme;

    let strategies: Vec<&dyn Strategy> = vec![
        &latency_arb,
        &certainty_capture,
        &convexity_fade,
        &strike_misalign,
        &lp_extreme,
    ];

    let (mut state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);
    force_regime_range(&mut state, now);
    inject_book(
        &mut state,
        Side::Up,
        vec![(0.48, 200.0), (0.47, 300.0)],
        vec![(0.50, 200.0), (0.51, 300.0)],
    );

    let mut buf = Vec::with_capacity(8);

    // Warmup
    for _ in 0..10 {
        evaluate_filtered(&strategies, &state, now, &mut buf);
    }

    let start = Instant::now();
    for _ in 0..ITERATIONS {
        evaluate_filtered(&strategies, &state, now, &mut buf);
    }
    let elapsed_us = start.elapsed().as_micros();

    eprintln!(
        "[BENCH] evaluate_filtered (5 strats): {}μs total, {:.2}μs/call ({} iters)",
        elapsed_us, elapsed_us as f64 / ITERATIONS as f64, ITERATIONS,
    );
    // 50ms budget for 1000 batches of 5 strategies = 50μs per batch
    assert!(
        elapsed_us < 50_000,
        "evaluate_filtered too slow: {}μs for {} calls (max 50000μs)",
        elapsed_us, ITERATIONS,
    );
}

/// Scenario: Risk manager check_strategy() called 1000 times with a valid signal,
///   measuring the overhead of the 10-gate risk check pipeline.
/// Expected: Total time under 10ms — risk checks are pure arithmetic comparisons.
#[test]
fn test_latency_risk_check() {
    let config = make_config();
    let mut risk = StrategyRiskManager::new(&config);
    let (state, now) = make_state(95_000.0, 95_500.0, 0.001, 120.0, 0.50, 0.50);

    let signal = Signal {
        strategy: "latency_arb",
        side: Side::Up,
        edge: 0.05,
        fair_value: 0.55,
        market_price: 0.50,
        confidence: 0.8,
        size_frac: 0.02,
        is_passive: false,
        use_bid: false,
    };

    // Warmup
    for i in 0..10u64 {
        let _ = risk.check_strategy(&signal, &state, i + 1, now);
    }

    // Reset risk state for the benchmark (clear cooldown/order counts)
    risk = StrategyRiskManager::new(&config);

    let start = Instant::now();
    for i in 0..ITERATIONS {
        let _ = risk.check_strategy(&signal, &state, (i + 100) as u64, now);
    }
    let elapsed_us = start.elapsed().as_micros();

    eprintln!(
        "[BENCH] risk_check: {}μs total, {:.2}μs/call ({} iters)",
        elapsed_us, elapsed_us as f64 / ITERATIONS as f64, ITERATIONS,
    );
    assert!(
        elapsed_us < MAX_TOTAL_US,
        "risk_check too slow: {}μs for {} calls",
        elapsed_us, ITERATIONS,
    );
}
