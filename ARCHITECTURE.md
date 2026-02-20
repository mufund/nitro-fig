# Architecture

Event-driven, low-latency trading system for Polymarket crypto Up/Down binary markets (5m, 15m, 1h, 4h). Single-owner event loop design with zero shared mutable state. Persistent Binance WebSocket connection survives across market cycles. Live order execution via `polymarket-client-sdk` with EIP-712 signed limit orders.

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
│  ┌──────────────────┐              ┌────────────────────────────────┐   │
│  │ ORDER GATEWAY     │              │ TELEMETRY WRITER               │   │
│  │  order_rx(64)     │              │  telem_rx(4096)                │   │
│  │                   │              │  CSVs + Telegram alerts        │   │
│  │ dry_run: simulate │              │  signals / orders / fills /    │   │
│  │ live: CLOB submit │              │  latency / market summary /    │   │
│  │   (EIP-712 sign)  │              │  rejected order alerts         │   │
│  │ USDC balance gate │              │                                │   │
│  │ Ack → feed_tx     │              │                                │   │
│  └──────────────────┘              └────────────────────────────────┘   │
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

**Effect**: Market 1 takes ~10 seconds to warm up EWMA (10 one-second samples). Market 2+ carries over real volatility from the persistent state (ewma_n grows: 0 → 198 → 415 → 609 → ...). However, the engine still requires **10 fresh EWMA samples per market** before trading -- it records the sample count at market entry and waits for 10 new samples to accumulate. This prevents strategies from firing on stale cross-market volatility data.

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
├── main.rs                        # Market loop: discover → connect → engine → repeat (.env loaded via dotenvy)
├── config.rs                      # Interval, Config from env vars/.env, series_id lookup, CLOB credentials
├── types.rs                       # FeedEvent, BinanceTrade, Signal, Fill, Order, etc.
├── feeds/
│   ├── mod.rs
│   ├── binance.rs                 # Persistent Binance WS → FeedEvent::BinanceTrade
│   └── polymarket.rs              # Per-market CLOB WS → PolymarketQuote + PolymarketBook
├── engine/
│   ├── mod.rs
│   ├── state.rs                   # BinanceState (persistent) + MarketState (per-market)
│   ├── risk.rs                    # Two-tier risk: per-strategy + portfolio-level
│   ├── runner.rs                  # Core event loop + LiveSink + diagnostics
│   └── pipeline.rs                # Shared signal pipeline (deconfliction, sorting, risk, coherence)
├── strategies/
│   ├── mod.rs                     # Strategy trait + evaluate_filtered + kelly()
│   ├── latency_arb.rs             # S1: Binance→PM latency exploitation
│   ├── certainty_capture.rs       # S2: z-score gated settlement convergence
│   ├── convexity_fade.rs          # S3: ATM gamma/convexity mean-reversion
│   ├── strike_misalign.rs         # S4: VWAP vs strike bias in first 15s
│   ├── lp_extreme.rs              # S5: Passive LP on losing side (tail risk)
│   ├── cross_timeframe.rs         # S6: Vol surface RV (disabled — no feed)
│   ├── test_helpers.rs            # Shared test fixtures (make_state, inject_book, etc.)
│   └── bench_latency.rs           # Per-strategy evaluation latency benchmarks
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
│   └── order.rs                   # Order gateway: CLOB execution (live) / simulation (dry_run), USDC balance gate
├── telemetry/
│   ├── mod.rs
│   ├── writer.rs                  # Single writer task: CSVs + Telegram
│   └── telegram.rs                # Telegram Bot API client
├── market/
│   ├── mod.rs
│   └── discovery.rs               # Gamma API: slug + series_id market discovery
└── bin/
    ├── backtester.rs              # Replay CSVs through library strategies
    ├── recorder.rs                # Record live market feeds to CSV (--cycles N)
    ├── replay/                    # Interactive TUI for recorded data analysis
    │   ├── main.rs                # Entry point, event loop, keybindings
    │   ├── types.rs               # CSV row types, ReplayEvent, App struct
    │   ├── loader.rs              # CSV loading, event merging
    │   ├── app.rs                 # Core logic: snapshots, navigation, strategy eval
    │   └── render.rs              # All TUI rendering (charts, orderbook, metrics)
    ├── backtest/                   # Multi-market backtester with 8-tab TUI + text dump
    │   ├── main.rs                # Entry point, CLI args, keybindings, text dump
    │   ├── engine.rs              # Market replay, BacktestSink, CSV loaders
    │   ├── types.rs               # TradeRecord, MarketResult, StrategyStats, BacktestApp (analytics)
    │   └── render.rs              # 8-tab TUI rendering (summary, strategies, markets, trades, equity, risk, timing, correlation)
    ├── approve.rs                 # One-time on-chain USDC.e + CTF approvals for Polymarket CLOB
    ├── analyzer.rs                # Post-hoc analysis of recorded data
    └── ws_test.rs                 # WebSocket connectivity test
```

## Market Lifecycle

Each market follows this sequence:

1. **Discover** — Query Gamma API by series_id to find next market (slug, token IDs, tick_size, neg_risk)
2. **Wait** — Sleep until pre-wake seconds before market start (10s for 5m, 30s for 1h)
3. **Set strike** — Fetch candle open price from Binance klines API for the market's interval
4. **Create channels** — Per-market `feed_tx/rx`, `order_tx/rx`, `telem_tx/rx`, market context oneshot
5. **Activate Binance** — Swap the Binance feed's output to this market's `feed_tx`
6. **Spawn per-market tasks** — Polymarket WS, heartbeat tick (100ms), order gateway, telemetry writer
7. **Run engine** — Process events until `market.end_ms + 10s`, returns `BinanceState`
8. **Pause Binance** — Set feed swap to `None` (trades dropped between markets)
9. **Cleanup** — Abort PM feed, tick, gateway, telemetry. Flush logs.
10. **Loop** — Discover next market, repeat

Markets auto-cycle indefinitely. The Binance WebSocket is never disconnected.

## Core Engine

`engine/runner.rs` processes events sequentially in a single async task.

**Strategy instantiation**: Strategies are conditionally loaded based on `Config` toggles (env vars `STRAT_*`). At startup, the engine builds three trigger-partitioned vectors, only including enabled strategies:

```rust
binance_strategies:  [latency_arb, lp_extreme]         // if enabled in config
pm_strategies:       [certainty_capture, convexity_fade, lp_extreme]
open_strategies:     [strike_misalign]
```

**Per-market warmup**: Before evaluating binance/pm-triggered strategies, the engine requires 10 fresh 1-second EWMA samples collected since the current market started. This prevents firing on stale cross-market volatility. **Exception**: `open_strategies` (strike_misalign) are exempt — they only need `ewma_vol.is_valid()` and can fire immediately at market open.

**Strategy evaluation triggers:**
- `BinanceTrade` → evaluates `binance_strategies` + `open_strategies` if in opening window
- `PolymarketQuote` / `PolymarketBook` → evaluates `pm_strategies` + `open_strategies` if in opening window
- `OrderAck` → records fill, updates position
- `Tick` → stale data detection (5s threshold)

**Shared signal pipeline** (`engine/pipeline.rs`): Both the live engine and the backtester process signals through the same `process_signals()` function. This guarantees identical behavior: house-side filtering, deconfliction (scoring conflicting sides by `sum(edge * confidence)`), sorting by score, risk checking, and house-side setting. Engine-specific behavior (async channel dispatch for live, Vec pushes for backtest) is abstracted via the `SignalSink` trait. The live engine implements `LiveSink`, the backtester implements `BacktestSink`.

**Side coherence**: First dispatched active order with confidence >= 0.7 sets `house_side`. Subsequent active orders must agree. Passive signals (lp_extreme) are exempt. Low-confidence signals (e.g. convexity_fade at 0.3-0.65) cannot lock portfolio direction. See [STRATEGIES.md](STRATEGIES.md) for details.

**Settlement**: At market end, determines outcome from final `distance()`, iterates over all fills, computes realized PnL per fill and per strategy.

**Diagnostics**: Every 10 seconds, logs `[DIAG]` block showing z-score, regime, distance, and per-strategy gate analysis.

## Risk Management

`engine/risk.rs` — Two-tier system: per-strategy limits + portfolio-level caps.

**Per-strategy limits** (each strategy operates independently):

| Strategy | Per-trade | Total | Cooldown | Max orders |
|----------|-----------|-------|----------|------------|
| latency_arb | $20 (2%) | $40 (4%) | 60s | 2 |
| certainty_capture | $30 (3%) | $30 (3%) | 120s | 1 |
| convexity_fade | $10 (1%) | $20 (2%) | 60s | 2 |
| strike_misalign | $20 (2%) | $20 (2%) | 15s | 1 |
| lp_extreme | $20 (2%) | $20 (2%) | 120s | 1 |

**Portfolio-level gates** (checked before per-strategy):

| Gate | Default | Env Var |
|------|---------|---------|
| Max total exposure | 15% of bankroll | `MAX_EXPOSURE_FRAC` |
| Daily loss halt | -3% of bankroll | `DAILY_LOSS_HALT` |
| Weekly loss halt | -8% of bankroll | `WEEKLY_LOSS_HALT` |
| Stale feed rejection | 5s threshold | — |

**Sizing flow**: `Signal.size_frac * bankroll` → capped by per-trade limit → capped by strategy room → capped by portfolio room → minimum $1.

**PnL accounting**: Fills are tracked in `Vec<Fill>` during the market. At settlement, `settle_market(outcome, fills)` computes correct binary PnL and updates daily/weekly counters. Per-market exposure resets to zero.

## Order Gateway

`gateway/order.rs` — background task that receives orders from the engine and returns acks.

**Two modes:**
- **`dry_run=true`** — Simulates immediate fills at the order price with 0ms latency. No network I/O.
- **`dry_run=false`** — Real CLOB execution via `polymarket-client-sdk`:

**Live execution flow:**
1. Wait for `MarketContext` (tick_size, neg_risk, token IDs) from main.rs
2. Authenticate: `LocalSigner` from `POLYMARKET_PRIVATE_KEY` → `Client::authentication_builder()` → `.authenticate().await`
3. Pre-flight: query USDC balance via `balance_allowance()` API, log warning if zero
4. Per order:
   - **USDC balance gate**: reject locally if insufficient funds (emits `OrderRejectedLocal` telemetry + TG alert)
   - Convert price (f64 → Decimal with tick_size precision), size (USDC → shares, floored to whole number)
   - Build limit order: `client.limit_order().token_id().price().size().side(Buy).order_type(FOK|GTC).tick_size()` + `.neg_risk(true)` if applicable
   - Sign with EIP-712: `client.sign(&signer, order).await`
   - Submit: `client.post_order(signed).await` → `Vec<PostOrderResponse>`
   - Record raw request/response JSON to `clob_raw.csv` via telemetry
   - Return `OrderAck` with status, latency, CLOB order ID
5. Deduct spent USDC from local balance tracker on successful fills

**Order type mapping** (set in `risk.rs`):
- `signal.is_passive == false` → `OrderType::FOK` (aggressive taker, crosses spread)
- `signal.is_passive == true` → `OrderType::GTC` + `post_only: true` (passive maker, rests on book)

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
| `approve` | `cargo run --release --bin approve` | One-time on-chain USDC.e + CTF approvals |
| `backtest` | `cargo run --release --bin backtest -- logs/1h` | Multi-market backtester with 8-tab TUI dashboard (or `--dump` for text) |
| `backtester` | `cargo run --release --bin backtester [dir]` | Replay CSVs through strategies (legacy) |
| `recorder` | `cargo run --release --bin recorder -- --cycles N` | Record live feeds to CSV |
| `replay` | `cargo run --release --bin replay -- <data_dir>` | Interactive TUI: charts, orderbook, strategy signals |
| `analyzer` | `cargo run --release --bin analyzer` | Post-hoc data analysis |
| `ws_test` | `cargo run --release --bin ws_test` | Test WS connectivity |

All binaries import from the `polymarket_crypto` library crate.

## Replay TUI Architecture

The `replay` binary provides an interactive terminal interface for stepping through recorded market data. It replays events through the same `MarketState`, strategy, and risk code used in live trading.

```
┌─────────────────────────────────────────────────────────────┐
│  loader.rs                                                   │
│  CSV files → BinanceCsvRow / PmCsvRow / BookSnapshot         │
│  merge_events() → sorted Vec<ReplayEvent>                    │
└──────────────────────┬──────────────────────────────────────┘
                       ▼
┌─────────────────────────────────────────────────────────────┐
│  app.rs                                                      │
│  App { events, cursor, state: MarketState, snapshots, ... }  │
│                                                              │
│  build_snapshots(): clone MarketState every 1000 events      │
│  step_forward(n): apply events, eval strategies, push charts │
│  step_back(n): restore nearest snapshot, replay forward      │
│  export_csv(): write all intermediate values to CSV          │
│                                                              │
│  StrategySet: shared strategy instances (LA, CC, CF, SM, LP) │
│  evaluate_event() + record_signals_and_orders()              │
└──────────────────────┬──────────────────────────────────────┘
                       ▼
┌─────────────────────────────────────────────────────────────┐
│  render.rs                                                   │
│  draw() → layout: header | left(books+metrics) | right(…)   │
│                                                              │
│  render_price_chart(): BTC(yellow) + Strike(magenta) +       │
│    VWAP(cyan) + signal/order markers snapped to price line   │
│  render_pm_chart(): YES/NO split with bid/ask lines +        │
│    per-strategy fair value scatter (6 colors)                │
│  render_volume_sparklines(): buy(green) / sell(red)          │
│  render_orderbook(): horizontal depth bars                   │
│  render_metrics(): sigma, z, fair, delta, regime, VWAP, …   │
│  render_signals() / render_orders(): scrollable tables       │
└─────────────────────────────────────────────────────────────┘
```

**Snapshot-based navigation**: Cloning `MarketState` every 1000 events enables O(1000) backward jumps instead of replaying from the start. Binary search via `partition_point` finds the nearest prior snapshot.

**Signal snap-to-line**: Strategy signals fire on PM events, whose event indices fall between Binance trade indices on the x-axis. A binary search in `price_history` snaps each marker to the nearest BTC price point on the line.

## Dependencies

Minimal — no `parking_lot`, no locks anywhere:

- `tokio` — Async runtime (mpsc, watch, time, spawn)
- `polymarket-client-sdk` — CLOB client, EIP-712 signing, alloy signer (with `clob` + `ctf` features)
- `reqwest` — HTTP (Gamma API, Telegram)
- `tokio-tungstenite` — WebSocket (Binance + Polymarket CLOB)
- `serde` / `serde_json` — JSON parsing
- `dotenvy` — `.env` file loading
- `chrono` — Timestamps
- `futures-util` — Stream utilities for WS
- `ratatui` + `crossterm` — Terminal UI framework (replay TUI)
