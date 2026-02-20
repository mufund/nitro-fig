# Deployment

## VPS

- **IP**: 82.24.195.32
- **User**: `root`
- **OS**: Ubuntu (x86_64)
- **Rust**: 1.93.1+ installed under `~/.cargo/env`

## Paths

| What | Path |
|------|------|
| Source | `/root/nitro-fig/` |
| `.env` config | `/root/nitro-fig/.env` |
| Release binary | `/root/nitro-fig/target/release/bot` |
| Approve binary | `/root/nitro-fig/target/release/approve` |
| Start script | `/root/nitro-fig/start.sh` |
| Log directory | `/root/nitro-fig/logs/` |
| Latest log symlink | `/root/nitro-fig/logs/latest.log` |

## Environment Variables

All variables can be set via a `.env` file in the project root (loaded by `dotenvy` at startup) or as traditional environment variables. Environment variables take precedence over `.env` values.

**Core:**

| Variable | Default | Description |
|----------|---------|-------------|
| `DRY_RUN` | `true` | Simulate fills (no real orders). Set `false` for live |
| `BANKROLL` | `1000` | Total bankroll in USD |
| `ASSET` | `btc` | Asset: `btc`, `eth`, `sol`, `xrp` |
| `INTERVAL` | `5m` | Market interval: `5m`, `15m`, `1h`, `4h` |

**CLOB execution (required for `DRY_RUN=false`):**

| Variable | Default | Description |
|----------|---------|-------------|
| `POLYMARKET_PRIVATE_KEY` | _(none)_ | Hex private key for EIP-712 signing (Polygon wallet) |
| `POLYMARKET_SIG_TYPE` | `0` | Signature type: `0`=EOA, `1`=Poly Proxy, `2`=Gnosis Safe |
| `POLYMARKET_FUNDER_ADDRESS` | _(none)_ | Optional funder address (for proxy/safe setups) |

**Risk:**

| Variable | Default | Description |
|----------|---------|-------------|
| `MAX_EXPOSURE_FRAC` | `0.15` | Max portfolio exposure as fraction of bankroll |
| `DAILY_LOSS_HALT` | `-0.03` | Daily loss fraction that triggers halt |
| `WEEKLY_LOSS_HALT` | `-0.08` | Weekly loss fraction that triggers halt |

**Model:**

| Variable | Default | Description |
|----------|---------|-------------|
| `ORACLE_DELTA_S` | `2.0` | Oracle timestamp uncertainty in seconds |
| `EWMA_LAMBDA` | `0.94` | EWMA decay factor for realized vol |
| `SIGMA_FLOOR_ANNUAL` | `0.30` | Minimum annualized vol (prevents overconfidence) |

**Feeds:**

| Variable | Default | Description |
|----------|---------|-------------|
| `BINANCE_WS` | auto-derived | Binance trade stream URL |
| `TELEGRAM_BOT_TOKEN` | _(empty)_ | Telegram Bot API token (enables TG alerts) |
| `TELEGRAM_CHAT_ID` | _(empty)_ | Telegram chat ID for alerts |

**Strategy toggles:**

| Variable | Default | Description |
|----------|---------|-------------|
| `STRAT_LATENCY_ARB` | `true` | Enable/disable latency arbitrage strategy |
| `STRAT_CERTAINTY_CAPTURE` | `true` | Enable/disable certainty capture strategy |
| `STRAT_CONVEXITY_FADE` | `true` | Enable/disable convexity fade strategy |
| `STRAT_STRIKE_MISALIGN` | `true` | Enable/disable strike misalignment strategy |
| `STRAT_LP_EXTREME` | `true` | Enable/disable extreme probability LP strategy |
| `STRAT_CROSS_TF` | `false` | Enable/disable cross-timeframe RV (requires cross-market feed) |

## Quick Deploy (from local machine)

```bash
# 1. Sync source to VPS (exclude target, git, logs, .env, and start.sh)
rsync -az --delete \
  --exclude 'target' --exclude '.git' --exclude 'logs' \
  --exclude 'start.sh' --exclude '.env' \
  /Users/keon/dev/mu/nitro-fig/ \
  root@82.24.195.32:/root/nitro-fig/

# 2. Build on VPS
ssh root@82.24.195.32 "source ~/.cargo/env && \
  cd /root/nitro-fig && \
  cargo build --release"

# 3. Start
ssh root@82.24.195.32 "bash /root/nitro-fig/start.sh"
```

**Important**: Always exclude `.env` from rsync. The VPS `.env` contains the live private key and `DRY_RUN=false`; the local `.env` may have `DRY_RUN=true` or different tokens.

## `.env` File

The bot loads configuration from `/root/nitro-fig/.env` via `dotenvy`. Create this file on the VPS:

```bash
# /root/nitro-fig/.env
ASSET=btc
INTERVAL=1h
DRY_RUN=false
BANKROLL=1000

# CLOB execution
POLYMARKET_PRIVATE_KEY=<your-hex-private-key>
POLYMARKET_SIG_TYPE=0

# Risk
MAX_EXPOSURE_FRAC=0.15
ORACLE_DELTA_S=2.0
EWMA_LAMBDA=0.94
SIGMA_FLOOR_ANNUAL=0.30

# Feeds
BINANCE_WS=wss://stream.binance.com:9443/ws/btcusdt@trade

# Telegram alerts (optional)
TELEGRAM_BOT_TOKEN=<your-bot-token>
TELEGRAM_CHAT_ID=<your-chat-id>

# Strategy toggles (uncomment to disable)
# STRAT_LATENCY_ARB=false
# STRAT_CERTAINTY_CAPTURE=false
# STRAT_CONVEXITY_FADE=false
# STRAT_STRIKE_MISALIGN=false
# STRAT_LP_EXTREME=false
# STRAT_CROSS_TF=true
```

## Start Script

`/root/nitro-fig/start.sh`:

```bash
#!/bin/bash
cd /root/nitro-fig
pkill -f "target/release/bot" 2>/dev/null
sleep 1
# Config is loaded from .env by the bot (dotenvy)
mkdir -p /root/nitro-fig/logs
LOGFILE=/root/nitro-fig/logs/bot-$(date +%Y%m%d-%H%M%S).log
nohup ./target/release/bot >> "$LOGFILE" 2>&1 &
echo "PID=$! LOG=$LOGFILE"
ln -sf "$LOGFILE" /root/nitro-fig/logs/latest.log
```

## Monitoring

```bash
ssh root@82.24.195.32

# Live log output
tail -f /root/nitro-fig/logs/latest.log

# Watch only signals and fills
tail -f /root/nitro-fig/logs/latest.log | grep -E 'SIG|FILL|ENGINE.*ended'

# Watch diagnostic logs
tail -f /root/nitro-fig/logs/latest.log | grep DIAG

# Check if running
ps aux | grep bot

# Stop
pkill -f "target/release/bot"

# Count regime types in current session
grep -c 'regime=Range' /root/nitro-fig/logs/latest.log
grep -c 'regime=Ambiguous' /root/nitro-fig/logs/latest.log
grep -c 'regime=Trend' /root/nitro-fig/logs/latest.log

# See per-strategy fill breakdown
grep 'ENGINE.*:' /root/nitro-fig/logs/latest.log
```

## Log Format

Each market produces log output like:

```
[ENGINE] Running market btc-updown-1h-... | strike=$95000 | window=3600s | bankroll=$1000 | ewma_n=415 | warmup_baseline=415
[ENGINE] Warmup complete: σ_real=0.00009092 ... total_samples=425 fresh=10 | trading enabled
[GW] Order gateway started (dry_run=false)
[GW] CLOB client authenticated, address=0x...
[GW] Available USDC for trading: $150.00
[DIAG] t_left=3550s sigma=0.00009092 z=0.00 dist=$0 regime=Ambiguous(73%/251) house=None ...
[SIG] strike_misalign Down edge=0.078 fair=0.597 mkt=0.519 sz=$20.0 ACTIVE
[FILL] #1 [strike_misalign] Down Filled price=Some(0.519) size=Some(20.0) lat=142.3ms
[TELEM] Order #2 rejected locally: insufficient USDC (latency_arb)
[ENGINE] Market ... ended | outcome=Down | house=Some(Down) | sig=4186 ord=12 fill=12 pnl=$58.04
[ENGINE]   strike_misalign: sig=94 ord=2 fill=2 pnl=$19.42 avg_edge=0.082
[ENGINE]   latency_arb: sig=1440 ord=4 fill=4 pnl=$28.02 avg_edge=0.042
```

## Recording & Replay

The recorder captures live market data for offline analysis. The replay TUI visualizes it interactively.

```bash
# Record on VPS (5 market cycles)
ssh root@82.24.195.32 "source ~/.cargo/env && \
  cd /root/nitro-fig && \
  cargo run --release --bin recorder -- --cycles 5"

# Sync recorded data back to local machine
rsync -az root@82.24.195.32:/root/nitro-fig/logs/5m/ ./logs/5m/

# Replay locally with TUI
cargo run --release --bin replay -- logs/5m/btc-updown-5m-1771320600
```

Each recorded market produces a directory under `logs/{interval}/{slug}/` containing:
- `binance.csv` — Binance trade stream (timestamp, price, qty, is_buy)
- `polymarket.csv` — PM best bid/ask for UP and DOWN tokens
- `book.csv` — PM orderbook depth snapshots
- `market_info.txt` — slug, start/end timestamps, strike price

The replay TUI is a local-only tool (requires a terminal with color support). It does not need network access.

## First-Time Setup (Live Trading)

Before the first live trade, run the one-time on-chain approval:

```bash
# On VPS (reads POLYMARKET_PRIVATE_KEY from .env)
cd /root/nitro-fig
cargo run --release --bin approve
```

This approves USDC.e (ERC-20) and Conditional Tokens (ERC-1155) for all 3 Polymarket exchange contracts (CTF Exchange, Neg-Risk Exchange, Neg-Risk Adapter). Only needed once per wallet.

Also fund the wallet with USDC.e on Polygon. The gateway checks the balance at startup and logs a warning if it is zero. Orders are locally rejected if insufficient USDC is available.

## Notes

- **Binance WS geo-blocking**: `stream.binance.com` is blocked from US IPs. The EU VPS works fine.
- **Strike price**: Set from Binance klines API (candle open price for the market's interval). Polymarket uses Chainlink oracle (small difference handled by `ORACLE_DELTA_S`).
- **Market cycling**: Bot runs indefinitely, auto-discovering next market. No cron needed.
- **Build on VPS**: Cross-compiling from macOS aarch64 to Linux x86_64 requires a toolchain. Build directly on VPS.
- **start.sh and .env are server-only**: The rsync excludes both to avoid overwriting. Edit directly on the server.
- **EWMA persistence**: The bot must stay running across markets for EWMA to accumulate. Restarting resets to cold start (10s warmup per market).
- **Per-market warmup**: Even with persistent EWMA, each market requires 10 fresh EWMA samples (~10s) before most strategies are enabled. Exception: `strike_misalign` is exempt and can fire immediately at market open (only needs valid sigma).
- **Replay TUI**: Requires `ratatui`/`crossterm`. Build with `cargo build --release --bin replay`. Only works locally (not over SSH without proper terminal forwarding).
- **Telegram alerts**: Orders, fills, market start/end, strategy metrics, and locally-rejected orders (e.g. insufficient balance) are sent to Telegram if configured. All TG sends are fire-and-forget (never block the main loop).
