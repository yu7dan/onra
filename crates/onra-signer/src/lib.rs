//! Trading keys for Onra, and the signatures they make.
//!
//! This crate only ever runs inside the signer daemon (`onra daemon`). The
//! command line and the agents driving it talk to the daemon; they never
//! hold a key.
//!
//! - Binance: an API key and secret; requests are signed with HMAC-SHA256.
//! - Hyperliquid: an API wallet (it can trade, it cannot withdraw), a
//!   secp256k1 key signing EIP-712 typed data.
//! - Solana: an ed25519 keypair signing transactions built by Jupiter.

mod evm;
mod solana;

pub use evm::{EvmKey, EvmSignature, checksum, keccak};
pub use solana::SolanaKey;

use onra_core::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// A Binance API key pair. The key is an identifier; the secret signs.
#[derive(Clone, Serialize, Deserialize)]
pub struct BinanceKey {
    pub key: String,
    pub secret: Zeroizing<String>,
}

impl std::fmt::Debug for BinanceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BinanceKey").field("key", &self.key).finish_non_exhaustive()
    }
}

/// What a vault entry holds, decoded and ready to sign with.
pub enum TradingKey {
    Binance(BinanceKey),
    Hyperliquid(EvmKey),
    Solana(SolanaKey),
}

impl TradingKey {
    /// Decodes a vault entry. Names are `bn`, `hl` and `sol`, optionally
    /// suffixed `.testnet`.
    pub fn decode(name: &str, secret: &str) -> Result<Self> {
        match name.split('.').next() {
            Some("bn") => serde_json::from_str(secret)
                .map(TradingKey::Binance)
                .map_err(|_| invalid("Binance key entry is malformed")),
            Some("hl") => EvmKey::parse(secret).map(TradingKey::Hyperliquid),
            Some("sol") => SolanaKey::parse(secret).map(TradingKey::Solana),
            _ => Err(invalid(format!("unknown trading key `{name}`"))),
        }
    }

    /// The public side: an address, or for Binance the (non-secret) key id.
    pub fn public(&self) -> String {
        match self {
            TradingKey::Binance(k) => k.key.clone(),
            TradingKey::Hyperliquid(k) => k.address(),
            TradingKey::Solana(k) => k.address(),
        }
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::Credentials, message)
}
