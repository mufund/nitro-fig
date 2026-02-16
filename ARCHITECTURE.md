# Architecture

Event-driven, low-latency trading system for Polymarket crypto Up/Down binary markets. Supports multiple assets (BTC, ETH, SOL, XRP) and intervals (5m, 15m, 1h, 4h) via two env vars: `ASSET` and `INTERVAL`. Single-owner event loop design with zero shared mutable state.

## Data Flow

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Feed Producers (WebSocket)                       │
│                                                                     │
│  ┌──────────────────┐          ┌──────────────────────────────────┐ │
│  │  Binance WS       │          │  Polymarket CLOB WS              │ │
│  │  {asset}usdt@trade│          │  best_bid_ask / price_change     │ │
│  │  → BinanceTrade   │          │  → PolymarketQuote               │ │
│  └────────┬─────────┘          └───────────────┬──────────────────┘ │
│           └──────────────┬─────────────────────┘                    │
│                          ▼                                          │
│                ┌───────────────────┐                                │
│                │  feed_tx → feed_rx │  mpsc(4096)                   │
│                └─────────┬─────────┘                                │
│                          ▼                                          │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │                  CORE ENGINE (single task)                    │  │
│  │                                                              │  │
│  │  MarketState ← owned, not shared (no Arc, no RwLock)         │  │
│  │                                                              │  │
│  │  loop {                                                      │  │
│  │    BinanceTrade  → state.on_binance_trade()      ~50ns       │  │
│  │    PolymarketQuote → state.on_polymarket_quote()  ~50ns      │  │
│  │                    → evaluate_all(strategies)     ~5-10μs    │  │
│  │                    → risk.check()                 ~100ns     │  │
│  │                    → order_tx.try_send()           ~100ns    │  │
│  │    OrderAck      → state.position.on_fill()       ~50ns     │  │
│  │    Tick          → stale data check                          │  │
│  │  }                                                           │  │
│  └─────────────┬─────────────────────────┬──────────────────────┘  │
│                │                         │                          │
│       ┌────────┘                         └────────┐                 │
│       ▼                                           ▼                 │
│  ┌──────────────┐                  ┌──────────────────────────┐    │
│  │  order_tx(64) │                  │  telem_tx(4096)          │    │
│  └──────┬───────┘                  └────────────┬─────────────┘    │
│         ▼                                       ▼                   │
│  ┌──────────────┐      ┌────────────────────────────────────────┐  │
│  │ ORDER GATEWAY │      │ TELEMETRY WRITER                       │  │
│  │               │      │ signals.csv + latency.csv              │  │
│  │ Order → CLOB  │      │ orders.csv  + fills.csv                │  │
│  │ Ack → feed_tx │      │ Telegram alerts (orders/fills/market)  │  │
│  └──────────────┘      └────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

## Why Single-Owner (vs Shared State)

| Concern | Shared State (RwLock) | Single-Owner Event Loop |
|---|---|---|
| Race conditions | Possible | Impossible by construction |
| Lock contention | Non-zero jitter | Zero — no locks exist |
| Debugging | Hard to reproduce | Deterministic — replay events |
| Backtesting | Must mock shared state | Same code path — feed events from CSV |
| Code complexity | `Arc<RwLock<T>>` everywhere | Plain owned struct |

## Multi-Asset / Multi-Interval

Two env vars control everything:

| Env Var | Default | Values |
|---|---|---|
| `ASSET` | `btc` | `btc`, `eth`, `sol`, `xrp` |
| `INTERVAL` | `5m` | `5m`, `15m`, `1h`, `4h` |

Auto-derived from `ASSET` + `INTERVAL`:
- **Binance WS URL**: `wss://stream.binance.com:9443/ws/{asset}usdt@trade`
- **Series ID**: Lookup table (e.g. btc/5m=10684, eth/15m=10191)
- **Slug prefix**: `{asset}-updown-{interval}-` (e.g. `btc-updown-15m-`)
- **Log path**: `logs/{interval}/{slug}/`
- **Telegram labels**: Asset name in alerts (BTC, ETH, SOL, XRP)

Known Polymarket series IDs:

| Asset | 5m | 15m | 1h | 4h |
|---|---|---|---|---|
| BTC | 10684 | 10192 | 10114 | 10331 |
| ETH | — | 10191 | — | — |
| SOL | — | 10423 | — | — |
| XRP | — | 10422 | — | — |

Strategy thresholds use **fraction-of-strike** so they scale automatically to any asset price. See [STRATEGIES.md](STRATEGIES.md) for details.

## Module Map

```
src/
├── lib.rs                         # Library crate — re-exports all modules
├── main.rs                        # Market loop: discover → connect → engine → repeat
├── config.rs                      # Interval enum, ASSET/INTERVAL, derived URLs/series_ids
├── types.rs                       # FeedEvent, BinanceTrade, PolymarketQuote, Signal, Order, etc.
├── feeds/
│   ├── mod.rs
│   ├── binance.rs                 # Binance WS → FeedEvent::BinanceTrade
│   └── polymarket.rs              # CLOB WS → FeedEvent::PolymarketQuote
├── engine/
│   ├── mod.rs
│   ├── state.rs                   # MarketState + PositionTracker (owned, no Arc)
│   ├── risk.rs                    # RiskManager: position limits, cooldowns, Kelly sizing
│   └── runner.rs                  # Core event loop: recv → update → eval → dispatch
├── strategies/
│   ├── mod.rs                     # Strategy trait + evaluate_all + select_best + kelly
│   ├── distance_fade.rs           # S1: Binance distance → sigmoid fair value
│   ├── momentum.rs                # S2: Trade flow microstructure
│   └── settlement_sniper.rs       # S3: Late-phase certainty trade
├── gateway/
│   ├── mod.rs
│   └── order.rs                   # Order gateway: execute → ack back via feed channel
├── telemetry/
│   ├── mod.rs
│   ├── writer.rs                  # Single writer task: CSVs + Telegram
│   └── telegram.rs                # Telegram Bot API client
├── market/
│   ├── mod.rs
│   └── discovery.rs               # Gamma API: slug-based + series_id market discovery
└── bin/
    ├── backtester.rs              # Replay CSVs through library strategies (zero duplication)
    ├── recorder.rs                # Record live market feeds to CSV
    └── ws_test.rs                 # WebSocket connectivity test
```

## Market Lifecycle

Each market follows this sequence in `main.rs`:

1. **Discover** — Compute expected slug `{asset}-updown-{interval}-{window_start}`, query Gamma API
2. **Wait** — Sleep until 10s before market start (WS warmup)
3. **Connect Binance WS** — First price becomes the strike
4. **Connect Polymarket CLOB WS** — Subscribe to UP/DOWN token order books
5. **Run engine** — Process events until market.end_ms + 10s
6. **Cleanup** — Abort feeds, flush telemetry, discover next market

Markets auto-cycle: the bot runs indefinitely, handling one market after another.

## Market Discovery

Discovery works by:

1. **Slug lookup** (primary) — Compute `{asset}-updown-{interval}-{window_start}` and query by exact slug
2. **Next window** — If current window's market ended, try the next window boundary
3. **Series fallback** — If slug lookup fails, search by `series_id` for active markets

Note: 1h markets use human-readable slugs (e.g. `bitcoin-up-or-down-february-16-3am-et`), so slug-based discovery won't match — the series_id fallback handles those.

Token IDs are extracted from the event's `markets` array. Handles both:
- Two separate markets with `groupItemTitle` ("Up"/"Down")
- Single market with `outcomes: ["Up","Down"]` and `clobTokenIds` as JSON array string

## Core Engine

`engine/runner.rs` — Single async task that owns all state. Processes events sequentially:

- **BinanceTrade** — Update `binance_price`, maintain 30s rolling trade buffer
- **PolymarketQuote** — Update bid/ask, trigger strategy evaluation on every quote
- **OrderAck** — Update position tracker, record fill results
- **Tick** — 100ms heartbeat for stale data detection

Strategy evaluation only triggers on Polymarket quotes (when both feeds have data).

## Strategies

All strategies implement `Strategy::evaluate(&MarketState, now_ms) -> Option<Signal>`. Stateless pure functions. Same code runs in live engine, backtester, and any future binary via the library crate.

See [STRATEGIES.md](STRATEGIES.md) for detailed descriptions, thresholds, and mathematical models.

Signal selection: highest `edge * confidence` wins. Half-Kelly position sizing.

## Risk Management

`engine/risk.rs` enforces per-market limits:

| Parameter | Env Var | Default |
|---|---|---|
| Max position per market | `MAX_POSITION_USD` | $100 |
| Max orders per market | `MAX_ORDERS_PER_MARKET` | 10 |
| Cooldown between orders | `COOLDOWN_MS` | 5000ms |

Order flow: Signal → Risk check (cooldown, max orders, position limit) → Kelly sizing → `order_tx.try_send()` (non-blocking).

## Order Gateway

`gateway/order.rs` — Background task that:
1. Receives `Order` from engine via `order_rx`
2. Executes against CLOB (dry run: simulate instant fill)
3. Feeds `OrderAck` back to engine via `feed_tx` (so position tracking stays in the engine)

Real CLOB execution (POST /order with signed payload) is stubbed — currently simulates fills.

## Telemetry

`telemetry/writer.rs` — Single background task consolidating ALL I/O:

**CSV output** (per market, under `logs/{interval}/{slug}/`):
| File | Content |
|---|---|
| `signals.csv` | Every strategy signal with full context |
| `latency.csv` | Timing: binance_recv, pm_recv, eval, e2e, order_exec |
| `orders.csv` | Every order attempt with edge, price, size, strategy |
| `fills.csv` | Every fill with price, latency, theoretical PnL |
| `market_info.txt` | Slug, strike, start/end timestamps |

**Telegram alerts** (non-blocking, via Bot API):
- Market start/end summaries
- Order attempts with strategy/edge/price
- Fill results with PnL

Signals go to CSV only (thousands per market would flood Telegram).

## Latency Profile

All timing recorded in `latency.csv`:

| Measurement | Expected |
|---|---|
| `binance_recv` — WS frame → channel send | <50us |
| `pm_recv` — WS frame → channel send | <50us |
| `eval` — 3 strategies evaluated | <20us |
| `e2e` — PM quote received → order dispatched | <50us |

No lock overhead anywhere in the hot path.

## Binaries

| Binary | Command | Purpose |
|---|---|---|
| `bot` | `cargo run --release --bin bot` | Live trading / dry-run |
| `backtester` | `cargo run --release --bin backtester [dir]` | Replay CSVs through strategies |
| `recorder` | `cargo run --release --bin recorder` | Record live feeds to CSV |
| `ws_test` | `cargo run --release --bin ws_test` | Test WS connectivity |

All binaries import from the `polymarket_crypto` library crate — no code duplication.

## Dependencies

Minimal dependency set — no `parking_lot`, no locks in the entire system:

- `tokio` — Async runtime
- `reqwest` — HTTP (Gamma API, Telegram, future CLOB orders)
- `tokio-tungstenite` — WebSocket (Binance + Polymarket CLOB)
- `serde` / `serde_json` — JSON parsing
- `chrono` — Timestamps
- `futures-util` — Stream utilities for WS
