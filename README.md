# nitro-fig

Event-driven Polymarket crypto binary options trading bot.

## What It Does

Trades BTC/ETH/SOL/XRP up/down 5-minute binary markets on Polymarket using Binance spot as a price oracle. The bot auto-discovers markets, connects to both exchanges via WebSocket, evaluates five independent strategies on every tick, and settles PnL at market close. Markets cycle continuously with no manual intervention.

## Key Features

- **Single-owner async event loop** -- zero shared mutable state, no locks on the hot path
- **6 configurable strategies** -- latency arbitrage, certainty capture, convexity fade, strike misalignment, extreme probability LP, cross-timeframe (disabled by default). Each individually togglable via env vars.
- **Persistent Binance state** -- EWMA volatility, VWAP, and regime classifier carry across market cycles (only the first market needs warmup)
- **Binary settlement PnL** -- correct accounting at market end, not at fill time
- **Side coherence** -- active strategies agree on a directional house view; passive LP is exempt
- **Two-tier risk** -- per-strategy limits (size, cooldown, order caps) plus portfolio-level exposure and loss halts

## Quick Start

See [DEPLOY.md](DEPLOY.md) for VPS setup, environment variables, and deploy commands.

```bash
# Build
cargo build --release

# Run (dry-run mode by default)
DRY_RUN=true BANKROLL=1000 ./target/release/bot
```

## Documentation

| Document | Contents |
|----------|----------|
| [ARCHITECTURE.md](ARCHITECTURE.md) | Data flow, module map, market lifecycle, risk tiers, volatility model, latency profile |
| [STRATEGIES.md](STRATEGIES.md) | Full explanation of all 5 strategies, side coherence, half-Kelly sizing, PnL accounting |
| [DEPLOY.md](DEPLOY.md) | VPS details, environment variables, rsync deploy, start script, monitoring commands |
| [DEBUGGING.md](DEBUGGING.md) | Log filtering, DIAG field reference, reading signals/fills/settlement, common issues, one-liners |

## Binaries

| Binary | Command | Purpose |
|--------|---------|---------|
| `bot` | `cargo run --release --bin bot` | Live trading / dry-run |
| `backtester` | `cargo run --release --bin backtester [dir]` | Replay recorded CSVs through strategies |
| `recorder` | `cargo run --release --bin recorder` | Record live Binance + Polymarket feeds to CSV |
| `analyzer` | `cargo run --release --bin analyzer` | Post-hoc analysis of recorded data |
| `ws_test` | `cargo run --release --bin ws_test` | Test WebSocket connectivity to Binance and Polymarket |

All binaries import from the `polymarket_crypto` library crate (`src/lib.rs`).

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

260+ unit tests across all modules including:
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
