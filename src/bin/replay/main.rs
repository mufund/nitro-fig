//! Replay TUI: step through recorded market data tick-by-tick.
//! Visualizes orderbook depth, BTC price, PM quotes, computed metrics, and strategy signals.
//!
//! Usage: cargo run --bin replay -- <data_dir>
//! Keys: [Right/l] step fwd | [Left/h] step back | [Space] play/pause | [+/-] speed
//!       [PgDn/n] +100 | [PgUp/b] -100 | [Home/g] start | [End/G] end | [q/Esc] quit

mod app;
mod loader;
mod render;
mod types;

use std::io::{self, stdout};
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event as CEvent, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;

use polymarket_crypto::engine::risk::StrategyRiskManager;

use crate::types::{App, replay_config};

// ─── Convenience helpers on App used only by the event loop ───

impl App {
    pub fn current_event_ts(&self) -> i64 {
        if self.cursor > 0 && self.cursor <= self.events.len() {
            self.events[self.cursor - 1].ts_ms()
        } else if !self.events.is_empty() {
            self.events[0].ts_ms()
        } else {
            0
        }
    }

    pub fn current_event_label(&self) -> &'static str {
        if self.cursor > 0 && self.cursor <= self.events.len() {
            self.events[self.cursor - 1].type_label()
        } else {
            "---"
        }
    }
}

// ─── Input handling ───

fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    match key {
        KeyEvent { code: KeyCode::Char('q'), .. }
        | KeyEvent { code: KeyCode::Esc, .. } => return true,

        // Step forward
        KeyEvent { code: KeyCode::Right, .. }
        | KeyEvent { code: KeyCode::Char('l'), modifiers: KeyModifiers::NONE, .. } => {
            app.step_forward(1);
        }

        // Step back
        KeyEvent { code: KeyCode::Left, .. }
        | KeyEvent { code: KeyCode::Char('h'), modifiers: KeyModifiers::NONE, .. } => {
            app.step_back(1);
        }

        // Play/Pause
        KeyEvent { code: KeyCode::Char(' '), .. } => {
            app.playing = !app.playing;
        }

        // Speed
        KeyEvent { code: KeyCode::Char('+'), .. }
        | KeyEvent { code: KeyCode::Char('='), .. } => {
            app.speed = (app.speed * 2).min(32);
        }
        KeyEvent { code: KeyCode::Char('-'), .. } => {
            app.speed = (app.speed / 2).max(1);
        }

        // Jump +/-100
        KeyEvent { code: KeyCode::PageDown, .. }
        | KeyEvent { code: KeyCode::Char('n'), modifiers: KeyModifiers::NONE, .. } => {
            app.step_forward(100);
        }
        KeyEvent { code: KeyCode::PageUp, .. }
        | KeyEvent { code: KeyCode::Char('b'), modifiers: KeyModifiers::NONE, .. } => {
            app.step_back(100);
        }

        // Jump to start
        KeyEvent { code: KeyCode::Home, .. }
        | KeyEvent { code: KeyCode::Char('g'), modifiers: KeyModifiers::NONE, .. } => {
            app.signal_log.clear();
            app.order_log.clear();
            app.risk = StrategyRiskManager::new(&replay_config());
            app.house_side = None;
            app.next_order_id = 1;
            app.jump_to(0);
        }

        // Jump to end
        KeyEvent { code: KeyCode::End, .. }
        | KeyEvent { code: KeyCode::Char('G'), .. } => {
            let remaining = app.events.len() - app.cursor;
            app.step_forward(remaining);
        }

        // Export CSV
        KeyEvent { code: KeyCode::Char('s'), modifiers: KeyModifiers::NONE, .. } => {
            app.playing = false;
            let msg = match app.export_csv() {
                Ok(path) => format!("Saved {} events \u{2192} {}", app.cursor, path),
                Err(e) => format!("Export failed: {}", e),
            };
            app.status_msg = Some((msg, Instant::now()));
        }

        _ => {}
    }
    false
}

// ─── Main ───

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let data_dir = if args.len() > 1 {
        &args[1]
    } else {
        eprintln!("Usage: replay <data_dir>");
        eprintln!("  e.g. cargo run --bin replay -- logs/5m/btc-updown-5m-1771320600");
        std::process::exit(1);
    };

    eprintln!("Loading data from {}...", data_dir);

    let binance_trades = loader::load_binance_csv(&format!("{}/binance.csv", data_dir));
    let pm_quotes = loader::load_polymarket_csv(&format!("{}/polymarket.csv", data_dir));
    let book_snapshots = loader::load_book_csv(&format!("{}/book.csv", data_dir));
    let market_info = loader::load_market_info(&format!("{}/market_info.txt", data_dir));

    eprintln!(
        "Loaded {} Binance, {} PM, {} book events",
        binance_trades.len(), pm_quotes.len(), book_snapshots.len()
    );

    let events = loader::merge_events(&binance_trades, &pm_quotes, &book_snapshots);
    eprintln!("Merged {} events", events.len());

    let strike = if market_info.strike > 0.0 {
        market_info.strike
    } else {
        binance_trades.first().map(|t| t.price).unwrap_or(100_000.0)
    };
    eprintln!("Strike: ${:.2}", strike);

    let mut app = App::new(events, market_info, strike, data_dir.to_string());

    eprintln!("Building snapshots...");
    app.build_snapshots();
    eprintln!("Built {} snapshots. Starting TUI...", app.snapshots.len());

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tick_rate = Duration::from_millis(50);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|frame| render::draw(&app, frame))?;

        let timeout = tick_rate.checked_sub(last_tick.elapsed()).unwrap_or(Duration::ZERO);

        if crossterm::event::poll(timeout)? {
            if let CEvent::Key(key) = event::read()? {
                if handle_key(&mut app, key) {
                    break;
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            if app.playing && app.cursor < app.events.len() {
                app.step_forward(app.speed);
            }
            last_tick = Instant::now();
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
