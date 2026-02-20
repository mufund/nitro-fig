//! Auto-redeem resolved Polymarket positions.
//!
//! Queries the Data API for redeemable positions, then calls the CTF contract
//! to burn winning tokens and recover USDC.e. Designed to run via cron every
//! 30 minutes — silent when there's nothing to redeem.
//!
//! Usage:
//!   cargo run --release --bin auto-redeem
//!
//! Cron:
//!   */30 * * * * cd /root/nitro-fig && ./target/release/auto-redeem >> logs/redeem.log 2>&1
//!
//! Reads from .env:
//!   POLYMARKET_PRIVATE_KEY   — wallet private key (required)
//!   TELEGRAM_BOT_TOKEN       — for alerts (optional)
//!   TELEGRAM_CHAT_ID         — for alerts (optional)

use std::collections::HashSet;
use std::str::FromStr;

use alloy::primitives::B256;
use alloy::providers::ProviderBuilder;
use alloy::signers::Signer as _;
use alloy::signers::local::LocalSigner;
use polymarket_client_sdk::ctf::Client as CtfClient;
use polymarket_client_sdk::ctf::types::RedeemPositionsRequest;
use polymarket_client_sdk::data::Client as DataClient;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::types::address;
use polymarket_client_sdk::POLYGON;

const RPC_URL: &str = "https://polygon-bor-rpc.publicnode.com";
const USDC: alloy::primitives::Address = address!("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174");

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");

    let pk = std::env::var("POLYMARKET_PRIVATE_KEY")
        .expect("Set POLYMARKET_PRIVATE_KEY in .env");
    let signer = LocalSigner::from_str(&pk)
        .expect("Invalid private key")
        .with_chain_id(Some(POLYGON));
    let wallet_addr = signer.address();

    // ── Query Data API for redeemable positions ──
    let data_client = DataClient::default();
    let request = PositionsRequest::builder()
        .user(wallet_addr)
        .redeemable(true)
        .build();

    let positions = match data_client.positions(&request).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[{now}] Data API error: {e}");
            std::process::exit(1);
        }
    };

    if positions.is_empty() {
        eprintln!("[{now}] No redeemable positions");
        return;
    }

    eprintln!(
        "[{now}] Found {} redeemable position(s) across {} market(s)",
        positions.len(),
        positions.iter().map(|p| p.condition_id).collect::<HashSet<_>>().len(),
    );

    // ── Build CTF client with wallet for on-chain tx ──
    let provider = ProviderBuilder::new()
        .wallet(signer)
        .connect(RPC_URL)
        .await
        .expect("Failed to connect to Polygon RPC");

    let ctf = CtfClient::new(provider.clone(), POLYGON)
        .expect("Failed to create CTF client");

    let ctf_neg = CtfClient::with_neg_risk(provider, POLYGON)
        .expect("Failed to create NegRisk CTF client");

    // ── Deduplicate by condition_id ──
    // Multiple positions (Up + Down) share the same condition_id.
    // CTF redeemPositions with index_sets [1, 2] redeems both outcomes in one tx.
    let mut redeemed: HashSet<B256> = HashSet::new();
    let mut total_redeemed = 0u32;

    for pos in &positions {
        if redeemed.contains(&pos.condition_id) {
            continue;
        }
        redeemed.insert(pos.condition_id);

        let label = format!("{} | {}", pos.title, pos.outcome);
        eprintln!("  Redeeming: {} (neg_risk={})", label, pos.negative_risk);

        let result = if pos.negative_risk {
            // NegRisk: need to specify amounts. Use U256::MAX to redeem all.
            // Actually, the standard binary redeem also works for neg_risk conditions
            // if we use the adapter. Let's use the adapter's redeemPositions.
            use polymarket_client_sdk::ctf::types::RedeemNegRiskRequest;
            use alloy::primitives::U256;
            let req = RedeemNegRiskRequest::builder()
                .condition_id(pos.condition_id)
                .amounts(vec![U256::MAX, U256::MAX])
                .build();
            ctf_neg.redeem_neg_risk(&req).await
                .map(|r| (r.transaction_hash, r.block_number))
        } else {
            let req = RedeemPositionsRequest::for_binary_market(USDC, pos.condition_id);
            ctf.redeem_positions(&req).await
                .map(|r| (r.transaction_hash, r.block_number))
        };

        match result {
            Ok((tx_hash, block)) => {
                total_redeemed += 1;
                eprintln!(
                    "    tx: {} (block {})",
                    tx_hash, block,
                );
                send_tg_alert(&pos.title, &pos.outcome, &format!("{tx_hash}")).await;
            }
            Err(e) => {
                eprintln!("    FAILED: {e}");
            }
        }
    }

    eprintln!("[{now}] Done — redeemed {total_redeemed} market(s)");
}

async fn send_tg_alert(title: &str, outcome: &str, tx_hash: &str) {
    let token = match std::env::var("TELEGRAM_BOT_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        _ => return,
    };
    let chat_id = match std::env::var("TELEGRAM_CHAT_ID") {
        Ok(c) if !c.is_empty() => c,
        _ => return,
    };

    let text = format!(
        "💰 *Redeemed*\n{}\nOutcome: {}\n[View tx](https://polygonscan.com/tx/{})",
        title, outcome, tx_hash,
    );

    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let body = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "Markdown",
        "disable_web_page_preview": true,
    });

    let client = reqwest::Client::new();
    if let Err(e) = client.post(&url).json(&body).send().await {
        eprintln!("    TG alert failed: {e}");
    }
}
