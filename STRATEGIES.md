# Strategies

Six stateless strategies evaluate a shared `MarketState` and produce `Signal` values. Each implements `Strategy::evaluate(&MarketState, now_ms) -> Option<Signal>`. All passing signals are dispatched through the risk manager simultaneously (no "best signal wins" — every signal that clears risk gets an order).

Each strategy can be individually enabled/disabled via environment variables (see [Configuration](#configuration) below). Five are active by default; `cross_timeframe` is disabled because no cross-market data feed is wired yet.

All six strategies can be visualized in the [replay TUI](README.md#replay-tui) — fair value dots for each strategy are shown on the Polymarket YES/NO charts, color-coded: LA=yellow, CC=cyan, CF=magenta, CT=blue, SM=red, LP=green.

## How Polymarket Binary Markets Work

Each market is a 5-minute window: will BTC be **above** (Up) or **below** (Down) the strike price at expiry? You buy Up or Down tokens priced $0.00-$1.00. If correct, the token pays $1.00. If wrong, it pays $0.00.

**Edge** = our model's fair value minus the market's ask price. If we compute P(Up)=0.85 but Polymarket asks $0.72, edge = $0.13.

**Key quantities used across all strategies:**

| Symbol | Definition | Formula |
|--------|-----------|---------|
| S | Oracle-adjusted BTC estimate | `binance_price + beta` |
| K | Strike price (set at market open) | First Binance price when market starts |
| sigma | Realized vol (per-second) | 1-second sampled EWMA, floored at 30% annualized |
| tau | Effective time to expiry (seconds) | `time_left + delta_oracle` (floor 0.001s) |
| z | Certainty score | `ln(S/K) / (sigma * sqrt(tau))` |
| d2 | Black-Scholes d2 | `[ln(S/K) - sigma^2*tau/2] / (sigma*sqrt(tau))` |
| P_fair | Model fair probability of Up | `Phi(d2)` (standard normal CDF) |

---

## S1: Latency Arbitrage

**File**: `strategies/latency_arb.rs`
**Trigger**: Every Binance trade
**Type**: Active (sets house view)

### Concept

Polymarket's CLOB quotes lag Binance by 1-5 seconds. When BTC moves on Binance, our model immediately computes the new fair binary probability. If Polymarket's stale quotes still offer mispricing, we hit them before market makers update.

This is the workhorse strategy. It fires on every Binance trade (~100/sec), evaluates in <1us, and is the first to detect any BTC move.

### Mechanism Step-by-Step

1. **Compute model fair value**: Using the latest Binance price S, strike K, realized vol sigma, and time to expiry tau:
   ```
   d2 = [ln(S/K) - sigma^2 * tau / 2] / (sigma * sqrt(tau))
   P_fair(Up) = Phi(d2)
   P_fair(Down) = 1 - P_fair(Up)
   ```

2. **Scan all four directions for edge**:
   - Buy Up: edge = `P_fair(Up) - up_ask`
   - Buy Down: edge = `P_fair(Down) - down_ask`
   - (Sell Up and Sell Down are computed but reserved for future short-selling)

3. **Select best direction**: Whichever side has the highest edge wins.

4. **Gate**: Edge must exceed 3 cents (`MIN_EDGE = 0.03`).

5. **Size**: Half-Kelly capped at 2% of bankroll per trade:
   ```
   kelly = edge / (1 - price)
   size_frac = min(kelly * 0.5, 0.02)
   ```

6. **Confidence**: Proportional to edge magnitude: `(edge / 0.10)`, clamped [0.3, 1.0].

### Why 3 Cents?

The minimum edge of 3 cents accounts for Polymarket's fee structure. Below this threshold, execution costs eat the edge. The strategy fires extremely often (hundreds of signals per market), so the risk manager's cooldown (200ms) and per-market order cap (50) prevent overtrading.

### Risk Limits

| Parameter | Value |
|-----------|-------|
| Per-trade size cap | 2% of bankroll |
| Total exposure cap | 8% of bankroll |
| Cooldown | 200ms |
| Max orders per market | 50 |

---

## S2: Certainty Capture

**File**: `strategies/certainty_capture.rs`
**Trigger**: Every Polymarket quote
**Type**: Active (sets house view)

### Concept

When BTC is far from the strike with time running out, the binary outcome becomes near-deterministic. If BTC is $130 above strike with 60 seconds left, the probability of it crashing back below is minuscule. But Polymarket's order book often still prices Up at $0.88-0.93 because liquidity providers are slow to push prices to their theoretical limits. This strategy captures the gap between near-certainty and the market's residual doubt.

### Mechanism Step-by-Step

1. **Compute z-score**: Measures how many "standard deviations" BTC is from the strike:
   ```
   z = ln(S/K) / (sigma * sqrt(tau))
   ```
   At typical vol (sigma ~0.00006/s) with 60s left, z=1.5 corresponds to BTC being ~$130 from strike.

2. **Gate on |z| >= 1.5**: If z is below this threshold, the outcome isn't certain enough. This filters out the noisy near-ATM region where binary prices should be close to 50/50.

3. **Select side**:
   - z > 0: S > K, Up is the near-certain side. Fair = P_fair(Up), ask = up_ask.
   - z < 0: S < K, Down is the near-certain side. Fair = 1 - P_fair(Up), ask = down_ask.

4. **Edge gate**: `fair - market_ask >= 0.02` (2 cents minimum).

5. **Tiered sizing based on z-score**:
   ```
   |z| > 3.0  -> max 5% of bankroll (near-certain, max conviction)
   |z| > 2.5  -> max 3% of bankroll
   |z| > 1.5  -> max 1% of bankroll
   ```
   This naturally increases position size as the outcome becomes more deterministic.

6. **Confidence**: `(|z| / 4.0)`, clamped [0.5, 0.99]. At z=3.0, confidence=0.75.

### Mathematical Intuition

The z-score is the signal-to-noise ratio: how large is the log-moneyness relative to the expected random walk. At z=2, there's only a ~2.3% chance the price reverses enough to cross the strike. At z=3, it's ~0.1%. The strategy exploits the fact that Polymarket participants don't price this extreme asymmetry correctly.

### Why Not Use d2 Directly?

The z-score intentionally omits the drift term (`-sigma^2*tau/2`) that appears in d2. For these short time horizons (seconds to minutes), the drift contribution is negligible (~10^-6). The simpler z-score is more intuitive: positive z means S > K, negative means S < K. The P_fair calculation still uses the full d2 formula for precision.

### Risk Limits

| Parameter | Value |
|-----------|-------|
| Per-trade size cap | 5% of bankroll (tiered) |
| Total exposure cap | 10% of bankroll |
| Cooldown | 1000ms |
| Max orders per market | 15 |

---

## S3: Convexity Fade

**File**: `strategies/convexity_fade.rs`
**Trigger**: Every Polymarket quote
**Type**: Active (sets house view)

### Concept

When BTC is near the strike (ATM), the binary option has maximum gamma (curvature). Small BTC oscillations of $10-30 cause large probability swings of 5-15 cents. In range-bound conditions, Polymarket participants tend to overreact to these swings, pushing prices away from the model's fair value. This strategy fades those overreactions.

### The Convexity Effect

Binary option delta peaks at ATM:
```
delta = phi(d2) / (S * sigma * sqrt(tau))
```

At ATM with sigma=0.00006/s and tau=200s:
- delta ~ phi(0) / (68000 * 0.00006 * 14.1) = 0.399 / 57.5 = 0.0069 per dollar
- A $20 BTC move causes a ~13.8 cent probability shift

This is enormous. In equity options, a similar ATM gamma causes "pin risk." In binary options, it means the price of Up tokens bounces wildly as BTC oscillates around the strike. The fade strategy bets these bounces will revert.

### Mechanism Step-by-Step

1. **Regime filter**: Only fires when regime is Range or Ambiguous. If Trend, the price moves are likely to continue (adverse selection), so fading is dangerous.
   ```
   regime == Trend -> reject
   ```

2. **Distance filter**: BTC must be within 0.3% of the strike:
   ```
   |S - K| / K <= 0.003
   ```
   At BTC $69,000, this is a $207 window around the strike. Outside this, gamma is too low for the convexity effect.

3. **Minimum tau**: tau >= 30 seconds. Below this, certainty_capture should be handling the trade (near-expiry is a different regime).

4. **Compute fair value and edges for both sides**:
   ```
   fair_up = P_fair(S, K, sigma, tau)
   edge_up = fair_up - up_ask
   edge_down = (1 - fair_up) - down_ask
   ```

5. **Select better side**: Whichever side has higher edge (must be >= 2 cents).

6. **Sizing**: Half-Kelly capped at 0.5% of bankroll — deliberately small because:
   - This is a mean-reversion bet, not a directional conviction
   - High frequency compensates (fires many times per market)
   - Individual trades have lower confidence (0.4 fixed)

### Regime Classifier

The regime is determined by tick direction analysis over a rolling 30-second window:

```
For each Binance trade where price != previous price:
  Record (timestamp, is_up_tick)

dominant_frac = max(up_ticks, down_ticks) / total_ticks

Range:     dominant_frac < 60%  (price chopping both ways)
Ambiguous: 60% <= dominant_frac < 75%
Trend:     dominant_frac >= 75%  (strong directional flow)
```

Only actual price changes are counted. Repeated trades at the same price are noise and ignored.

### Expected Probability Swing

The strategy also computes the expected swing magnitude for diagnostics:
```
E[|dP|] = phi(d2) * sqrt(dt/tau) * sqrt(2/pi)
```
This represents how much the binary price should bounce in the next `dt` seconds. It peaks at ATM and decays away from it.

### Risk Limits

| Parameter | Value |
|-----------|-------|
| Per-trade size cap | 0.5% of bankroll |
| Total exposure cap | 3% of bankroll |
| Cooldown | 2000ms |
| Max orders per market | 20 |

---

## S4: Strike Misalignment

**File**: `strategies/strike_misalign.rs`
**Trigger**: Binance trade (only in first 15 seconds of market)
**Type**: Active (sets house view)

### Concept

The strike K is set from a single Binance price snapshot at market open. Due to microstructure noise (spread, timing, liquidity), this snapshot is often biased relative to the true fair price. The 60-second rolling VWAP provides a better estimate of BTC's "true" level. The difference between VWAP and strike represents a predictable pricing error that the market will correct within the first 10-15 seconds.

### Mechanism Step-by-Step

1. **Time window**: Only active in the first 15 seconds of the market (`elapsed_ms <= 15,000`). After this, the market has priced in the misalignment.

2. **Require VWAP data**: The persistent Binance state tracks a 60-second rolling VWAP. If no data is available yet (fresh start), skip.

3. **Compute strike bias**:
   ```
   epsilon = K - VWAP
   ```
   If epsilon > 0, the strike was set too high (snapshot caught a local spike). If < 0, too low.

4. **Compute probability shift**:
   ```
   sensitivity = phi(d2) / (VWAP * sigma * sqrt(tau))
   dP = -sensitivity * epsilon
   ```
   This is the first-order Taylor expansion of P_fair around the VWAP price. It measures how much the fair probability shifts due to the strike being "wrong."

5. **Gate on |dP| >= 0.02**: The probability shift must be at least 2 percentage points to be worth trading. Below this, the edge is too small after fees.

6. **Side selection**:
   - dP > 0: Up is underpriced (strike was set too high relative to VWAP). Buy Up.
   - dP < 0: Down is underpriced (strike was set too low). Buy Down.

7. **Fair value**: Compute P_fair using VWAP as the reference price (not the biased strike-time snapshot):
   ```
   fair = P_fair(VWAP, K, sigma, tau)  // or 1 - P_fair for Down
   ```

8. **Edge and sizing**: `edge = fair - market_ask`, minimum 2 cents. Half-Kelly capped at 2%.

### Why VWAP?

A single price snapshot is noisy — it could be a local extreme caused by a single large market order. VWAP over 60 seconds smooths this out, representing where BTC is actually trading, not where a single trade printed. The difference between the snapshot and VWAP is mean-reverting and predictable.

### Sensitivity Formula

The formula `phi(d2) / (S * sigma * sqrt(tau))` is the binary option delta. It measures how much the probability changes per dollar of price movement. Near ATM, this sensitivity is highest (phi(0)/... is maximized). Far from ATM, the probability barely changes with small price moves. The strike bias `epsilon` acts as that price movement.

### Risk Limits

| Parameter | Value |
|-----------|-------|
| Per-trade size cap | 2% of bankroll |
| Total exposure cap | 4% of bankroll |
| Cooldown | 500ms |
| Max orders per market | 5 |

---

## S5: Extreme Probability LP

**File**: `strategies/lp_extreme.rs`
**Trigger**: Both Binance trades and Polymarket quotes
**Type**: Passive (exempt from house view)

### Concept

When the binary outcome is near-certain (|z| > 1.5), the losing side's token price collapses to near zero. Market makers retreat because the expected loss per token approaches 100%. But the token isn't worthless — there's still a small probability of a dramatic reversal. If the market prices this tail probability too cheaply, we can buy the losing-side tokens at a discount to their true (tiny) fair value.

This is a classic liquidity provision strategy: provide capital where others won't, earn a premium for bearing tail risk.

### Key Distinction: Passive Signal

This is the only passive strategy (`is_passive = true`). It is **exempt from house_side filtering** — it intentionally buys the opposite side of the house view. If latency_arb sets house=Up, lp_extreme may buy Down tokens at $0.05. This is not a contradiction: the house is betting Up wins (pays $1), while the LP position is betting the Down tokens are worth more than the $0.05 we paid (they're worth $0.08 by the model).

### Mechanism Step-by-Step

1. **Volatility check**: sigma must be positive (EWMA must be warmed up).

2. **Minimum tau**: tau >= 60 seconds. Too close to expiry and the outcome is too certain — there's no residual value in the losing side.

3. **Regime filter**: Not Trend. During trends, the price is moving directionally and the "losing" side could get even cheaper (adverse selection). We only LP when the market is range-bound or ambiguous.

4. **Compute z-score**: Same as certainty_capture:
   ```
   z = ln(S/K) / (sigma * sqrt(tau))
   ```

5. **Gate on |z| >= 1.5**: The outcome must be sufficiently lopsided. Below this, both sides have reasonable probability and there's no extreme-LP opportunity.

6. **Identify losing side and market price**:
   - z > 0 (Up winning): Buy Down tokens at `down_ask`
   - z < 0 (Down winning): Buy Up tokens at `up_ask`

7. **Price gate**: The losing side's ask must be < $0.25 (extreme price territory). Above this, it's not cheap enough to be an LP opportunity.

8. **Compute true probability of losing side**:
   ```
   If z > 0: true_prob = 1 - P_fair(S, K, sigma, tau)  // P(Down wins)
   If z < 0: true_prob = P_fair(S, K, sigma, tau)       // P(Up wins)
   ```

9. **Edge**: `true_prob - market_ask`. Must be >= 2 cents. This is positive when the market underprices the tail probability.

10. **Kelly sizing for binary payoff**:
    ```
    p_winning = 1 - true_prob  // probability of the WINNING side
    a = market_ask              // our buy price for the losing side
    f* = true_prob - p_winning * (1 - a) / a
    size_frac = (f* * 0.5), clamped [0.001, 0.02]
    ```
    This is the optimal Kelly fraction for a binary bet where we buy at price `a` and either receive $1 (with probability `true_prob`) or lose `a` (with probability `1 - true_prob`).

### When Does This Fire?

Rarely. It requires all of:
- |z| >= 1.5 (BTC far from strike)
- Losing side ask < $0.25 (cheap tokens)
- Regime not Trend (stable conditions)
- tau >= 60s (enough time for potential reversal)
- Edge >= 2 cents (positive expected value)

This confluence happens perhaps a few times per hour. The strategy compensates with outsized returns when it hits — buying $0.05 tokens that are worth $0.08 yields 60% return.

### Risk Limits

| Parameter | Value |
|-----------|-------|
| Per-trade size cap | 2% of bankroll |
| Total exposure cap | 5% of bankroll |
| Cooldown | 2000ms |
| Max orders per market | 10 |

---

## S6: Cross-Timeframe Relative Value (Disabled)

**File**: `strategies/cross_timeframe.rs`
**Status**: Code exists but disabled by default (`STRAT_CROSS_TF=false`). No cross-market data feed is wired.

### Concept (Planned)

Extract implied volatility from multiple expiry windows (5m, 15m, 1h). Fit a vol surface `sigma(tau) = a * tau^b` (power law in log-log space). Trade outliers that deviate from the fitted curve. If the 5m market implies 80% vol while the fitted curve predicts 60% for that tenor, the 5m is overpriced.

### Why Disabled

`state.cross_markets` is always empty because no feed connects to markets at other intervals. The strategy self-disables when fewer than 2 cross-market data points are available. Implementing this requires subscribing to Polymarket CLOB feeds for multiple token pairs simultaneously.

---

## Configuration

Each strategy can be individually enabled or disabled via environment variables. The engine conditionally instantiates strategies at startup based on these toggles and logs which are active.

| Env Var | Strategy | Default | Disable with |
|---------|----------|---------|-------------|
| `STRAT_LATENCY_ARB` | S1: Latency Arbitrage | **enabled** | `STRAT_LATENCY_ARB=0` |
| `STRAT_CERTAINTY_CAPTURE` | S2: Certainty Capture | **enabled** | `STRAT_CERTAINTY_CAPTURE=false` |
| `STRAT_CONVEXITY_FADE` | S3: Convexity Fade | **enabled** | `STRAT_CONVEXITY_FADE=0` |
| `STRAT_STRIKE_MISALIGN` | S4: Strike Misalignment | **enabled** | `STRAT_STRIKE_MISALIGN=false` |
| `STRAT_LP_EXTREME` | S5: Extreme Probability LP | **enabled** | `STRAT_LP_EXTREME=0` |
| `STRAT_CROSS_TF` | S6: Cross-Timeframe RV | **disabled** | (enable: `STRAT_CROSS_TF=1`) |

**Active strategies** (S1-S5) default to `true`. Set the env var to `"0"` or `"false"` to disable.

**Cross-timeframe** (S6) defaults to `false`. Set to `"1"` or `"true"` to enable (requires cross-market data feed).

At startup, the engine logs which strategies are active:
```
[ENGINE] Strategies enabled: ["latency_arb", "certainty_capture", "convexity_fade", "strike_misalign", "lp_extreme"]
```

---

## Side Coherence (House View)

The engine enforces directional coherence across active strategies within a single market:

1. **First active order sets the house view**. If latency_arb fires first with side=Down, then `house_side = Down`.

2. **Subsequent active orders must agree**. If convexity_fade wants to buy Up after the house is Down, it's filtered out.

3. **Passive signals are exempt**. lp_extreme can buy the opposite side (that's its purpose — LP on the losing side).

4. **When no house view exists and active signals disagree**, the side with the highest `sum(edge * confidence)` wins. Signals for the losing side are dropped.

### Why?

Without side coherence, the bot could simultaneously bet Up and Down — which guarantees a loss (you'd buy Up at $0.45 and Down at $0.55, paying $1.00 total for a $1.00 payout). The house view prevents this while still allowing the LP strategy to operate independently.

---

## Position Sizing: Half-Kelly

All strategies use the Half-Kelly criterion:

```
full_kelly = edge / (1 - price)
half_kelly = full_kelly * 0.5
size_frac = min(half_kelly, strategy_cap)
size_dollars = size_frac * bankroll
```

Why half-Kelly instead of full Kelly:
- **Estimation error**: Our edge estimates are noisy. Full Kelly is optimal only with perfect information. Half-Kelly sacrifices ~25% expected growth for ~50% variance reduction.
- **Discrete outcomes**: Binary options have only two outcomes ($0 or $1). Kelly assumes continuous compounding. Half-Kelly is more appropriate for lumpy binary payoffs.
- **Fat tails**: BTC can flash crash. Half-Kelly provides a buffer against model misspecification.

Each strategy has its own size cap on top of Half-Kelly:

| Strategy | Max per trade | Max total | Cooldown | Max orders/market |
|----------|--------------|-----------|----------|-------------------|
| latency_arb | 2% | 8% | 200ms | 50 |
| certainty_capture | 5% | 10% | 1s | 15 |
| convexity_fade | 0.5% | 3% | 2s | 20 |
| strike_misalign | 2% | 4% | 500ms | 5 |
| lp_extreme | 2% | 5% | 2s | 10 |

---

## PnL Accounting

PnL is computed at **settlement**, not at fill time:

```
For each fill:
  if fill.side == outcome:
    pnl = (1 - fill.price) * fill.size    // correct: paid X, received $1
  else:
    pnl = -(fill.price * fill.size)        // wrong: paid X, received $0
```

This is the only correct way to account for binary option PnL. Earlier versions incorrectly computed PnL at fill time (which was always positive — you can't lose money buying a token, only at settlement).

---

## Diagnostic Logging

Every 10 seconds, the engine logs a `[DIAG]` block showing:

```
[DIAG] t_left=241s sigma=0.00009092 z=0.00 dist=$0 dist_frac=0.00000 regime=Ambiguous(73%/251) house=None
[DIAG]   certainty_capture: z_abs=0.00 fair=0.500 ask=0.990 edge=-0.490 -> z<1.5
[DIAG]   convexity_fade: regime=Ambiguous dist_frac=0.00000 -> PASS(regime+dist)
[DIAG]   lp_extreme: z_abs=0.00 losing_side=Down ask=0.990 regime=Ambiguous -> z<1.5
[DIAG]   strike_misalign: elapsed=10649ms -> in_window
```

For each strategy, it shows the specific gate condition that blocks it (or PASS if it would fire). The regime line now includes the dominant tick fraction and total tick count (e.g., `Ambiguous(73%/251)` means 73% of 251 direction-changing ticks went the same way).
