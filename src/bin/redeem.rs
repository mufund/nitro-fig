//! Redeem resolved Polymarket positions back to USDC.
//!
//! Calls the CTF contract's `redeemPositions` for a given condition_id.
//! The condition is already resolved on-chain — this just burns winning tokens
//! and credits USDC.e back to the wallet.
//!
//! Usage:
//!   cargo run --release --bin redeem -- <condition_id_hex>
//!
//! Example:
//!   cargo run --release --bin redeem -- 0x571a3c90918ed50c6df079e880f40f73f4877b1b0636996e8bd2508b4d8e7ca3
//!
//! Reads POLYMARKET_PRIVATE_KEY from .env or environment.

use std::str::FromStr;

use alloy::primitives::B256;
use alloy::providers::ProviderBuilder;
use alloy::signers::Signer as _;
use alloy::signers::local::LocalSigner;
use polymarket_client_sdk::ctf::Client as CtfClient;
use polymarket_client_sdk::ctf::types::RedeemPositionsRequest;
use polymarket_client_sdk::types::address;
use polymarket_client_sdk::POLYGON;

const RPC_URL: &str = "https://polygon-bor-rpc.publicnode.com";
const USDC: alloy::primitives::Address = address!("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174");

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: redeem <condition_id_hex>");
        eprintln!("  e.g. redeem 0x571a3c90...");
        std::process::exit(1);
    }

    let condition_id = B256::from_str(&args[1])
        .expect("Invalid condition_id hex (must be 0x-prefixed 32-byte hex)");

    let pk = std::env::var("POLYMARKET_PRIVATE_KEY")
        .expect("Set POLYMARKET_PRIVATE_KEY in .env");
    let signer = LocalSigner::from_str(&pk)
        .expect("Invalid private key")
        .with_chain_id(Some(POLYGON));

    let wallet_addr = signer.address();
    eprintln!("Wallet: {}", wallet_addr);
    eprintln!("Condition: {}", condition_id);

    let provider = ProviderBuilder::new()
        .wallet(signer)
        .connect(RPC_URL)
        .await
        .expect("Failed to connect to Polygon RPC");

    let ctf = CtfClient::new(provider, POLYGON)
        .expect("Failed to create CTF client");

    let request = RedeemPositionsRequest::for_binary_market(USDC, condition_id);

    eprintln!("Sending redeem tx...");
    match ctf.redeem_positions(&request).await {
        Ok(resp) => {
            eprintln!("Redeemed!");
            eprintln!("  tx:    {}", resp.transaction_hash);
            eprintln!("  block: {}", resp.block_number);
            eprintln!(
                "  view:  https://polygonscan.com/tx/{}",
                resp.transaction_hash
            );
        }
        Err(e) => {
            eprintln!("Redeem failed: {}", e);
            std::process::exit(1);
        }
    }
}
