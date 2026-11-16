//! ed25519 keypairs as Solana accounts.

use ed25519_dalek::{Signer, SigningKey};
use onra_core::Result;
use zeroize::Zeroizing;

use crate::invalid;

pub struct SolanaKey {
    key: SigningKey,
}

impl SolanaKey {
    /// Accepts the two common export formats: a base58 string of the 64-byte
    /// keypair (wallet apps), or a JSON array of 64 numbers (`solana-keygen`).
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        let bytes: Zeroizing<Vec<u8>> = Zeroizing::new(if text.starts_with('[') {
            serde_json::from_str(text).map_err(|_| invalid("keypair JSON must be 64 numbers"))?
        } else {
            bs58::decode(text).into_vec().map_err(|_| invalid("keypair is not valid base58"))?
        });
        let pair: &[u8; 64] =
            bytes.as_slice().try_into().map_err(|_| invalid("a Solana keypair is 64 bytes"))?;
        // Rejects pairs whose public half doesn't match the secret.
        let key = SigningKey::from_keypair_bytes(pair)
            .map_err(|_| invalid("keypair is inconsistent: public key does not match"))?;
        Ok(Self { key })
    }

    pub fn address(&self) -> String {
        bs58::encode(self.key.verifying_key().as_bytes()).into_string()
    }

    pub fn public_bytes(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.key.sign(message).to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 8032, test 1.
    const SECRET: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    const PUBLIC: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    const SIGNATURE: &str = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";

    fn pair() -> Vec<u8> {
        [hex::decode(SECRET).unwrap(), hex::decode(PUBLIC).unwrap()].concat()
    }

    #[test]
    fn parses_both_formats_and_signs() {
        let b58 = bs58::encode(pair()).into_string();
        let json = serde_json::to_string(&pair()).unwrap();
        for text in [b58.as_str(), json.as_str()] {
            let key = SolanaKey::parse(text).unwrap();
            assert_eq!(hex::encode(key.public_bytes()), PUBLIC);
            assert_eq!(hex::encode(key.sign(b"")), SIGNATURE);
        }
    }

    #[test]
    fn rejects_mismatched_pairs() {
        let mut bad = pair();
        bad[40] ^= 1;
        assert!(SolanaKey::parse(&bs58::encode(bad).into_string()).is_err());
        assert!(SolanaKey::parse("abc").is_err());
    }
}
