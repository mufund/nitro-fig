use crate::types::*;

/// Telegram Bot API client. Persistent connection pool via reqwest.
#[derive(Clone)]
pub struct TelegramClient {
    client: reqwest::Client,
    url: String,
    chat_id: String,
    asset_label: String,
}

impl TelegramClient {
    pub fn new(bot_token: &str, chat_id: &str, asset_label: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            url: format!("https://api.telegram.org/bot{}/sendMessage", bot_token),
            chat_id: chat_id.to_string(),
            asset_label: asset_label.to_string(),
        }
    }

    /// Send with HTML parse mode (for formatted messages).
    async fn send_html(&self, text: &str) {
        match self
            .client
            .post(&self.url)
            .json(&serde_json::json!({
                "chat_id": &self.chat_id,
                "text": text,
                "parse_mode": "HTML",
            }))
            .send()
            .await
        {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    eprintln!("[TG] Send failed: {} — {}", status, body);
                }
            }
            Err(e) => {
                eprintln!("[TG] Request error: {}", e);
            }
        }
    }

    /// Send plain text (no parse mode — safe for all characters).
    async fn send_plain(&self, text: &str) {
        match self
            .client
            .post(&self.url)
            .json(&serde_json::json!({
                "chat_id": &self.chat_id,
                "text": text,
            }))
            .send()
            .await
        {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    eprintln!("[TG] Send failed: {} — {}", status, body);
                }
            }
            Err(e) => {
                eprintln!("[TG] Request error: {}", e);
            }
        }
    }

    pub async fn send_signal_alert(&self, s: &SignalRecord) {
        let side_str = match s.side {
            Side::Up => "🟢 UP",
            Side::Down => "🔴 DOWN",
        };
        let text = format!(
            "⚡ {} signal: {}\n\
             Edge: {:.1}¢ | Fair: {:.2} | Mkt: {:.2}\n\
             {}: ${:.0} | Dist: ${:.0}\n\
             Time: {:.0}s left | eval: {}μs",
            s.strategy, side_str,
            s.edge * 100.0, s.fair_value, s.market_price,
            self.asset_label, s.binance_price, s.distance,
            s.time_left_s, s.eval_latency_us,
        );
        self.send_plain(&text).await;
    }

    pub async fn send_order_alert(&self, o: &OrderRecord) {
        let side_str = match o.side {
            Side::Up => "🟢 UP",
            Side::Down => "🔴 DOWN",
        };
        let text = format!(
            "📦 ORDER #{}: {} @ ${:.2} x ${:.2}\n\
             Strategy: {} | Edge: {:.1}¢\n\
             {}: ${:.0} | Time: {:.0}s left",
            o.order_id, side_str, o.price, o.size,
            o.strategy, o.edge_at_submit * 100.0,
            self.asset_label, o.binance_price, o.time_left_s,
        );
        self.send_plain(&text).await;
    }

    pub async fn send_fill_alert(&self, f: &FillRecord) {
        let status_emoji = if f.status.contains("Filled") { "✅" } else { "❌" };
        let side_str = match f.side {
            Side::Up => "🟢 UP",
            Side::Down => "🔴 DOWN",
        };
        let text = format!(
            "{} FILL #{}: {} {} price={} size={} ({:.1}ms)\n\
             Strategy: {} | PnL if correct: {}",
            status_emoji, f.order_id, side_str, f.status,
            f.filled_price.map_or("n/a".to_string(), |p| format!("${:.2}", p)),
            f.filled_size.map_or("n/a".to_string(), |s| format!("${:.2}", s)),
            f.submit_to_ack_ms,
            f.strategy,
            f.pnl_if_correct.map_or("n/a".to_string(), |p| format!("${:.2}", p)),
        );
        self.send_plain(&text).await;
    }

    pub async fn send_market_start(&self, m: &MarketStartRecord) {
        let text = format!(
            "🏁 Market started: <code>{}</code>\n\
             Strike: ${:.0} | Window: {}s",
            m.slug, m.strike, (m.end_ms - m.start_ms) / 1000,
        );
        self.send_html(&text).await;
    }

    pub async fn send_strategy_metrics(&self, sm: &StrategyMetricsRecord) {
        let text = format!(
            "📊 Strategy: {}\n\
             Fills: {} | Rate: {:.1}% | WR: {:.1}%\n\
             Adv sel: {:.3} | Avg edge: {:.4}",
            sm.strategy, sm.fill_count, sm.fill_rate * 100.0,
            sm.win_rate * 100.0, sm.adverse_selection, sm.avg_edge,
        );
        self.send_plain(&text).await;
    }

    pub async fn send_rejection_alert(&self, order_id: u64, strategy: &str, reason: &str) {
        let text = format!(
            "⛔ ORDER #{} REJECTED (local)\n\
             Strategy: {} | Reason: {}",
            order_id, strategy, reason,
        );
        self.send_plain(&text).await;
    }

    pub async fn send_market_summary(&self, m: &MarketEndRecord) {
        let outcome_str = match m.outcome {
            Side::Up => "🟢 UP",
            Side::Down => "🔴 DOWN",
        };
        let mut text = format!(
            "🏆 Market ended: <code>{}</code>\n\
             Outcome: {} | Final dist: ${:.0}\n\
             Signals: {} | Orders: {} | Filled: {}\n\
             Gross PnL: ${:.2}",
            m.slug, outcome_str, m.final_distance,
            m.total_signals, m.total_orders, m.total_filled,
            m.gross_pnl,
        );

        // Per-strategy breakdown
        if !m.per_strategy.is_empty() {
            text.push_str("\n\n📊 <b>Per Strategy:</b>");
            for ps in &m.per_strategy {
                text.push_str(&format!(
                    "\n  <code>{}</code>: sig={} ord={} fill={} pnl=${:.2}",
                    ps.strategy, ps.signals, ps.orders, ps.filled, ps.gross_pnl,
                ));
            }
        }

        self.send_html(&text).await;
    }
}
