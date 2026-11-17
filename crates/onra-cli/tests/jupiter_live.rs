//! Live checks against Jupiter and Solana mainnet. Nothing is sent: the
//! wallet is fresh and empty, and transactions are only simulated.
//! `cargo test -p onra-cli --test jupiter_live -- --ignored --nocapture`

use onra_signer::SolanaKey;
use onra_venues::{NetConfig, Venues};

#[tokio::test]
#[ignore]
async fn quotes_builds_signs_and_simulates() {
    let net = NetConfig { proxy: std::env::var("ONRA_PROXY").ok(), ..NetConfig::default() };
    let venues = Venues::new(&net).unwrap();
    let jup = venues.jupiter_swap();
    let usdc = jup.resolve("USDC").await.unwrap();
    let sol = jup.resolve("SOL").await.unwrap();
    assert_eq!(usdc.mint, "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
    assert_eq!(sol.mint, onra_venues::SOL_MINT);
    assert!(jup.resolve("NOT-A-REAL-TOKEN-XYZ").await.is_err());

    let quote = jup.quote(&usdc.mint, &sol.mint, 1_000_000, 50).await.unwrap();
    println!(
        "1 USDC -> {} lamports (min {}), via {:?}, impact {}%",
        quote.out_amount, quote.min_out, quote.route, quote.price_impact_pct
    );
    assert!(quote.min_out <= quote.out_amount && quote.out_amount > 0);

    // A brand-new keypair: Jupiter builds for it, we sign, the cluster
    // simulates. It has nothing, so the simulation must fail, not the plumbing.
    let mut pair = [0u8; 64];
    let secret = onra_signer::keccak(
        format!("onra jupiter check {:?}", std::time::SystemTime::now()).as_bytes(),
    );
    pair[..32].copy_from_slice(&secret);
    let public = ed25519_public(&secret);
    pair[32..].copy_from_slice(&public);
    let key = SolanaKey::parse(&bs58::encode(pair).into_string()).unwrap();

    let tx = jup.transaction(&quote, &key.address()).await.unwrap();
    let signed = key.sign_transaction(&tx).unwrap();
    assert_eq!(signed.len(), tx.len());
    let watch = venues.solana().accounts(&key.address()).await.unwrap();
    let sim = venues.solana().simulate(&signed, &watch).await.unwrap();
    println!("simulation: err={:?} after={:?}", sim.err, sim.after);
    assert!(sim.err.is_some(), "an empty wallet cannot swap");
}

fn ed25519_public(secret: &[u8; 32]) -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(secret).verifying_key().to_bytes()
}
