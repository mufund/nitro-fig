# Architecture

Event-driven, low-latency trading system for Polymarket crypto Up/Down binary markets. Single-owner event loop design with zero shared mutable state. Persistent Binance WebSocket connection survives across market cycles.

## Data Flow

```
┌──────────────────────────────────────────────────────────────────────────┐
│                        Feed Layer                                        │
│                                                                          │
│  ┌─────────────────────┐          ┌─────────────────────────────────┐   │
│  │  Binance WS          │          │  Polymarket CLOB WS             │   │
│  │  btcusdt@trade       │          │  best_bid_ask / book snapshots  │   │
│  │  PERSISTENT (lives   │          │  PER-MARKET (new connection     │   │
│  │  across all markets) │          │  each 5-min cycle)              │   │
│  └──────────┬───────────┘          └──────────────┬──────────────────┘   │
│             │ watch::channel                      │                      │
│             │ (swap feed_tx per market)            │                      │
│             └──────────────┬──────────────────────┘                      │
│                            ▼                                             │
│                  ┌───────────────────┐                                   │
│                  │  feed_tx → feed_rx │  mpsc(4096)                      │
│                  └─────────┬─────────┘                                   │
│                            ▼                                             │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │                 CORE ENGINE (single async task)                    │  │
│  │                                                                   │  │
│  │  MarketState {                                                    │  │
│  │    bn: BinanceState (persistent)                                  │  │
│  │    PM quotes, books, oracle, position (per-market)                │  │
│  │  }                                                                │  │
│  │                                                                   │  │
│  │  loop {                                                           │  │
│  │    BinanceTrade  → state.on_binance_trade()                       │  │
│  │                  → [latency_arb, lp_extreme].evaluate()           │  │
│  │                  → [strike_misalign].evaluate() (first 15s only)  │  │
│  │    PM Quote/Book → state.on_polymarket_quote/book()               │  │
│  │                  → [certainty_capture, convexity_fade,            │  │
│  │                     lp_extreme].evaluate()                        │  │
│  │    OrderAck      → fills.push(), position.on_fill()               │  │
│  │    Tick(100ms)   → stale data check                               │  │
│  │  }                                                                │  │
│  │  Settlement → compute PnL from fills + outcome                    │  │
│  │  Return BinanceState for next market                              │  │
│  └──────────────┬──────────────────────────┬─────────────────────────┘  │
│                 │                          │                             │
│        ┌────────┘                          └────────┐                    │
│        ▼                                            ▼                    │
│  ┌──────────────┐                  ┌────────────────────────────────┐   │
│  │ ORDER GATEWAY │                  │ TELEMETRY WRITER               │   │
│  │  order_rx(64) │                  │  telem_rx(4096)                │   │
│  │               │                  │  CSVs + Telegram alerts        │   │
│  │ Order → CLOB  │                  │  signals / orders / fills /    │   │
│  │ Ack → feed_tx │                  │  latency / market summary      │   │
│  └──────────────┘                  └────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────────────┘
```

## Persistent Binance State

The most important architectural decision: `BinanceState` is created once at startup and threaded through every market cycle:

```rust
let mut binance_state = BinanceState::new(lambda, min_samples, sigma_floor, vwap_window, regime_window);

loop {
    let market = discover_next_market().await;
    binance_state = run_engine(market, binance_state, ...).await;
}
```

`BinanceState` contains:
- **SampledEwmaVol**: 1-second sampled realized volatility (lambda=0.94, 10-sample warmup)
- **VwapTracker**: 60-second rolling volume-weighted average price
- **RegimeClassifier**: 30-second rolling tick direction tracker
- **Trade buffer**: 30-second VecDeque of raw Binance trades
- **Price state**: current/previous Binance price, timestamp
- **Cached sigma_real**: Updated once per second, zero-cost reads on hot path

**Effect**: Market 1 takes ~10 seconds to warm up EWMA (10 one-second samples). Market 2+ starts instantly with real volatility from the persistent state (ewma_n grows: 0 → 198 → 415 → 609 → ...).

## Why Single-Owner (vs Shared State)

| Concern | Shared State (RwLock) | Single-Owner Event Loop |
|---|---|---|
| Race conditions | Possible | Impossible by construction |
| Lock contention | Non-zero jitter | Zero — no locks exist |
| Debugging | Hard to reproduce | Deterministic — replay events |
| Backtesting | Must mock shared state | Same code path — feed events from CSV |
| Code complexity | `Arc<RwLock<T>>` everywhere | Plain owned struct |

## Module Map

```
src/
├── lib.rs                         # Library crate — re-exports all modules
├── main.rs                        # Market loop: discover → connect → engine → repeat
├── config.rs                      # Interval, Config from env vars, series_id lookup
├── types.rs                       # FeedEvent, BinanceTrade, Signal, Fill, Order, etc.
├── feeds/
│   ├── mod.rs
│   ├── binance.rs                 # Persistent Binance WS → FeedEvent::BinanceTrade
│   └── polymarket.rs              # Per-market CLOB WS → PolymarketQuote + PolymarketBook
├── engine/
│   ├── mod.rs
│   ├── state.rs                   # BinanceState (persistent) + MarketState (per-market)
│   ├── risk.rs                    # Two-tier risk: per-strategy + portfolio-level
│   └── runner.rs                  # Core event loop + side coherence + diagnostics
├── strategies/
│   ├── mod.rs                     # Strategy trait + evaluate_filtered + kelly()
│   ├── latency_arb.rs             # S1: Binance→PM latency exploitation
│   ├── certainty_capture.rs       # S2: z-score gated settlement convergence
│   ├── convexity_fade.rs          # S3: ATM gamma/convexity mean-reversion
│   ├── strike_misalign.rs         # S4: VWAP vs strike bias in first 15s
│   ├── lp_extreme.rs              # S5: Passive LP on losing side (tail risk)
│   └── cross_timeframe.rs         # S6: Vol surface RV (disabled — no feed)
├── math/
│   ├── mod.rs
│   ├── normal.rs                  # phi(x), Phi(x) — standard normal PDF/CDF
│   ├── pricing.rs                 # d2, p_fair, z_score, delta, gamma, vega, implied_vol
│   ├── ewma.rs                    # SampledEwmaVol (1s) + legacy EwmaVol (per-tick)
│   ├── oracle.rs                  # OracleBasis: S_est = S + beta, tau_eff = tau + delta
│   ├── vwap.rs                    # Rolling VWAP with O(1) amortized updates
│   └── regime.rs                  # RegimeClassifier: Range/Ambiguous/Trend
├── gateway/
│   ├── mod.rs
│   └── order.rs                   # Order gateway: execute → ack back via feed channel
├── telemetry/
│   ├── mod.rs
│   ├── writer.rs                  # Single writer task: CSVs + Telegram
│   └── telegram.rs                # Telegram Bot API client
├── market/
│   ├── mod.rs
│   └── discovery.rs               # Gamma API: slug + series_id market discovery
└── bin/
    ├── backtester.rs              # Replay CSVs through library strategies
    ├── recorder.rs                # Record live market feeds to CSV
    ├── analyzer.rs                # Post-hoc analysis of recorded data
    └── ws_test.rs                 # WebSocket connectivity test
```

## Market Lifecycle

Each market follows this sequence:

1. **Discover** — Compute slug `btc-updown-5m-{window_start}`, query Gamma API
2. **Wait** — Sleep until 10s before market start
3. **Set strike** — Read latest Binance price from persistent `price_rx` watch channel
4. **Create channels** — Per-market `feed_tx/rx`, `order_tx/rx`, `telem_tx/rx`
5. **Activate Binance** — Swap the Binance feed's output to this market's `feed_tx`
6. **Spawn per-market tasks** — Polymarket WS, heartbeat tick (100ms), order gateway, telemetry writer
7. **Run engine** — Process events until `market.end_ms + 10s`, returns `BinanceState`
8. **Pause Binance** — Set feed swap to `None` (trades dropped between markets)
9. **Cleanup** — Abort PM feed, tick, gateway, telemetry. Flush logs.
10. **Loop** — Discover next market, repeat

Markets auto-cycle indefinitely. The Binance WebSocket is never disconnected.

## Core Engine

`engine/runner.rs` processes events sequentially in a single async task:

**Strategy evaluation triggers:**
- `BinanceTrade` → evaluates `[latency_arb, lp_extreme]` + `[strike_misalign]` if in first 15s
- `PolymarketQuote` / `PolymarketBook` → evaluates `[certainty_capture, convexity_fade, lp_extreme]` + `[strike_misalign]` if in first 15s
- `OrderAck` → records fill, updates position
- `Tick` → stale data detection (5s threshold)

**Side coherence**: First dispatched active order sets `house_side`. Subsequent active orders must agree. Passive signals (lp_extreme) are exempt. See [STRATEGIES.md](STRATEGIES.md) for details.

**Settlement**: At market end, determines outcome from final `distance()`, iterates over all fills, computes realized PnL per fill and per strategy.

**Diagnostics**: Every 10 seconds, logs `[DIAG]` block showing z-score, regime, distance, and per-strategy gate analysis.

## Risk Management

`engine/risk.rs` — Two-tier system: per-strategy limits + portfolio-level caps.

**Per-strategy limits** (each strategy operates independently):

| Strategy | Per-trade | Total | Cooldown | Max orders |
|----------|-----------|-------|----------|------------|
| latency_arb | 2% | 8% | 200ms | 50 |
| certainty_capture | 5% | 10% | 1s | 15 |
| convexity_fade | 0.5% | 3% | 2s | 20 |
| strike_misalign | 2% | 4% | 500ms | 5 |
| lp_extreme | 2% | 5% | 2s | 10 |

**Portfolio-level gates** (checked before per-strategy):

| Gate | Default | Env Var |
|------|---------|---------|
| Max total exposure | 15% of bankroll | `MAX_EXPOSURE_FRAC` |
| Daily loss halt | -3% of bankroll | `DAILY_LOSS_HALT` |
| Weekly loss halt | -8% of bankroll | `WEEKLY_LOSS_HALT` |
| Stale feed rejection | 5s threshold | — |

**Sizing flow**: `Signal.size_frac * bankroll` → capped by per-trade limit → capped by strategy room → capped by portfolio room → minimum $1.

**PnL accounting**: Fills are tracked in `Vec<Fill>` during the market. At settlement, `settle_market(outcome, fills)` computes correct binary PnL and updates daily/weekly counters. Per-market exposure resets to zero.

## Realized Volatility Model

`SampledEwmaVol` (in `math/ewma.rs`) samples once per second instead of once per tick:

```
On each Binance trade:
  if elapsed >= 1000ms since last sample:
    dt_s = elapsed / 1000
    r = ln(price / last_sample_price)
    r_sq_per_sec = r^2 / dt_s
    sigma_sq = lambda * sigma_sq + (1-lambda) * r_sq_per_sec
    n_samples++
```

**Why 1-second sampling?**
Binance delivers ~100 trades/sec. Most are at identical prices (same-price fills). Tick-level EWMA produces sigma dominated by zero-returns, requiring a noisy `trades_per_sec` conversion. 1-second sampling:
- Eliminates the zero-return problem
- Gives sigma directly in per-second units
- 10 samples = 10 seconds warmup (vs 300 ticks = ~3s, but useless sigma)
- Sigma is cached in `BinanceState.sigma_real_cached` — updated once/second, zero-cost reads

**Floor**: 30% annualized → ~0.0000534/s. Prevents the model from becoming overconfident during flat periods.

## Latency Profile

| Measurement | Expected |
|---|---|
| `binance_recv` — WS frame → channel send | <50us |
| `pm_recv` — WS frame → channel send | <50us |
| `eval_binance` — 2-3 strategies evaluated | <10us |
| `eval_pm` — 2-3 strategies evaluated | <10us |
| `e2e` — feed received → order dispatched | <50us |

No lock overhead anywhere in the hot path.

## Binaries

| Binary | Command | Purpose |
|---|---|---|
| `bot` | `cargo run --release --bin bot` | Live trading / dry-run |
| `backtester` | `cargo run --release --bin backtester [dir]` | Replay CSVs through strategies |
| `recorder` | `cargo run --release --bin recorder` | Record live feeds to CSV |
| `analyzer` | `cargo run --release --bin analyzer` | Post-hoc data analysis |
| `ws_test` | `cargo run --release --bin ws_test` | Test WS connectivity |

All binaries import from the `polymarket_crypto` library crate.

## Dependencies

Minimal — no `parking_lot`, no locks anywhere:

- `tokio` — Async runtime (mpsc, watch, time, spawn)
- `reqwest` — HTTP (Gamma API, Telegram, future CLOB orders)
- `tokio-tungstenite` — WebSocket (Binance + Polymarket CLOB)
- `serde` / `serde_json` — JSON parsing
- `chrono` — Timestamps
- `futures-util` — Stream utilities for WS
