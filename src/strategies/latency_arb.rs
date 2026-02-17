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
        let mut best_price = 0.0_f64;
        let mut best_fair = fair;

        if state.up_ask > 0.0 && state.up_ask < 1.0 && edge_buy_up > best_edge {
            best_edge = edge_buy_up;
            best_side = Side::Up;
            best_price = state.up_ask;
            best_fair = fair;
        }
        if state.down_ask > 0.0 && state.down_ask < 1.0 && edge_buy_down > best_edge {
            best_edge = edge_buy_down;
            best_side = Side::Down;
            best_price = state.down_ask;
            best_fair = 1.0 - fair;
        }

        if best_edge < MIN_EDGE {
            return None;
        }

        // Confidence: based on how large the mispricing is relative to expected
        // Larger |ΔS| movements → higher conviction
        let _ = delta; // used for future delta-weighted sizing
        let confidence = (best_edge / 0.10).clamp(MIN_CONFIDENCE, 1.0);

        Some(Signal {
            strategy: "latency_arb",
            side: best_side,
            edge: best_edge,
            fair_value: best_fair,
            market_price: best_price,
            confidence,
            size_frac: kelly(best_edge, best_price).min(0.02),
            is_passive: false,
        })
    }
}
