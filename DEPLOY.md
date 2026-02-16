# Deployment

## VPS

- **IP**: 82.24.195.32
- **User**: `root`
- **OS**: Ubuntu (x86_64)
- **Rust**: 1.93.1+ installed under `~/.cargo/env`

## Paths

| What | Path |
|------|------|
| Source + binary | `/root/polymarket-btc/` |
| Release binary | `/root/polymarket-btc/target/release/bot` |
| Market logs | `/root/polymarket-btc/logs/{interval}/{slug}/` |
| Bot output log | `/root/bot_tg.log` |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `ASSET` | `btc` | Asset to trade: `btc`, `eth`, `sol`, `xrp` |
| `INTERVAL` | `5m` | Market interval: `5m`, `15m`, `1h`, `4h` |
| `DRY_RUN` | `true` | Simulate fills (no real orders). Set `false` for live trading |
| `BINANCE_WS` | auto-derived | Binance trade stream. Auto: `wss://stream.binance.com:9443/ws/{asset}usdt@trade` |
| `BINANCE_WS_FALLBACK` | auto-derived | Binance US fallback. Auto: `wss://stream.binance.us:9443/ws/{asset}usd@trade` |
| `PM_CLOB_WS` | `wss://ws-subscriptions-clob.polymarket.com/ws/market` | Polymarket CLOB WebSocket |
| `GAMMA_API_URL` | `https://gamma-api.polymarket.com` | Gamma API for market discovery |
| `SERIES_ID` | auto-derived | Series ID for market discovery. Auto-derived from ASSET+INTERVAL |
| `MAX_POSITION_USD` | `100` | Max total exposure per market |
| `MAX_ORDERS_PER_MARKET` | `10` | Max orders per market window |
| `COOLDOWN_MS` | `5000` | Min milliseconds between orders |
| `TELEGRAM_BOT_TOKEN` | _(empty)_ | Telegram Bot API token for alerts |
| `TELEGRAM_CHAT_ID` | _(empty)_ | Telegram chat ID for alerts |

**Auto-derived values**: `BINANCE_WS`, `BINANCE_WS_FALLBACK`, and `SERIES_ID` are automatically computed from `ASSET` + `INTERVAL`. You only need to set them explicitly to override the defaults.

## Quick Deploy (from local machine)

```bash
# 1. Sync source to VPS
rsync -avz --exclude target --exclude .git \
  /Users/keon/dev/PolymarketBTC15mAssistant/rust/ \
  root@82.24.195.32:/root/polymarket-btc/

# 2. Build on VPS
ssh root@82.24.195.32 "source ~/.cargo/env && \
  cd /root/polymarket-btc && \
  cargo build --release"
```

## Running

### BTC 5m (default)

```bash
DRY_RUN=true \
TELEGRAM_BOT_TOKEN=<your-bot-token> \
TELEGRAM_CHAT_ID=<your-chat-id> \
nohup ./target/release/bot > /root/bot_tg.log 2>&1 &
```

### BTC 15m

```bash
ASSET=btc INTERVAL=15m \
DRY_RUN=true \
TELEGRAM_BOT_TOKEN=<your-bot-token> \
TELEGRAM_CHAT_ID=<your-chat-id> \
nohup ./target/release/bot > /root/bot_btc15m.log 2>&1 &
```

### ETH 15m

```bash
ASSET=eth INTERVAL=15m \
DRY_RUN=true \
TELEGRAM_BOT_TOKEN=<your-bot-token> \
TELEGRAM_CHAT_ID=<your-chat-id> \
nohup ./target/release/bot > /root/bot_eth15m.log 2>&1 &
```

### SOL 15m

```bash
ASSET=sol INTERVAL=15m \
DRY_RUN=true \
TELEGRAM_BOT_TOKEN=<your-bot-token> \
TELEGRAM_CHAT_ID=<your-chat-id> \
nohup ./target/release/bot > /root/bot_sol15m.log 2>&1 &
```

### BTC 4h

```bash
ASSET=btc INTERVAL=4h \
DRY_RUN=true \
TELEGRAM_BOT_TOKEN=<your-bot-token> \
TELEGRAM_CHAT_ID=<your-chat-id> \
nohup ./target/release/bot > /root/bot_btc4h.log 2>&1 &
```

### Running Multiple Instances

You can run multiple asset/interval combinations simultaneously. Each instance writes to its own log directory (`logs/{interval}/{slug}/`):

```bash
# BTC 5m + ETH 15m + SOL 15m simultaneously
ASSET=btc INTERVAL=5m  DRY_RUN=true nohup ./target/release/bot > /root/bot_btc5m.log 2>&1 &
ASSET=eth INTERVAL=15m DRY_RUN=true nohup ./target/release/bot > /root/bot_eth15m.log 2>&1 &
ASSET=sol INTERVAL=15m DRY_RUN=true nohup ./target/release/bot > /root/bot_sol15m.log 2>&1 &
```

## Monitoring

```bash
ssh root@82.24.195.32

# Live bot output
tail -f /root/bot_tg.log

# Check if running
ps aux | grep bot

# Stop all bot instances
pkill -f "target/release/bot"

# View latest market signals (any interval)
ls -lt /root/polymarket-btc/logs/15m/ | head -5
tail -20 /root/polymarket-btc/logs/15m/*/signals.csv

# View latest orders
tail -20 /root/polymarket-btc/logs/15m/*/orders.csv
```

## Test WebSocket Connectivity

```bash
# Test BTC (default)
ssh root@82.24.195.32 "cd /root/polymarket-btc && ./target/release/ws_test"

# Test ETH
ssh root@82.24.195.32 "cd /root/polymarket-btc && ASSET=eth INTERVAL=15m ./target/release/ws_test"
```

## Output Files (per market)

Each market creates a directory under `logs/{interval}/{slug}/`:

```
logs/15m/btc-updown-15m-1771226700/
├── signals.csv      # Every strategy signal (thousands per market)
├── latency.csv      # binance_recv, pm_recv, eval, e2e timing (us)
├── orders.csv       # Order attempts with strategy, edge, price, size
├── fills.csv        # Fill results with latency, theoretical PnL
└── market_info.txt  # slug, strike, start_ms, end_ms
```

## Notes

- **Binance WS geo-blocking**: Binance global endpoint (`stream.binance.com`) is blocked from US IPs (HTTP 451). The EU VPS works fine. If deploying from US, use `stream.binance.us` via `BINANCE_WS` env var.
- **Strike price**: Currently uses first Binance spot price as strike. Polymarket uses Chainlink oracle price (small difference). This is a known limitation.
- **Market cycling**: The bot runs indefinitely, auto-discovering the next market after each one ends. No cron needed.
- **Build on VPS**: Cross-compiling from macOS aarch64 to Linux x86_64 requires a toolchain. Build directly on the VPS instead.
- **Log rotation**: Signal CSVs accumulate. Clean up old log directories periodically.
- **1h market discovery**: 1h markets use human-readable slugs, not `btc-updown-1h-{ts}`. Discovery falls back to series_id search automatically.
