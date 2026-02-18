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
}

// ─── Active tab in the TUI ───

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Summary,
    Strategies,
    Markets,
    Trades,
    Equity,
}

impl Tab {
    pub fn all() -> &'static [Tab] {
        &[Tab::Summary, Tab::Strategies, Tab::Markets, Tab::Trades, Tab::Equity]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Tab::Summary => "Summary",
            Tab::Strategies => "Strategies",
            Tab::Markets => "Markets",
            Tab::Trades => "Trades",
            Tab::Equity => "Equity",
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
}
