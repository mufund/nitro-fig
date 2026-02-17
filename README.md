# nitro-fig

Event-driven Polymarket crypto binary options trading bot.

## What It Does

Trades BTC/ETH/SOL/XRP up/down 5-minute binary markets on Polymarket using Binance spot as a price oracle. The bot auto-discovers markets, connects to both exchanges via WebSocket, evaluates six independent strategies on every tick, and settles PnL at market close. Markets cycle continuously with no manual intervention.

## Key Features

- **Single-owner async event loop** -- zero shared mutable state, no locks on the hot path
- **6 configurable strategies** -- latency arbitrage, certainty capture, convexity fade, strike misalignment, extreme probability LP, cross-timeframe (disabled by default). Each individually togglable via env vars.
- **Persistent Binance state** -- EWMA volatility, VWAP, and regime classifier carry across market cycles (only the first market needs warmup)
- **Binary settlement PnL** -- correct accounting at market end, not at fill time
- **Side coherence** -- active strategies agree on a directional house view; passive LP is exempt
- **Two-tier risk** -- per-strategy limits (size, cooldown, order caps) plus portfolio-level exposure and loss halts
- **Interactive replay TUI** -- step through recorded market data tick-by-tick with live orderbook, BTC/PM charts, strategy signals, and CSV export

## Quick Start

See [DEPLOY.md](DEPLOY.md) for VPS setup, environment variables, and deploy commands.

```bash
# Build
cargo build --release

# Run (dry-run mode by default)
DRY_RUN=true BANKROLL=1000 ./target/release/bot

# Record 5 market cycles
cargo run --release --bin recorder -- --cycles 5

# Replay recorded data in the TUI
cargo run --release --bin replay -- logs/5m/btc-updown-5m-1771320600
```

## Documentation

| Document | Contents |
|----------|----------|
| [ARCHITECTURE.md](ARCHITECTURE.md) | Data flow, module map, market lifecycle, risk tiers, volatility model, latency profile |
| [STRATEGIES.md](STRATEGIES.md) | Full explanation of all 6 strategies, side coherence, half-Kelly sizing, PnL accounting |
| [DEPLOY.md](DEPLOY.md) | VPS details, environment variables, rsync deploy, start script, monitoring commands |
| [DEBUGGING.md](DEBUGGING.md) | Log filtering, DIAG field reference, reading signals/fills/settlement, common issues, one-liners |

## Binaries

| Binary | Command | Purpose |
|--------|---------|---------|
| `bot` | `cargo run --release --bin bot` | Live trading / dry-run |
| `backtester` | `cargo run --release --bin backtester [dir]` | Replay recorded CSVs through strategies, print signal/order summary |
| `recorder` | `cargo run --release --bin recorder -- --cycles N` | Record live Binance + Polymarket feeds to CSV (default: infinite cycles) |
| `replay` | `cargo run --release --bin replay -- <data_dir>` | Interactive TUI: step through recorded data with charts and strategy eval |
| `analyzer` | `cargo run --release --bin analyzer` | Post-hoc analysis of recorded data |
| `ws_test` | `cargo run --release --bin ws_test` | Test WebSocket connectivity to Binance and Polymarket |

All binaries import from the `polymarket_crypto` library crate (`src/lib.rs`).

## Replay TUI

The replay binary provides an interactive terminal interface for debugging and analyzing recorded market data. It replays events through the same strategy and risk code used in live trading.

**Panels:**
- **Orderbook depth** -- UP and DOWN books rendered as horizontal bar charts
- **BTC price chart** -- price line (yellow), strike (magenta), VWAP (cyan), with signal/order markers snapped to the price line
- **Polymarket YES/NO charts** -- bid (green) / ask (red) lines with per-strategy fair value dots (LA=yellow, CC=cyan, CF=magenta, CT=blue, SM=red, LP=green)
- **Volume sparklines** -- buy (green) / sell (red) Binance trade volume
- **Metrics** -- sigma, z-score, fair value, delta, regime, VWAP, distance, EWMA, book depth
- **Signals & orders** -- scrollable tables of every strategy signal and dispatched order

**Keybindings:**

| Key | Action |
|-----|--------|
| `Right` / `l` | Step forward 1 event |
| `Left` / `h` | Step back 1 event |
| `Space` | Play / pause |
| `+` / `-` | Speed up / slow down (1x-32x) |
| `PgDn` / `n` | Jump forward 100 events |
| `PgUp` / `b` | Jump back 100 events |
| `Home` / `g` | Jump to start |
| `End` / `G` | Jump to end |
| `s` | Export CSV snapshot (all intermediate values) |
| `q` / `Esc` | Quit |

Backward navigation uses periodic `MarketState` snapshots (every 1000 events) to avoid replaying from the start.

## Strategy Configuration

Each strategy can be independently enabled or disabled via environment variables. All active strategies default to **enabled**; set to `0` or `false` to disable.

| Env Var | Strategy | Default |
|---------|----------|---------|
| `STRAT_LATENCY_ARB` | Latency Arbitrage | enabled |
| `STRAT_CERTAINTY_CAPTURE` | Certainty Capture | enabled |
| `STRAT_CONVEXITY_FADE` | Convexity Fade | enabled |
| `STRAT_STRIKE_MISALIGN` | Strike Misalignment | enabled |
| `STRAT_LP_EXTREME` | Extreme Probability LP | enabled |
| `STRAT_CROSS_TF` | Cross-Timeframe RV | **disabled** |

```bash
# Example: disable convexity fade and latency arb
STRAT_CONVEXITY_FADE=false STRAT_LATENCY_ARB=0 ./target/release/bot
```

## Test Coverage

259 unit tests across all modules including:
- Strategy evaluation correctness and edge cases
- Risk management gate chain (10 sequential gates)
- Math library (pricing, EWMA, VWAP, regime, normal distribution)
- Market discovery and orderbook depth
- End-to-end calculation latency benchmarks (<1us per strategy evaluation)

```bash
cargo test             # run all tests
cargo test -- -q       # quiet output
cargo test latency     # run latency benchmarks only
```

## License

Private / proprietary. Not licensed for redistribution.
