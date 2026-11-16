//! secp256k1 keys as Ethereum-style accounts.

use k256::ecdsa::SigningKey;
use onra_core::Result;
use sha3::{Digest, Keccak256};
use zeroize::Zeroizing;

use crate::invalid;

pub struct EvmKey {
    key: SigningKey,
}

/// An Ethereum signature: `r`, `s` and `v` (27 or 28).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvmSignature {
    pub r: [u8; 32],
    pub s: [u8; 32],
    pub v: u8,
}

impl EvmKey {
    /// A 32-byte private key in hex, with or without `0x`.
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        let hex_part = text.strip_prefix("0x").unwrap_or(text);
        let bytes = Zeroizing::new(
            hex::decode(hex_part).map_err(|_| invalid("private key must be 64 hex characters"))?,
        );
        if bytes.len() != 32 {
            return Err(invalid("private key must be 64 hex characters"));
        }
        let key = SigningKey::from_slice(&bytes).map_err(|_| invalid("not a valid private key"))?;
        Ok(Self { key })
    }

    /// The account address, EIP-55 checksummed.
    pub fn address(&self) -> String {
        checksum(&self.address_bytes())
    }

    pub fn address_bytes(&self) -> [u8; 20] {
        let point = self.key.verifying_key().to_sec1_point(false);
        // Uncompressed SEC1 is 0x04 || x || y; the address hashes x || y.
        let hash = Keccak256::digest(&point.as_bytes()[1..]);
        let mut out = [0u8; 20];
        out.copy_from_slice(&hash[12..]);
        out
    }

    /// Signs a 32-byte digest, e.g. an EIP-712 hash.
    pub fn sign_hash(&self, hash: &[u8; 32]) -> EvmSignature {
        let (sig, recovery) = self.key.sign_prehash_recoverable(hash);
        let bytes = sig.to_bytes();
        let mut r = [0u8; 32];
        let mut s = [0u8; 32];
        r.copy_from_slice(&bytes[..32]);
        s.copy_from_slice(&bytes[32..]);
        EvmSignature { r, s, v: 27 + u8::from(recovery.is_y_odd()) }
    }
}

pub fn keccak(data: &[u8]) -> [u8; 32] {
    Keccak256::digest(data).into()
}

/// EIP-55 mixed-case checksum encoding.
pub fn checksum(address: &[u8; 20]) -> String {
    let lower = hex::encode(address);
    let hash = Keccak256::digest(lower.as_bytes());
    let mut out = String::with_capacity(42);
    out.push_str("0x");
    for (i, c) in lower.chars().enumerate() {
        let nibble = (hash[i / 2] >> (if i % 2 == 0 { 4 } else { 0 })) & 0x0f;
        out.push(if nibble >= 8 { c.to_ascii_uppercase() } else { c });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The account from the web3.js documentation.
    const KEY: &str = "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";

    #[test]
    fn derives_address() {
        let key = EvmKey::parse(KEY).unwrap();
        assert_eq!(key.address(), "0x2c7536E3605D9C16a7a3D7b1898e529396a65c23");
        assert!(EvmKey::parse(&KEY[2..]).is_ok(), "0x is optional");
        assert!(EvmKey::parse("0x1234").is_err());
        assert!(EvmKey::parse(&"0".repeat(64)).is_err(), "zero is not a key");
    }

    #[test]
    fn signs_recoverably() {
        use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
        let key = EvmKey::parse(KEY).unwrap();
        let hash = keccak(b"onra");
        let sig = key.sign_hash(&hash);
        assert!(sig.v == 27 || sig.v == 28);
        let mut bytes = [0u8; 64];
        bytes[..32].copy_from_slice(&sig.r);
        bytes[32..].copy_from_slice(&sig.s);
        let parsed = Signature::from_slice(&bytes).unwrap();
        let recovered =
            VerifyingKey::recover_from_prehash(&hash, &parsed, RecoveryId::new(sig.v == 28, false))
                .unwrap();
        assert_eq!(&recovered, key.key.verifying_key());
        assert!(parsed.normalize_s() == parsed, "s must be low (EIP-2)");
    }
}
