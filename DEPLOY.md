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
| Release binary | `/root/nitro-fig/target/release/bot` |
| Start script | `/root/nitro-fig/start.sh` |
| Log directory | `/root/nitro-fig/logs/` |
| Latest log symlink | `/root/nitro-fig/logs/latest.log` |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `DRY_RUN` | `true` | Simulate fills (no real orders). Set `false` for live |
| `BANKROLL` | `1000` | Total bankroll in USD |
| `MAX_EXPOSURE_FRAC` | `0.15` | Max portfolio exposure as fraction of bankroll |
| `ORACLE_DELTA_S` | `2.0` | Oracle timestamp uncertainty in seconds |
| `EWMA_LAMBDA` | `0.94` | EWMA decay factor for realized vol |
| `SIGMA_FLOOR_ANNUAL` | `0.30` | Minimum annualized vol (prevents overconfidence) |
| `DAILY_LOSS_HALT` | `-0.03` | Daily loss fraction that triggers halt |
| `WEEKLY_LOSS_HALT` | `-0.08` | Weekly loss fraction that triggers halt |
| `ASSET` | `btc` | Asset: `btc`, `eth`, `sol`, `xrp` |
| `INTERVAL` | `5m` | Market interval: `5m`, `15m`, `1h`, `4h` |
| `BINANCE_WS` | auto-derived | Binance trade stream URL |
| `TELEGRAM_BOT_TOKEN` | _(empty)_ | Telegram Bot API token |
| `TELEGRAM_CHAT_ID` | _(empty)_ | Telegram chat ID for alerts |
| `STRAT_LATENCY_ARB` | `true` | Enable/disable latency arbitrage strategy |
| `STRAT_CERTAINTY_CAPTURE` | `true` | Enable/disable certainty capture strategy |
| `STRAT_CONVEXITY_FADE` | `true` | Enable/disable convexity fade strategy |
| `STRAT_STRIKE_MISALIGN` | `true` | Enable/disable strike misalignment strategy |
| `STRAT_LP_EXTREME` | `true` | Enable/disable extreme probability LP strategy |
| `STRAT_CROSS_TF` | `false` | Enable/disable cross-timeframe RV (requires cross-market feed) |

## Quick Deploy (from local machine)

```bash
# 1. Sync source to VPS (exclude target, git, logs, and start.sh)
rsync -az --delete \
  --exclude 'target' --exclude '.git' --exclude 'logs' --exclude 'start.sh' \
  /Users/keon/dev/mu/nitro-fig/ \
  root@82.24.195.32:/root/nitro-fig/

# 2. Build on VPS
ssh root@82.24.195.32 "source ~/.cargo/env && \
  cd /root/nitro-fig && \
  cargo build --release"

# 3. Start
ssh root@82.24.195.32 "bash /root/nitro-fig/start.sh"
```

## Start Script

`/root/nitro-fig/start.sh`:

```bash
#!/bin/bash
cd /root/nitro-fig
pkill -f "target/release/bot" 2>/dev/null
sleep 1
export DRY_RUN=true
export BANKROLL=1000
export MAX_EXPOSURE_FRAC=0.15
export ORACLE_DELTA_S=2.0
export EWMA_LAMBDA=0.94
export SIGMA_FLOOR_ANNUAL=0.30
export BINANCE_WS=wss://stream.binance.com:9443/ws/btcusdt@trade
export TELEGRAM_BOT_TOKEN=<your-token>
export TELEGRAM_CHAT_ID=<your-chat-id>
# Strategy toggles (all enabled by default except cross-timeframe)
# export STRAT_LATENCY_ARB=false       # disable latency arb
# export STRAT_CERTAINTY_CAPTURE=false  # disable certainty capture
# export STRAT_CONVEXITY_FADE=false     # disable convexity fade
# export STRAT_STRIKE_MISALIGN=false    # disable strike misalign
# export STRAT_LP_EXTREME=false         # disable LP extreme
# export STRAT_CROSS_TF=true            # enable cross-timeframe (needs feed)
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
[ENGINE] Running market btc-updown-5m-1771291200 | strike=$68938 | window=300s | bankroll=$1000 | ewma_n=198
[ENGINE] Warmup complete: sigma_real=0.00009092 ...
[DIAG] t_left=291s sigma=0.00009092 z=0.00 dist=$0 regime=Ambiguous(73%/251) house=None ...
[DIAG]   certainty_capture: z_abs=0.00 ... -> z<1.5
[DIAG]   convexity_fade: regime=Ambiguous ... -> PASS(regime+dist)
[SIG] strike_misalign Down edge=0.078 fair=0.597 mkt=0.519 sz=$20.0 ACTIVE
[FILL] #1 [strike_misalign] Down Filled price=Some(0.519) size=Some(20.0) lat=0.0ms
[SIG] convexity_fade Down edge=0.080 fair=0.690 mkt=0.610 sz=$5.0 ACTIVE
[FILL] #3 [convexity_fade] Down Filled price=Some(0.61) size=Some(5.0) lat=0.0ms
[ENGINE] Market btc-updown-5m-1771291200 ended | outcome=Down | house=Some(Down) | sig=4186 ord=12 fill=12 pnl=$58.04
[ENGINE]   strike_misalign: sig=94 ord=2 fill=2 pnl=$19.42 avg_edge=0.082
[ENGINE]   latency_arb: sig=1440 ord=4 fill=4 pnl=$28.02 avg_edge=0.042
[ENGINE]   convexity_fade: sig=2652 ord=6 fill=6 pnl=$10.60 avg_edge=0.053
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

## Notes

- **Binance WS geo-blocking**: `stream.binance.com` is blocked from US IPs. The EU VPS works fine.
- **Strike price**: Uses first Binance spot price. Polymarket uses Chainlink oracle (small difference handled by `ORACLE_DELTA_S`).
- **Market cycling**: Bot runs indefinitely, auto-discovering next market. No cron needed.
- **Build on VPS**: Cross-compiling from macOS aarch64 to Linux x86_64 requires a toolchain. Build directly on VPS.
- **start.sh is server-only**: The rsync excludes `start.sh` to avoid overwriting it. Edit directly on the server.
- **EWMA persistence**: The bot must stay running across markets for EWMA to accumulate. Restarting resets to cold start (10s warmup).
- **Replay TUI**: Requires `ratatui`/`crossterm`. Build with `cargo build --release --bin replay`. Only works locally (not over SSH without proper terminal forwarding).
