//! One-time USDC.e + CTF approval for Polymarket CLOB trading.
//!
//! Approves all 3 exchange contracts (CTF Exchange, Neg-Risk Exchange, Neg-Risk Adapter)
//! for both ERC-20 (USDC.e collateral) and ERC-1155 (Conditional Tokens).
//!
//! Usage:
//!   cargo run --release --bin approve
//!
//! Reads POLYMARKET_PRIVATE_KEY from .env or environment.

use std::str::FromStr;

use alloy::primitives::U256;
use alloy::providers::ProviderBuilder;
use alloy::signers::Signer as _;
use alloy::signers::local::LocalSigner;
use alloy::sol;
use polymarket_client_sdk::types::{Address, address};
use polymarket_client_sdk::{POLYGON, contract_config};

const RPC_URL: &str = "https://polygon-bor-rpc.publicnode.com";
const USDC_ADDRESS: Address = address!("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174");

sol! {
    #[sol(rpc)]
    interface IERC20 {
        function approve(address spender, uint256 value) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);
    }

    #[sol(rpc)]
    interface IERC1155 {
        function setApprovalForAll(address operator, bool approved) external;
        function isApprovedForAll(address account, address operator) external view returns (bool);
    }
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let pk = std::env::var("POLYMARKET_PRIVATE_KEY")
        .expect("Set POLYMARKET_PRIVATE_KEY in .env");
    let signer = LocalSigner::from_str(&pk)
        .expect("Invalid private key")
        .with_chain_id(Some(POLYGON));

    let owner = signer.address();
    eprintln!("Wallet: {}", owner);

    let provider = ProviderBuilder::new()
        .wallet(signer)
        .connect(RPC_URL)
        .await
        .expect("Failed to connect to Polygon RPC");

    let config = contract_config(POLYGON, false).unwrap();
    let neg_risk_config = contract_config(POLYGON, true).unwrap();

    let mut targets: Vec<(&str, Address)> = vec![
        ("CTF Exchange", config.exchange),
        ("Neg Risk CTF Exchange", neg_risk_config.exchange),
    ];
    if let Some(adapter) = neg_risk_config.neg_risk_adapter {
        targets.push(("Neg Risk Adapter", adapter));
    }

    let token = IERC20::new(USDC_ADDRESS, provider.clone());
    let ctf = IERC1155::new(config.conditional_tokens, provider.clone());

    // ── Check current state ──
    eprintln!("\n=== Current Allowances ===");
    for (name, target) in &targets {
        let allowance = token.allowance(owner, *target).call().await
            .map(|a| format!("{}", a))
            .unwrap_or_else(|e| format!("error: {}", e));
        let ctf_approved = ctf.isApprovedForAll(owner, *target).call().await
            .map(|a| format!("{}", a))
            .unwrap_or_else(|e| format!("error: {}", e));
        eprintln!("  {}: USDC={} CTF={}", name, allowance, ctf_approved);
    }

    // ── Approve ──
    eprintln!("\n=== Approving (6 transactions) ===");
    for (name, target) in &targets {
        eprint!("  {} USDC approve... ", name);
        match token.approve(*target, U256::MAX).send().await {
            Ok(pending) => match pending.watch().await {
                Ok(tx) => eprintln!("tx={}", tx),
                Err(e) => eprintln!("watch error: {}", e),
            },
            Err(e) => eprintln!("send error: {}", e),
        }

        eprint!("  {} CTF setApprovalForAll... ", name);
        match ctf.setApprovalForAll(*target, true).send().await {
            Ok(pending) => match pending.watch().await {
                Ok(tx) => eprintln!("tx={}", tx),
                Err(e) => eprintln!("watch error: {}", e),
            },
            Err(e) => eprintln!("send error: {}", e),
        }
    }

    // ── Verify ──
    eprintln!("\n=== Verified Allowances ===");
    for (name, target) in &targets {
        let allowance = token.allowance(owner, *target).call().await
            .map(|a| format!("{}", a))
            .unwrap_or_else(|e| format!("error: {}", e));
        let ctf_approved = ctf.isApprovedForAll(owner, *target).call().await
            .map(|a| format!("{}", a))
            .unwrap_or_else(|e| format!("error: {}", e));
        eprintln!("  {}: USDC={} CTF={}", name, allowance, ctf_approved);
    }

    eprintln!("\nDone! Wallet is ready for trading.");
}
