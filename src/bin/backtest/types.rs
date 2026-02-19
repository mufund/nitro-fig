use std::collections::HashMap;

use polymarket_crypto::types::Side;

// ─── Per-trade record (what the replay produces) ───

#[derive(Clone)]
pub struct TradeRecord {
    pub market_idx: usize,
    pub order_id: u64,
    pub strategy: String,
    pub side: Side,
    pub price: f64,
    pub size: f64,
    pub edge: f64,
    pub fair_value: f64,
    pub confidence: f64,
    pub time_left_s: f64,
    pub is_passive: bool,
    pub btc_price: f64,
    pub strike: f64,
    // Settlement
    pub outcome: Option<Side>,
    pub pnl: f64,
    pub won: bool,
}

// ─── Per-market result ───

#[derive(Clone)]
pub struct MarketResult {
    pub dir_name: String,
    pub slug: String,
    pub strike: f64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub final_price: f64,
    pub final_distance: f64,
    pub outcome: Side,
    pub n_events: usize,
    pub trades: Vec<TradeRecord>,
    pub total_pnl: f64,
    pub total_invested: f64,
}

// ─── Per-strategy aggregate stats ───

#[derive(Clone, Default)]
pub struct StrategyStats {
    pub n_signals: u32,
    pub n_orders: u32,
    pub n_wins: u32,
    pub n_losses: u32,
    pub total_pnl: f64,
    pub total_invested: f64,
    pub total_edge: f64,
    pub max_win: f64,
    pub max_loss: f64,
    pub avg_time_left_s: f64,
    pub time_left_sum: f64,
    pub pnl_history: Vec<f64>, // cumulative pnl after each trade
    // Per-trade PnL (non-cumulative) for advanced metrics
    pub trade_pnls: Vec<f64>,
    pub trade_edges: Vec<f64>,
    pub trade_confidences: Vec<f64>,
    pub trade_prices: Vec<f64>,
    pub trade_sizes: Vec<f64>,
    pub trade_time_lefts: Vec<f64>,
    pub n_passive: u32,
    pub n_active: u32,
}

impl StrategyStats {
    pub fn win_rate(&self) -> f64 {
        let total = self.n_wins + self.n_losses;
        if total == 0 { 0.0 } else { self.n_wins as f64 / total as f64 }
    }

    pub fn avg_edge(&self) -> f64 {
        if self.n_orders == 0 { 0.0 } else { self.total_edge / self.n_orders as f64 }
    }

    pub fn roi(&self) -> f64 {
        if self.total_invested == 0.0 { 0.0 } else { self.total_pnl / self.total_invested * 100.0 }
    }

    pub fn profit_factor(&self) -> f64 {
        let gross_wins: f64 = self.pnl_history.iter()
            .zip(std::iter::once(&0.0f64).chain(self.pnl_history.iter()))
            .map(|(curr, prev)| (curr - prev).max(0.0))
            .sum();
        let gross_losses: f64 = self.pnl_history.iter()
            .zip(std::iter::once(&0.0f64).chain(self.pnl_history.iter()))
            .map(|(curr, prev)| (curr - prev).min(0.0).abs())
            .sum();
        if gross_losses == 0.0 { f64::INFINITY } else { gross_wins / gross_losses }
    }

    pub fn max_drawdown(&self) -> f64 {
        let mut peak = 0.0f64;
        let mut max_dd = 0.0f64;
        for &val in &self.pnl_history {
            peak = peak.max(val);
            max_dd = max_dd.max(peak - val);
        }
        max_dd
    }

    /// Sortino ratio: mean / downside deviation
    pub fn sortino(&self) -> f64 {
        if self.trade_pnls.len() < 2 { return 0.0; }
        let mean = self.trade_pnls.iter().sum::<f64>() / self.trade_pnls.len() as f64;
        let downside_var: f64 = self.trade_pnls.iter()
            .map(|r| r.min(0.0).powi(2))
            .sum::<f64>() / self.trade_pnls.len() as f64;
        let downside_dev = downside_var.sqrt();
        if downside_dev == 0.0 { 0.0 } else { mean / downside_dev }
    }

    /// Average confidence on trades
    pub fn avg_confidence(&self) -> f64 {
        if self.trade_confidences.is_empty() { 0.0 }
        else { self.trade_confidences.iter().sum::<f64>() / self.trade_confidences.len() as f64 }
    }

    /// Average fill price
    pub fn avg_price(&self) -> f64 {
        if self.trade_prices.is_empty() { 0.0 }
        else { self.trade_prices.iter().sum::<f64>() / self.trade_prices.len() as f64 }
    }

    /// Average trade size
    pub fn avg_size(&self) -> f64 {
        if self.trade_sizes.is_empty() { 0.0 }
        else { self.trade_sizes.iter().sum::<f64>() / self.trade_sizes.len() as f64 }
    }

    /// Edge standard deviation
    pub fn edge_std(&self) -> f64 {
        if self.trade_edges.len() < 2 { return 0.0; }
        let mean = self.avg_edge();
        let var = self.trade_edges.iter().map(|e| (e - mean).powi(2)).sum::<f64>()
            / (self.trade_edges.len() - 1) as f64;
        var.sqrt()
    }

    /// Max consecutive wins
    pub fn max_consecutive_wins(&self) -> u32 {
        let mut max_streak = 0u32;
        let mut streak = 0u32;
        for &pnl in &self.trade_pnls {
            if pnl > 0.0 { streak += 1; max_streak = max_streak.max(streak); }
            else { streak = 0; }
        }
        max_streak
    }

    /// Max consecutive losses
    pub fn max_consecutive_losses(&self) -> u32 {
        let mut max_streak = 0u32;
        let mut streak = 0u32;
        for &pnl in &self.trade_pnls {
            if pnl <= 0.0 { streak += 1; max_streak = max_streak.max(streak); }
            else { streak = 0; }
        }
        max_streak
    }
}

// ─── Drawdown entry for tracking ───

#[derive(Clone)]
pub struct DrawdownEntry {
    pub trade_idx: usize,
    pub drawdown: f64,
    pub peak: f64,
    pub duration: usize, // trades since peak
}

// ─── Time bucket for timing analysis ───

#[derive(Clone)]
pub struct TimeBucket {
    pub label: &'static str,
    pub lo_s: f64,
    pub hi_s: f64,
    pub n_trades: u32,
    pub n_wins: u32,
    pub total_pnl: f64,
    pub avg_edge: f64,
    pub avg_confidence: f64,
    pub avg_size: f64,
}

// ─── Active tab in the TUI ───

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Summary,
    Strategies,
    Markets,
    Trades,
    Equity,
    Risk,
    Timing,
    Correlation,
}

impl Tab {
    pub fn all() -> &'static [Tab] {
        &[Tab::Summary, Tab::Strategies, Tab::Markets, Tab::Trades, Tab::Equity, Tab::Risk, Tab::Timing, Tab::Correlation]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Tab::Summary => "Summary",
            Tab::Strategies => "Strategies",
            Tab::Markets => "Markets",
            Tab::Trades => "Trades",
            Tab::Equity => "Equity",
            Tab::Risk => "Risk",
            Tab::Timing => "Timing",
            Tab::Correlation => "Correl",
        }
    }

    pub fn next(&self) -> Tab {
        let tabs = Self::all();
        let idx = tabs.iter().position(|t| t == self).unwrap_or(0);
        tabs[(idx + 1) % tabs.len()]
    }

    pub fn prev(&self) -> Tab {
        let tabs = Self::all();
        let idx = tabs.iter().position(|t| t == self).unwrap_or(0);
        tabs[(idx + tabs.len() - 1) % tabs.len()]
    }
}

// ─── App state ───

pub struct BacktestApp {
    pub tab: Tab,
    pub markets: Vec<MarketResult>,
    pub all_trades: Vec<TradeRecord>,
    pub strategy_stats: HashMap<String, StrategyStats>,
    pub equity_curve: Vec<(f64, f64)>, // (market_index, cumulative_pnl)
    pub total_pnl: f64,
    pub total_invested: f64,
    pub n_wins: u32,
    pub n_losses: u32,
    pub bankroll: f64,

    // Scroll states
    pub market_scroll: usize,
    pub trade_scroll: usize,
    pub strategy_scroll: usize,

    // Filter: which strategy to show in trades tab (None = all)
    pub trade_filter: Option<String>,
    pub strategy_names: Vec<String>,
    pub filter_idx: usize, // 0 = "All", 1..N = strategy names

    // Market drill-down: Some(market_idx) when viewing a single market's details
    pub market_drill_down: Option<usize>,

    // ── Advanced analytics (computed once at init) ──

    // Per-trade equity curve (indexed by trade number, not market)
    pub trade_equity_curve: Vec<(f64, f64)>,

    // Drawdown series
    pub drawdown_curve: Vec<DrawdownEntry>,

    // PnL distribution histogram (edge → count) bucketed
    pub pnl_histogram: Vec<(f64, u32)>, // (bucket_center, count)
    pub edge_histogram: Vec<(f64, u32)>,

    // Rolling metrics (computed per trade)
    pub rolling_sharpe: Vec<(f64, f64)>,    // (trade_idx, rolling_sharpe over 20-trade window)
    pub rolling_win_rate: Vec<(f64, f64)>,  // (trade_idx, rolling_wr over 20-trade window)

    // Time bucket analysis
    pub time_buckets: Vec<TimeBucket>,

    // Strategy correlation matrix (strategy_idx -> strategy_idx -> correlation)
    pub strategy_correlations: Vec<Vec<f64>>,
    pub correlation_names: Vec<String>,

    // Risk metrics
    pub sortino_ratio: f64,
    pub calmar_ratio: f64,
    pub max_drawdown_duration: usize, // longest number of trades in drawdown
    pub max_consecutive_wins: u32,
    pub max_consecutive_losses: u32,
    pub recovery_factor: f64, // total_pnl / max_drawdown
    pub expectancy: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub payoff_ratio: f64, // avg_win / abs(avg_loss)
    pub kelly_fraction: f64,

    // Outcome analysis
    pub up_outcomes: usize,
    pub dn_outcomes: usize,
    pub up_trades: usize,
    pub dn_trades: usize,
    pub up_pnl: f64,
    pub dn_pnl: f64,

    // Edge analysis per strategy
    pub edge_realized: HashMap<String, f64>, // avg realized PnL per trade
    pub edge_predicted: HashMap<String, f64>, // avg predicted edge per trade
}

impl BacktestApp {
    pub fn new(markets: Vec<MarketResult>, bankroll: f64) -> Self {
        let mut all_trades: Vec<TradeRecord> = Vec::new();
        let mut strategy_stats: HashMap<String, StrategyStats> = HashMap::new();
        let mut equity_curve: Vec<(f64, f64)> = Vec::new();
        let mut cumulative_pnl = 0.0;
        let mut total_invested = 0.0;
        let mut n_wins = 0u32;
        let mut n_losses = 0u32;

        equity_curve.push((0.0, 0.0));

        for (mi, market) in markets.iter().enumerate() {
            for trade in &market.trades {
                // Update per-strategy stats
                let stats = strategy_stats.entry(trade.strategy.clone()).or_default();
                stats.n_orders += 1;
                stats.total_invested += trade.size;
                stats.total_edge += trade.edge;
                stats.time_left_sum += trade.time_left_s;
                stats.trade_pnls.push(trade.pnl);
                stats.trade_edges.push(trade.edge);
                stats.trade_confidences.push(trade.confidence);
                stats.trade_prices.push(trade.price);
                stats.trade_sizes.push(trade.size);
                stats.trade_time_lefts.push(trade.time_left_s);
                if trade.is_passive { stats.n_passive += 1; } else { stats.n_active += 1; }
                total_invested += trade.size;

                if trade.won {
                    stats.n_wins += 1;
                    stats.max_win = stats.max_win.max(trade.pnl);
                    n_wins += 1;
                } else {
                    stats.n_losses += 1;
                    stats.max_loss = stats.max_loss.min(trade.pnl);
                    n_losses += 1;
                }
                stats.total_pnl += trade.pnl;
                stats.pnl_history.push(stats.total_pnl);

                all_trades.push(trade.clone());
            }
            cumulative_pnl += market.total_pnl;
            equity_curve.push(((mi + 1) as f64, cumulative_pnl));
        }

        // Compute avg_time_left_s
        for stats in strategy_stats.values_mut() {
            if stats.n_orders > 0 {
                stats.avg_time_left_s = stats.time_left_sum / stats.n_orders as f64;
            }
        }

        let total_pnl = cumulative_pnl;

        let mut strategy_names: Vec<String> = strategy_stats.keys().cloned().collect();
        strategy_names.sort();

        // ── Per-trade equity curve ──
        let mut trade_equity_curve = Vec::with_capacity(all_trades.len() + 1);
        trade_equity_curve.push((0.0, 0.0));
        let mut cum = 0.0;
        for (i, t) in all_trades.iter().enumerate() {
            cum += t.pnl;
            trade_equity_curve.push(((i + 1) as f64, cum));
        }

        // ── Drawdown curve ──
        let mut drawdown_curve = Vec::new();
        let mut peak = 0.0f64;
        let mut peak_idx = 0usize;
        for (i, &(_, cum_pnl)) in trade_equity_curve.iter().enumerate().skip(1) {
            if cum_pnl > peak {
                peak = cum_pnl;
                peak_idx = i;
            }
            drawdown_curve.push(DrawdownEntry {
                trade_idx: i,
                drawdown: peak - cum_pnl,
                peak,
                duration: i - peak_idx,
            });
        }

        // ── Max drawdown duration ──
        let max_drawdown_duration = drawdown_curve.iter()
            .map(|d| d.duration).max().unwrap_or(0);

        // ── PnL histogram ──
        let pnl_histogram = Self::compute_histogram(
            &all_trades.iter().map(|t| t.pnl).collect::<Vec<_>>(), 20);

        // ── Edge histogram ──
        let edge_histogram = Self::compute_histogram(
            &all_trades.iter().map(|t| t.edge).collect::<Vec<_>>(), 20);

        // ── Rolling metrics (window = 20 trades) ──
        let window = 20usize;
        let mut rolling_sharpe = Vec::new();
        let mut rolling_win_rate = Vec::new();
        for i in window..=all_trades.len() {
            let slice = &all_trades[i - window..i];
            let pnls: Vec<f64> = slice.iter().map(|t| t.pnl).collect();
            let mean = pnls.iter().sum::<f64>() / pnls.len() as f64;
            let var = pnls.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / (pnls.len() - 1) as f64;
            let std = var.sqrt();
            let sr = if std > 0.0 { mean / std } else { 0.0 };
            let wr = slice.iter().filter(|t| t.won).count() as f64 / slice.len() as f64;
            rolling_sharpe.push((i as f64, sr));
            rolling_win_rate.push((i as f64, wr * 100.0));
        }

        // ── Time bucket analysis ──
        let bucket_defs: &[(&str, f64, f64)] = &[
            ("0-15s",   0.0,   15.0),
            ("15-30s",  15.0,  30.0),
            ("30-60s",  30.0,  60.0),
            ("1-2min",  60.0,  120.0),
            ("2-3min",  120.0, 180.0),
            ("3-5min",  180.0, 300.0),
            ("5-10min", 300.0, 600.0),
            ("10min+",  600.0, 99999.0),
        ];
        let time_buckets: Vec<TimeBucket> = bucket_defs.iter().map(|&(label, lo, hi)| {
            let trades: Vec<&TradeRecord> = all_trades.iter()
                .filter(|t| t.time_left_s >= lo && t.time_left_s < hi).collect();
            let n = trades.len() as u32;
            let wins = trades.iter().filter(|t| t.won).count() as u32;
            let pnl: f64 = trades.iter().map(|t| t.pnl).sum();
            let avg_e = if n > 0 { trades.iter().map(|t| t.edge).sum::<f64>() / n as f64 } else { 0.0 };
            let avg_c = if n > 0 { trades.iter().map(|t| t.confidence).sum::<f64>() / n as f64 } else { 0.0 };
            let avg_s = if n > 0 { trades.iter().map(|t| t.size).sum::<f64>() / n as f64 } else { 0.0 };
            TimeBucket { label, lo_s: lo, hi_s: hi, n_trades: n, n_wins: wins, total_pnl: pnl,
                avg_edge: avg_e, avg_confidence: avg_c, avg_size: avg_s }
        }).collect();

        // ── Strategy correlation matrix ──
        let (strategy_correlations, correlation_names) = Self::compute_correlations(&markets, &strategy_names);

        // ── Risk metrics ──
        let trade_pnls: Vec<f64> = all_trades.iter().map(|t| t.pnl).collect();
        let sortino_ratio = Self::compute_sortino(&trade_pnls);
        let mdd = {
            let mut peak = 0.0f64;
            let mut max_dd = 0.0f64;
            let mut c = 0.0;
            for &pnl in &trade_pnls {
                c += pnl;
                peak = peak.max(c);
                max_dd = max_dd.max(peak - c);
            }
            max_dd
        };
        let calmar_ratio = if mdd > 0.0 { total_pnl / mdd } else { 0.0 };
        let recovery_factor = if mdd > 0.0 { total_pnl / mdd } else { 0.0 };

        let (max_consecutive_wins, max_consecutive_losses) = Self::compute_streaks(&trade_pnls);

        let wins_pnl: f64 = all_trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).sum();
        let loss_pnl: f64 = all_trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl).sum();
        let avg_win = if n_wins > 0 { wins_pnl / n_wins as f64 } else { 0.0 };
        let avg_loss = if n_losses > 0 { loss_pnl / n_losses as f64 } else { 0.0 };
        let payoff_ratio = if avg_loss != 0.0 { avg_win / avg_loss.abs() } else { 0.0 };
        let expectancy = if all_trades.is_empty() { 0.0 } else { total_pnl / all_trades.len() as f64 };

        let wr = if n_wins + n_losses > 0 { n_wins as f64 / (n_wins + n_losses) as f64 } else { 0.0 };
        let kelly_fraction = if payoff_ratio > 0.0 {
            (wr * (payoff_ratio + 1.0) - 1.0) / payoff_ratio
        } else { 0.0 };

        // ── Outcome analysis ──
        let up_outcomes = markets.iter().filter(|m| m.outcome == Side::Up).count();
        let dn_outcomes = markets.len() - up_outcomes;
        let up_trades = all_trades.iter().filter(|t| t.side == Side::Up).count();
        let dn_trades = all_trades.iter().filter(|t| t.side == Side::Down).count();
        let up_pnl: f64 = all_trades.iter().filter(|t| t.side == Side::Up).map(|t| t.pnl).sum();
        let dn_pnl: f64 = all_trades.iter().filter(|t| t.side == Side::Down).map(|t| t.pnl).sum();

        // ── Edge analysis per strategy ──
        let mut edge_realized: HashMap<String, f64> = HashMap::new();
        let mut edge_predicted: HashMap<String, f64> = HashMap::new();
        for name in &strategy_names {
            let stats = &strategy_stats[name];
            if stats.n_orders > 0 {
                edge_realized.insert(name.clone(), stats.total_pnl / stats.n_orders as f64);
                edge_predicted.insert(name.clone(), stats.avg_edge());
            }
        }

        BacktestApp {
            tab: Tab::Summary,
            markets,
            all_trades,
            strategy_stats,
            equity_curve,
            total_pnl,
            total_invested,
            n_wins,
            n_losses,
            bankroll,
            market_scroll: 0,
            trade_scroll: 0,
            strategy_scroll: 0,
            trade_filter: None,
            strategy_names,
            filter_idx: 0,
            market_drill_down: None,
            trade_equity_curve,
            drawdown_curve,
            pnl_histogram,
            edge_histogram,
            rolling_sharpe,
            rolling_win_rate,
            time_buckets,
            strategy_correlations,
            correlation_names,
            sortino_ratio,
            calmar_ratio,
            max_drawdown_duration,
            max_consecutive_wins,
            max_consecutive_losses,
            recovery_factor,
            expectancy,
            avg_win,
            avg_loss,
            payoff_ratio,
            kelly_fraction,
            up_outcomes,
            dn_outcomes,
            up_trades,
            dn_trades,
            up_pnl,
            dn_pnl,
            edge_realized,
            edge_predicted,
        }
    }

    pub fn filtered_trades(&self) -> Vec<&TradeRecord> {
        match &self.trade_filter {
            None => self.all_trades.iter().collect(),
            Some(name) => self.all_trades.iter().filter(|t| &t.strategy == name).collect(),
        }
    }

    pub fn win_rate(&self) -> f64 {
        let total = self.n_wins + self.n_losses;
        if total == 0 { 0.0 } else { self.n_wins as f64 / total as f64 }
    }

    pub fn roi(&self) -> f64 {
        if self.total_invested == 0.0 { 0.0 } else { self.total_pnl / self.total_invested * 100.0 }
    }

    pub fn max_drawdown(&self) -> f64 {
        let mut peak = 0.0f64;
        let mut max_dd = 0.0f64;
        for &(_, pnl) in &self.equity_curve {
            peak = peak.max(pnl);
            max_dd = max_dd.max(peak - pnl);
        }
        max_dd
    }

    pub fn sharpe_ratio(&self) -> f64 {
        if self.markets.len() < 2 { return 0.0; }
        let returns: Vec<f64> = self.markets.iter().map(|m| m.total_pnl).collect();
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
        let std_dev = variance.sqrt();
        if std_dev == 0.0 { 0.0 } else { mean / std_dev }
    }

    pub fn profit_factor(&self) -> f64 {
        let gross_wins: f64 = self.all_trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).sum();
        let gross_losses: f64 = self.all_trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl.abs()).sum();
        if gross_losses == 0.0 { f64::INFINITY } else { gross_wins / gross_losses }
    }

    pub fn cycle_filter_forward(&mut self) {
        // 0 = All, 1..N = strategy names
        let max = self.strategy_names.len();
        self.filter_idx = (self.filter_idx + 1) % (max + 1);
        self.trade_filter = if self.filter_idx == 0 {
            None
        } else {
            Some(self.strategy_names[self.filter_idx - 1].clone())
        };
        self.trade_scroll = 0;
    }

    // ── Private helpers ──

    fn compute_histogram(values: &[f64], n_bins: usize) -> Vec<(f64, u32)> {
        if values.is_empty() { return vec![]; }
        let min_v = values.iter().fold(f64::MAX, |a, &b| a.min(b));
        let max_v = values.iter().fold(f64::MIN, |a, &b| a.max(b));
        let range = max_v - min_v;
        if range == 0.0 { return vec![(min_v, values.len() as u32)]; }
        let bin_width = range / n_bins as f64;

        let mut bins = vec![0u32; n_bins];
        for &v in values {
            let idx = ((v - min_v) / bin_width) as usize;
            let idx = idx.min(n_bins - 1);
            bins[idx] += 1;
        }

        bins.iter().enumerate()
            .map(|(i, &count)| (min_v + (i as f64 + 0.5) * bin_width, count))
            .collect()
    }

    fn compute_sortino(returns: &[f64]) -> f64 {
        if returns.len() < 2 { return 0.0; }
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let downside_var: f64 = returns.iter()
            .map(|r| r.min(0.0).powi(2))
            .sum::<f64>() / returns.len() as f64;
        let downside_dev = downside_var.sqrt();
        if downside_dev == 0.0 { 0.0 } else { mean / downside_dev }
    }

    fn compute_streaks(pnls: &[f64]) -> (u32, u32) {
        let mut max_w = 0u32;
        let mut max_l = 0u32;
        let mut w = 0u32;
        let mut l = 0u32;
        for &pnl in pnls {
            if pnl > 0.0 { w += 1; l = 0; max_w = max_w.max(w); }
            else { l += 1; w = 0; max_l = max_l.max(l); }
        }
        (max_w, max_l)
    }

    fn compute_correlations(markets: &[MarketResult], strategy_names: &[String]) -> (Vec<Vec<f64>>, Vec<String>) {
        let names: Vec<String> = strategy_names.to_vec();
        let n = names.len();
        if n == 0 { return (vec![], names); }

        // Build per-strategy per-market PnL vectors
        let mut pnl_matrix: Vec<Vec<f64>> = vec![vec![0.0; markets.len()]; n];
        for (mi, market) in markets.iter().enumerate() {
            for trade in &market.trades {
                if let Some(si) = names.iter().position(|s| s == &trade.strategy) {
                    pnl_matrix[si][mi] += trade.pnl;
                }
            }
        }

        // Compute correlation matrix
        let mut corr = vec![vec![0.0f64; n]; n];
        for i in 0..n {
            for j in 0..n {
                corr[i][j] = Self::pearson_correlation(&pnl_matrix[i], &pnl_matrix[j]);
            }
        }

        (corr, names)
    }

    fn pearson_correlation(x: &[f64], y: &[f64]) -> f64 {
        let n = x.len() as f64;
        if n < 2.0 { return 0.0; }
        let mean_x = x.iter().sum::<f64>() / n;
        let mean_y = y.iter().sum::<f64>() / n;
        let mut cov = 0.0f64;
        let mut var_x = 0.0f64;
        let mut var_y = 0.0f64;
        for i in 0..x.len() {
            let dx = x[i] - mean_x;
            let dy = y[i] - mean_y;
            cov += dx * dy;
            var_x += dx * dx;
            var_y += dy * dy;
        }
        let denom = (var_x * var_y).sqrt();
        if denom == 0.0 { 0.0 } else { cov / denom }
    }
}
