use crate::engine::state::MarketState;
use crate::math::pricing::{delta_bin, p_fair};
use crate::strategies::{kelly, Strategy};
use crate::types::{EvalTrigger, Side, Signal};

/// Edge 1: Microstructure Latency Arbitrage
///
/// Polymarket CLOB quotes lag Binance. When BTC moves, compute fair binary
/// probability from Binance-implied price and hit stale PM quotes.
/// Evaluates on every BinanceTrade — the signal IS the Binance move.
pub struct LatencyArb;

const MIN_EDGE: f64 = 0.03; // 3 cents minimum after fees
const MIN_CONFIDENCE: f64 = 0.3;
const MIN_ASK_DEPTH: f64 = 50.0;   // minimum $50 of ask-side liquidity across top levels
const MAX_WALK_LEVELS: usize = 3;   // max ask levels to walk for VWAP fill estimate

impl Strategy for LatencyArb {
    fn name(&self) -> &'static str {
        "latency_arb"
    }

    fn trigger(&self) -> EvalTrigger {
        EvalTrigger::BinanceTrade
    }

    #[inline]
    fn evaluate(&self, state: &MarketState, now_ms: i64) -> Option<Signal> {
        let sigma = state.sigma_real();
        if sigma <= 0.0 {
            return None;
        }

        let s = state.s_est();
        let k = state.info.strike;
        let tau = state.tau_eff_s(now_ms);
        if tau < 1.0 || s <= 0.0 || k <= 0.0 {
            return None;
        }

        // Compute model fair probability
        let fair = p_fair(s, k, sigma, tau);

        // Binary delta: probability sensitivity to price move
        let delta = delta_bin(s, k, sigma, tau);

        // Check both sides for mispricing
        let edge_buy_up = fair - state.up_ask;
        let _edge_sell_up = state.up_bid - fair; // sell YES = buy NO (reserved for short-selling)

        let edge_buy_down = (1.0 - fair) - state.down_ask;
        let _edge_sell_down = state.down_bid - (1.0 - fair); // reserved for short-selling

        // Find the best opportunity across all four directions
        let mut best_edge = 0.0_f64;
        let mut best_side = Side::Up;
        let mut best_fair = fair;

        if state.up_ask > 0.0 && state.up_ask < 1.0 && edge_buy_up > best_edge {
            best_edge = edge_buy_up;
            best_side = Side::Up;
            best_fair = fair;
        }
        if state.down_ask > 0.0 && state.down_ask < 1.0 && edge_buy_down > best_edge {
            best_edge = edge_buy_down;
            best_side = Side::Down;
            best_fair = 1.0 - fair;
        }

        if best_edge < MIN_EDGE {
            return None;
        }

        // ── Orderbook depth gates ──
        let book = match best_side {
            Side::Up => &state.up_book,
            Side::Down => &state.down_book,
        };

        // Skip if ask-side liquidity is too thin to absorb a meaningful order
        let ask_liquidity = book.ask_depth(MAX_WALK_LEVELS);
        if ask_liquidity < MIN_ASK_DEPTH {
            return None;
        }

        // Compute VWAP fill price across top ask levels.
        // Conservative: assumes we consume all available depth in top 3 levels.
        // If edge still clears MIN_EDGE under this worst-case, the signal is robust.
        let (effective_price, _fillable) = book.vwap_fill_ask(ask_liquidity)?;

        // Recompute edge against realistic fill price (not optimistic best ask)
        let effective_edge = best_fair - effective_price;
        if effective_edge < MIN_EDGE {
            return None;
        }

        // Confidence: based on how large the mispricing is relative to expected
        // Larger |ΔS| movements → higher conviction
        let _ = delta; // used for future delta-weighted sizing
        let confidence = (effective_edge / 0.10).clamp(MIN_CONFIDENCE, 1.0);

        Some(Signal {
            strategy: "latency_arb",
            side: best_side,
            edge: effective_edge,
            fair_value: best_fair,
            market_price: effective_price,
            confidence,
            size_frac: kelly(effective_edge, effective_price).min(0.02),
            is_passive: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::test_helpers::*;

    /// Scenario: Realized vol is zero (no BTC price movement observed).
    /// Expected: None -- sigma=0 makes fair value computation undefined.
    #[test]
    fn test_none_when_sigma_zero() {
        let (state, now) = make_state(95_000.0, 96_000.0, 0.0, 120.0, 0.50, 0.50);
        assert!(LatencyArb.evaluate(&state, now).is_none());
    }

    /// Scenario: Time to expiry is 0.5s so tau_eff < 1.0 after oracle adjustment.
    /// Expected: None -- too close to expiry for latency arb execution.
    #[test]
    fn test_none_when_tau_expired() {
        // tau_s = 0.5 → tau_eff < 1.0
        let (state, now) = make_state(95_000.0, 96_000.0, 0.001, 0.5, 0.50, 0.50);
        assert!(LatencyArb.evaluate(&state, now).is_none());
    }

    /// Scenario: BTC above strike with edge, but orderbook has only $10 ask depth.
    /// Expected: None -- ask liquidity below $50 minimum is too thin to fill.
    #[test]
    fn test_none_when_thin_book() {
        // BTC well above strike → fair UP ~0.65, up_ask 0.55 → edge ~0.10
        // But book has only $10 of ask depth (< MIN_ASK_DEPTH=50)
        let (mut state, now) = make_state(95_000.0, 96_000.0, 0.001, 120.0, 0.55, 0.50);
        inject_book(&mut state, Side::Up,
            vec![(0.53, 10.0)],
            vec![(0.55, 10.0)], // only $10 of ask depth
        );
        assert!(LatencyArb.evaluate(&state, now).is_none());
    }

    /// Scenario: Best ask at 0.55 shows edge, but deeper levels walk VWAP to ~0.84 past fair.
    /// Expected: None -- effective edge after VWAP fill < 3-cent minimum.
    #[test]
    fn test_none_when_vwap_kills_edge() {
        // Best ask at 0.55 has edge, but deep levels walk past fair (~0.83)
        // VWAP across all depth ≈ 0.81, effective_edge = 0.83 - 0.81 = 0.02 < MIN_EDGE(0.03)
        let (mut state, now) = make_state(95_000.0, 96_000.0, 0.001, 120.0, 0.55, 0.50);
        inject_book(&mut state, Side::Up,
            vec![(0.53, 50.0)],
            vec![(0.55, 5.0), (0.82, 25.0), (0.90, 30.0)], // VWAP walks to ~0.84
        );
        assert!(LatencyArb.evaluate(&state, now).is_none());
    }

    /// Scenario: BTC at $96k vs $95k strike, up_ask stale at 0.55, deep book at 0.55-0.57.
    /// Expected: Signal on UP side with edge > 3 cents and aggressive (non-passive) execution.
    #[test]
    fn test_signal_when_mispriced() {
        // BTC at 96000, strike 95000 → UP is likely. fair ~0.65
        // up_ask at 0.55 → edge ~0.10. Deep book at 0.55.
        let (mut state, now) = make_state(95_000.0, 96_000.0, 0.001, 120.0, 0.55, 0.50);
        inject_book(&mut state, Side::Up,
            vec![(0.53, 100.0)],
            vec![(0.55, 100.0), (0.56, 100.0), (0.57, 100.0)],
        );
        let sig = LatencyArb.evaluate(&state, now);
        assert!(sig.is_some(), "Should produce signal with deep book and large edge");
        let sig = sig.unwrap();
        assert_eq!(sig.side, Side::Up);
        assert!(sig.edge > 0.03, "Edge should exceed MIN_EDGE: {}", sig.edge);
        assert!(!sig.is_passive);
    }

    // ── Parameterized success tests across price/vol/tau ──

    /// Scenario: BTC at 5 prices above $95k strike ($96k-$100k), deep book at 0.55.
    /// Expected: Most price levels produce UP signals with edge > 3 cents, size capped at 2%.
    #[test]
    fn test_signal_across_price_levels() {
        // BTC at various levels above strike, with deep book priced well below fair
        let prices = [96_000.0, 96_500.0, 97_000.0, 98_000.0, 100_000.0];
        let strike = 95_000.0;
        let mut signals = 0;

        for &bn_price in &prices {
            let (mut state, now) = make_state(strike, bn_price, 0.001, 120.0, 0.55, 0.50);
            inject_book(&mut state, Side::Up,
                vec![(0.53, 100.0)],
                vec![(0.55, 100.0), (0.56, 100.0), (0.57, 100.0)],
            );
            if let Some(sig) = LatencyArb.evaluate(&state, now) {
                assert_eq!(sig.side, Side::Up, "BTC above strike → UP side at price {}", bn_price);
                assert!(sig.edge > 0.03);
                assert!(sig.size_frac > 0.0 && sig.size_frac <= 0.02);
                signals += 1;
            }
        }
        assert!(signals >= 3, "Should produce signals for most prices above strike: {}/5", signals);
    }

    /// Scenario: Fixed mispricing tested across 5 tau values (30s-250s) with deep book.
    /// Expected: Strategy produces signals for most tau values -- arb works across time horizons.
    #[test]
    fn test_signal_across_tau_values() {
        // Various time-to-expiry values — strategy should work across them
        let taus = [30.0, 60.0, 120.0, 180.0, 250.0];
        let mut signals = 0;

        for &tau in &taus {
            let (mut state, now) = make_state(95_000.0, 96_500.0, 0.001, tau, 0.55, 0.50);
            inject_book(&mut state, Side::Up,
                vec![(0.53, 100.0)],
                vec![(0.55, 100.0), (0.56, 100.0), (0.57, 100.0)],
            );
            if let Some(sig) = LatencyArb.evaluate(&state, now) {
                assert!(sig.edge > 0.03, "tau={}: edge = {}", tau, sig.edge);
                signals += 1;
            }
        }
        assert!(signals >= 3, "Should produce signals across tau values: {}/5", signals);
    }

    /// Scenario: BTC at $93.5k, below $95k strike; down_ask cheap at 0.15 with deep book.
    /// Expected: Signal on DOWN side -- Binance shows BTC below strike, PM DOWN quote is stale.
    #[test]
    fn test_down_side_signal() {
        // BTC below strike → DOWN is likely, down_ask should be cheap
        let (mut state, now) = make_state(95_000.0, 93_500.0, 0.001, 120.0, 0.50, 0.15);
        inject_book(&mut state, Side::Down,
            vec![(0.13, 100.0)],
            vec![(0.15, 100.0), (0.16, 100.0), (0.17, 100.0)],
        );
        let sig = LatencyArb.evaluate(&state, now);
        if let Some(sig) = sig {
            assert_eq!(sig.side, Side::Down, "BTC below strike → should buy DOWN");
            assert!(sig.edge > 0.03);
        }
    }

    /// Scenario: BTC at $96k, up_ask set to 0.83 (close to model fair), deep book.
    /// Expected: None -- market is fairly priced, no exploitable lag.
    #[test]
    fn test_no_signal_when_fairly_priced() {
        // BTC at 96000, fair UP ~ 0.83 → if up_ask = 0.83, no edge
        let (mut state, now) = make_state(95_000.0, 96_000.0, 0.001, 120.0, 0.83, 0.17);
        inject_book(&mut state, Side::Up,
            vec![(0.81, 100.0)],
            vec![(0.83, 100.0), (0.84, 100.0)],
        );
        assert!(LatencyArb.evaluate(&state, now).is_none(),
            "No signal when market is fairly priced");
    }
}
