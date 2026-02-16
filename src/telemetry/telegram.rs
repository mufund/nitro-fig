use crate::types::*;

/// Telegram Bot API client. Persistent connection pool via reqwest.
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

    async fn send(&self, text: &str) {
        let _ = self
            .client
            .post(&self.url)
            .json(&serde_json::json!({
                "chat_id": &self.chat_id,
                "text": text,
                "parse_mode": "Markdown",
            }))
            .send()
            .await;
    }

    pub async fn send_signal_alert(&self, s: &SignalRecord) {
        let side_str = match s.side {
            Side::Up => "🟢 UP",
            Side::Down => "🔴 DOWN",
        };
        let text = format!(
            "⚡ *{}* signal: {}\n\
             Edge: {:.1}¢ | Fair: {:.2} | Mkt: {:.2}\n\
             {}: ${:.0} | Dist: ${:.0}\n\
             Time: {:.0}s left | eval: {}μs",
            s.strategy, side_str,
            s.edge * 100.0, s.fair_value, s.market_price,
            self.asset_label, s.binance_price, s.distance,
            s.time_left_s, s.eval_latency_us,
        );
        self.send(&text).await;
    }

    pub async fn send_order_alert(&self, o: &OrderRecord) {
        let side_str = match o.side {
            Side::Up => "🟢 UP",
            Side::Down => "🔴 DOWN",
        };
        let text = format!(
            "📦 ORDER #{}: {} @ ${:.2} × ${:.2}\n\
             Strategy: {} | Edge: {:.1}¢\n\
             {}: ${:.0} | Time: {:.0}s left",
            o.order_id, side_str, o.price, o.size,
            o.strategy, o.edge_at_submit * 100.0,
            self.asset_label, o.binance_price, o.time_left_s,
        );
        self.send(&text).await;
    }

    pub async fn send_fill_alert(&self, f: &FillRecord) {
        let status_emoji = if f.status.contains("Filled") { "✅" } else { "❌" };
        let text = format!(
            "{} FILL #{}: {} price={} size={} ({:.1}ms)\n\
             PnL if correct: {}",
            status_emoji, f.order_id, f.status,
            f.filled_price.map_or("n/a".to_string(), |p| format!("${:.2}", p)),
            f.filled_size.map_or("n/a".to_string(), |s| format!("${:.2}", s)),
            f.submit_to_ack_ms,
            f.pnl_if_correct.map_or("n/a".to_string(), |p| format!("${:.2}", p)),
        );
        self.send(&text).await;
    }

    pub async fn send_market_start(&self, m: &MarketStartRecord) {
        let text = format!(
            "🏁 Market started: `{}`\n\
             Strike: ${:.0} | Window: {}s",
            m.slug, m.strike, (m.end_ms - m.start_ms) / 1000,
        );
        self.send(&text).await;
    }

    pub async fn send_market_summary(&self, m: &MarketEndRecord) {
        let outcome_str = match m.outcome {
            Side::Up => "🟢 UP",
            Side::Down => "🔴 DOWN",
        };
        let mut text = format!(
            "🏆 Market ended: `{}`\n\
             Outcome: {} | Final dist: ${:.0}\n\
             Signals: {} | Orders: {} | Filled: {}\n\
             Gross PnL: ${:.2}",
            m.slug, outcome_str, m.final_distance,
            m.total_signals, m.total_orders, m.total_filled,
            m.gross_pnl,
        );

        // Per-strategy breakdown
        if !m.per_strategy.is_empty() {
            text.push_str("\n\n📊 *Per Strategy:*");
            for ps in &m.per_strategy {
                text.push_str(&format!(
                    "\n  `{}`: sig={} ord={} fill={} pnl=${:.2}",
                    ps.strategy, ps.signals, ps.orders, ps.filled, ps.gross_pnl,
                ));
            }
        }

        self.send(&text).await;
    }
}
