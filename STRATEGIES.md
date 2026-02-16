# Strategies

Three stateless strategies evaluate the same `MarketState` on every Polymarket quote. All implement `Strategy::evaluate(&MarketState, now_ms) -> Option<Signal>`. The best signal (highest `edge * confidence`) wins and goes to the risk manager.

All dollar thresholds are expressed as **fraction-of-strike** so they scale automatically to any asset price. The fractions were calibrated from BTC at ~$68,000.

## How Polymarket Binary Markets Work

Each market is a binary outcome: will the asset's price be **above** (Up) or **below** (Down) the strike at expiry? You can buy Up or Down tokens priced between $0.00 and $1.00. If correct, the token pays $1.00. If wrong, it pays $0.00.

**Edge** = our estimated fair value minus the market's ask price. If we think Up is worth $0.85 but the market asks $0.72, edge = $0.13.

---

## S1: Distance Fade

**File**: `strategies/distance_fade.rs`

### Concept

If Binance spot is clearly above or below the strike, compute a fair value for the winning side using a sigmoid function. Buy when Polymarket lags behind the Binance-implied probability.

### When Active

Entire market window. No time restriction.

### Signal Logic

1. **Distance check**: `|binance_price - strike| / strike` must exceed `MIN_DIST_FRAC` (0.000441). At BTC $68k, this equals ~$30. At SOL $150, this equals ~$0.07.

2. **Fair value** (sigmoid + time boost):
   ```
   dist_frac = (binance_price - strike) / strike
   base = 0.5 + 0.4 * tanh(dist_frac / SIGMOID_SCALE_FRAC)
   time_boost = (1 - time_left_fraction) * 0.15 * sign(dist_frac)
   fair_up = clamp(base + time_boost, 0.02, 0.98)
   ```
   - The sigmoid maps distance to probability smoothly — small distances give ~50/50, large distances give ~90/10
   - The time boost increases conviction as expiry approaches (same distance is more meaningful with less time left)
   - `SIGMOID_SCALE_FRAC` = 0.001176 (calibrated: $80/$68k). Controls how quickly the sigmoid saturates

3. **Side selection**: If distance > 0, buy Up at `fair_up`. If distance < 0, buy Down at `1 - fair_up`.

4. **Edge filter**: `fair - market_ask` must be at least 8 cents.

5. **Confidence**: Proportional to distance magnitude: `|dist_frac| / CONFIDENCE_NORM_FRAC`, clamped to [0.3, 1.0]. `CONFIDENCE_NORM_FRAC` = 0.001471 (calibrated: $100/$68k).

6. **Sizing**: Half-Kelly capped at 8% of bankroll.

### Threshold Reference

| Parameter | Fraction | At BTC $68k | At ETH $3.5k | At SOL $150 |
|---|---|---|---|---|
| Min distance | 0.000441 | $30 | $1.54 | $0.066 |
| Sigmoid scale | 0.001176 | $80 | $4.12 | $0.176 |
| Confidence norm | 0.001471 | $100 | $5.15 | $0.221 |

### Why It Works

Polymarket order books react slowly to Binance moves. The CLOB has ~1-5 second latency on price updates. During volatile moments, Binance can move $50+ while Polymarket asks stay stale. This strategy captures that lag.

---

## S2: Momentum

**File**: `strategies/momentum.rs`

### Concept

Detect directional momentum from Binance's 30-second rolling trade flow (velocity + net volume). When momentum is strong, Polymarket tends to be priced too low on the momentum side because order book makers pull liquidity slowly.

### When Active

Start of market to 85% of the window elapsed (`time_left_fraction > 0.15`). Disabled in the last ~15% because momentum becomes less predictive near settlement — the market transitions from momentum-driven to distance-driven.

### Signal Logic

1. **Buffer check**: Need at least 50 trades in the 30s rolling buffer.

2. **Velocity** (fractional):
   ```
   tick_velocity_frac = (current_price - oldest_price) / span_seconds / strike
   ```
   Must exceed `MIN_VELOCITY_FRAC` (0.00000735). At BTC $68k = $0.50/sec.

3. **Agreement check**: Velocity direction, net volume direction, and distance from strike must all agree:
   - Up signal: velocity > 0, net_volume > 0, dist_frac > MIN_DIST_FRAC
   - Down signal: velocity < 0, net_volume < 0, dist_frac < -MIN_DIST_FRAC

4. **Polymarket check**: Ask must be below $0.60 (strategy only fires when PM hasn't caught up).

5. **Fair value**: Fixed at $0.65 (momentum implies the side will likely win, but not with high certainty).

6. **Edge filter**: `0.65 - market_ask` must be at least 5 cents.

7. **Confidence**: Based on trade intensity (trades/second), clamped to [0.3, 0.8].

8. **Sizing**: Half-Kelly capped at 5% of bankroll (conservative — momentum can reverse).

### Threshold Reference

| Parameter | Fraction | At BTC $68k | At ETH $3.5k | At SOL $150 |
|---|---|---|---|---|
| Min velocity | 0.00000735/s | $0.50/s | $0.026/s | $0.0011/s |
| Min distance | 0.000294 | $20 | $1.03 | $0.044 |

### Why It Works

When a large Binance move happens (e.g. a $100 BTC spike in 10 seconds), Polymarket market makers take 5-30 seconds to adjust. The first few seconds after a momentum event are when the most mispricing occurs. By the time PM catches up, the opportunity is gone.

---

## S3: Settlement Sniper

**File**: `strategies/settlement_sniper.rs`

### Concept

In the last 90 seconds before settlement, if Binance is clearly on one side of the strike, the outcome probability approaches 1.0. This strategy uses a lookup table of fair values based on distance magnitude and time remaining. Highest conviction strategy.

### When Active

T-90s to T-5s only. This is the "endgame" — most of the position-building should happen here.

### Signal Logic

1. **Time window**: Only active when 5s < time_left < 90s.

2. **Distance check**: `|dist_frac|` must exceed `MIN_DIST_FRAC` (0.000588 = $40 at BTC $68k).

3. **Reversal check**: Compute 30s velocity. If price is moving against the current distance direction faster than `REVERSAL_VELOCITY_FRAC` (0.0000294/s = $2.0/s at BTC $68k), skip — a reversal may be underway.

4. **Fair value lookup table** (distance fraction vs time remaining):

   | Distance | T < 30s | T < 60s | T < 90s |
   |---|---|---|---|
   | High (>0.001471) | 0.97 | 0.95 | 0.93 |
   | Mid (>0.000735) | 0.95 | 0.92 | 0.88 |
   | Low (>0.000588) | 0.90 | — | 0.85 |

   At BTC $68k, "High" = >$100, "Mid" = >$50, "Low" = >$40.

5. **Edge filter**: `fair - market_ask` must be at least 5 cents.

6. **Confidence**: Fixed at 0.95 (highest of all strategies — near-settlement distance is the strongest signal).

7. **Sizing**: Half-Kelly capped at 10% of bankroll (largest position size — highest conviction).

### Threshold Reference

| Parameter | Fraction | At BTC $68k | At ETH $3.5k | At SOL $150 |
|---|---|---|---|---|
| Min distance (low) | 0.000588 | $40 | $2.06 | $0.088 |
| Mid distance | 0.000735 | $50 | $2.57 | $0.110 |
| High distance | 0.001471 | $100 | $5.15 | $0.221 |
| Reversal velocity | 0.0000294/s | $2.0/s | $0.103/s | $0.0044/s |

### Why It Works

With 30 seconds left and BTC $100 above strike, the probability of the price crashing back below strike is extremely low. But Polymarket order books often still show Up at $0.85-0.90 because of slow liquidity provision. Buying at $0.90 when fair value is $0.97 gives a safe 7.8% return in 30 seconds.

The reversal check protects against the rare case where price is rapidly moving back toward strike (e.g. a flash crash in the final minute).

---

## Signal Selection

When multiple strategies fire simultaneously, the engine selects the one with the highest `edge * confidence` score. This naturally prioritizes:
- Settlement sniper (high edge, 0.95 confidence) over distance fade (moderate edge, variable confidence)
- High-edge signals over low-edge signals at the same confidence

## Position Sizing (Half-Kelly)

All strategies use Half-Kelly criterion for position sizing:

```
full_kelly = edge / (1 - price)
half_kelly = full_kelly * 0.5
size_frac = min(half_kelly, strategy_cap)
```

Half-Kelly is more conservative than full Kelly, reducing variance at the cost of slightly lower expected growth. Each strategy has its own cap:

| Strategy | Max Size (% of bankroll) |
|---|---|
| Distance Fade | 8% |
| Momentum | 5% |
| Settlement Sniper | 10% |

The risk manager further caps the total position per market at `MAX_POSITION_USD` (default $100).

## Fraction-of-Strike Scaling

All thresholds are expressed as `value / strike_price`. This means the same fractional thresholds work across all assets:

- BTC at $68,000: a $40 move is 0.000588 of strike
- ETH at $3,500: a $2.06 move is 0.000588 of strike
- SOL at $150: a $0.088 move is 0.000588 of strike

All three are equivalently meaningful moves relative to the asset's price, triggering the same strategy logic at the same fractional distance.
