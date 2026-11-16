//! Live checks against Hyperliquid's testnet. They need network access, so
//! they only run on request: `cargo test -p onra-cli --test hyperliquid_live -- --ignored`.

use onra_signer::EvmKey;
use onra_signer::hyperliquid::sign_l1_action;
use onra_venues::{NetConfig, Venues};
use serde_json::json;

/// A key no one has approved: the testnet answers that this wallet does
/// not exist, naming the address it recovered from the signature. If that
/// is our address, the whole signing path matches what the venue expects.
#[tokio::test]
#[ignore]
async fn testnet_recovers_our_signer() {
    // Derived, not a well-known test key: those have testnet accounts, and
    // this check must never place an order for anyone.
    let key = EvmKey::parse(&hex::encode(onra_signer::keccak(b"onra: hyperliquid signing check")))
        .unwrap();
    let net = NetConfig { proxy: std::env::var("ONRA_PROXY").ok(), ..NetConfig::default() };
    let hl = Venues::new(&net).unwrap().hyperliquid_trade(true);
    let rules = hl.rules("ETH").await.unwrap();
    let top = hl.top("ETH").await.unwrap();
    let px = rules.price(top.bid * rust_decimal::Decimal::new(5, 1), true);
    let action = json!({
        "type": "order",
        "orders": [{ "a": rules.asset, "b": true, "p": onra_signer::hyperliquid::wire_decimal(px), "s": "0.01", "r": false, "t": { "limit": { "tif": "Gtc" } } }],
        "grouping": "na",
    });
    let nonce =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
            as u64;
    let sig = sign_l1_action(&key, &action, nonce, None, false).unwrap();
    let signature = json!({ "r": format!("0x{}", hex::encode(sig.r)), "s": format!("0x{}", hex::encode(sig.s)), "v": sig.v });
    let err = hl.exchange(&action, nonce, signature).await.unwrap_err();
    println!("venue said: {}", err.message);
    assert!(
        err.message.to_lowercase().contains(&key.address().to_lowercase()),
        "the venue recovered a different signer: {}",
        err.message
    );
}
