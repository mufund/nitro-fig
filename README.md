# nitro-fig

Event-driven Polymarket crypto binary options trading bot.

## What It Does

Trades BTC/ETH/SOL/XRP up/down binary markets (5m, 15m, 1h, 4h intervals) on Polymarket using Binance spot as a price oracle. The bot auto-discovers markets, connects to both exchanges via WebSocket, evaluates six independent strategies on every tick, and submits EIP-712 signed limit orders to the Polymarket CLOB. Markets cycle continuously with no manual intervention.

## Key Features

- **Live CLOB execution** -- EIP-712 signed limit orders via `polymarket-client-sdk`, with pre-flight USDC balance checks and raw request/response logging
- **Single-owner async event loop** -- zero shared mutable state, no locks on the hot path
- **6 configurable strategies** -- latency arbitrage, certainty capture, convexity fade, strike misalignment, extreme probability LP, cross-timeframe (disabled by default). Each individually togglable via env vars.
- **Persistent Binance state** -- EWMA volatility, VWAP, and regime classifier carry across market cycles. Per-market warmup gate requires 10 fresh EWMA samples before trading.
- **Binary settlement PnL** -- correct accounting at market end, not at fill time
- **Side coherence** -- active strategies agree on a directional house view; passive LP is exempt
- **Two-tier risk** -- per-strategy limits (size, cooldown, order caps) plus portfolio-level exposure, loss halts, and optional delta/gamma gates
- **Telegram alerts** -- order submissions, fills, market start/end summaries, strategy metrics, and locally-rejected orders (e.g. insufficient balance)
- **`.env` file support** -- configuration via `.env` file (loaded by `dotenvy`) or environment variables
- **Interactive replay TUI** -- step through recorded market data tick-by-tick with live orderbook, BTC/PM charts, strategy signals, and CSV export
- **Institutional backtest TUI** -- 8-tab analytics dashboard: summary, strategies, markets, trades, equity, risk, timing, correlation matrix

## Quick Start

See [DEPLOY.md](DEPLOY.md) for VPS setup, `.env` configuration, and deploy commands.

```bash
# Build
cargo build --release

# Configure via .env file (or export env vars directly)
cp .env.example .env   # edit with your keys

# Run (dry-run mode by default)
./target/release/bot

# Run live (requires POLYMARKET_PRIVATE_KEY in .env)
DRY_RUN=false ./target/release/bot

# One-time on-chain USDC.e + CTF approvals (required before first live trade)
cargo run --release --bin approve

# Auto-redeem all resolved positions (or set up as cron)
cargo run --release --bin auto-redeem

# Record 5 market cycles
cargo run --release --bin recorder -- --cycles 5

# Replay recorded data in the TUI
cargo run --release --bin replay -- logs/5m/btc-updown-5m-1771320600

# Backtest on recorded 1-hour markets (interactive TUI)
cargo run --release --bin backtest -- logs/1h

# Backtest with text dump (no TUI)
cargo run --release --bin backtest -- --dump logs/1h
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
| `approve` | `cargo run --release --bin approve` | One-time on-chain USDC.e + CTF approvals for all 3 Polymarket exchange contracts |
| `redeem` | `cargo run --release --bin redeem -- <condition_id>` | Manually redeem a resolved market by condition ID |
| `auto-redeem` | `cargo run --release --bin auto-redeem` | Auto-redeem all resolved positions (runs via cron every 30 min) |
| `backtest` | `cargo run --release --bin backtest -- logs/1h` | Multi-market backtester with 8-tab TUI dashboard (or `--dump` for text mode) |
| `backtester` | `cargo run --release --bin backtester [dir]` | Replay recorded CSVs through strategies, print signal/order summary (legacy) |
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

## Backtest TUI

The backtest binary provides an institutional-grade analytics dashboard with 8 tabs, supporting both interactive TUI and text dump modes.

```bash
# Interactive TUI (default)
cargo run --release --bin backtest -- logs/1h

# Text dump to stdout
cargo run --release --bin backtest -- --dump logs/1h
```

**Tabs:**

| # | Tab | Contents |
|---|-----|----------|
| 1 | Summary | PnL, ROI, Sharpe/Sortino/Calmar, drawdown, win/loss stats, directional analysis, strategy PnL bars, edge capture ratios, per-trade equity curve |
| 2 | Strategies | 15-column comparison table, per-market breakdown, strategy equity curves, detailed metrics (best/worst, streaks, avg size/price, edge std) |
| 3 | Markets | Scrollable market table with outcome, WR, PnL, strategy breakdown. Press Enter to drill-down into a single market (trade table + equity chart) |
| 4 | Trades | All trades with 15 columns, filterable by strategy with [f], scrollable |
| 5 | Equity | Per-trade equity curve with per-strategy overlays, per-market PnL sparkline, rolling 20-trade Sharpe and win rate chart |
| 6 | Risk | Drawdown chart, risk metrics panel (MDD, duration, recovery factor, Kelly), PnL distribution histogram, edge distribution histogram |
| 7 | Timing | PnL by time-remaining buckets, win rate chart, edge/confidence/size analysis per bucket |
| 8 | Correl | Strategy correlation matrix (per-market PnL), diversification score, strongest pairs with visual bars |

**Keybindings:**

| Key | Action |
|-----|--------|
| `1`-`8` / `Tab` | Switch tab |
| `j` / `k` / `Up` / `Down` | Scroll |
| `PgUp` / `PgDn` | Page scroll |
| `f` | Filter trades by strategy (Trades tab) |
| `Enter` | Drill-down into selected market (Markets tab) |
| `Esc` / `Backspace` | Back from drill-down, or quit |
| `Home` / `End` | Jump to top / bottom |
| `q` | Quit |

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

## Order Types

All orders are limit orders submitted via the SDK's `limit_order()` builder. Order type is determined by the signal's `is_passive` and `use_bid` flags:

| Strategy | Order Type | Behavior |
|----------|------------|----------|
| latency_arb | FOK (Fill-or-Kill) | Aggressive taker -- crosses the spread, fills immediately or cancels |
| certainty_capture | GTD (10s TTL) | Aggressive taker at best ask with 10-second expiration |
| convexity_fade | GTD + post_only (10s TTL) | Posts at best bid, rests on book for up to 10 seconds |
| strike_misalign | GTD + post_only (10s TTL) | Posts at best bid, rests on book for up to 10 seconds |
| lp_extreme | GTC + post_only | Passive maker -- rests on the book, earns maker rebate |
| cross_timeframe | GTD (10s TTL) | Aggressive taker at best ask (disabled) |

**Order type routing** (set in `risk.rs`):
- `is_passive` → GTC + post_only (lp_extreme: rests indefinitely)
- `use_bid` → GTD + post_only, 10s TTL (convexity_fade, strike_misalign: posts at best bid)
- `latency_arb` → FOK (latency race, needs instant fill-or-kill)
- All others → GTD, 10s TTL (certainty_capture, cross_timeframe: aggressive at ask with expiration)

## Test Coverage

278 unit tests across all modules including:
- Strategy evaluation correctness and edge cases
- Risk management gate chain (10 sequential gates)
- Math library (pricing, EWMA, VWAP, regime, normal distribution)
- Shared signal pipeline (deconfliction, sorting, house-side coherence, slippage)
- Market discovery and orderbook depth
- End-to-end calculation latency benchmarks (<1us per strategy evaluation)

```bash
cargo test             # run all tests
cargo test -- -q       # quiet output
cargo test latency     # run latency benchmarks only
```

## License

Private / proprietary. Not licensed for redistribution.
