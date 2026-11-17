//! ed25519 keypairs as Solana accounts.

use ed25519_dalek::{Signer, SigningKey};
use onra_core::Result;
use zeroize::Zeroizing;

use crate::invalid;

#[derive(Clone)]
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

    /// Signs a serialized transaction (legacy or versioned) in place of its
    /// empty signature slot. Refuses one that doesn't ask this wallet to sign.
    pub fn sign_transaction(&self, tx: &[u8]) -> Result<Vec<u8>> {
        let bad = || invalid("not a Solana transaction");
        let (sigs, sig_len) = compact_u16(tx, 0).ok_or_else(bad)?;
        let msg_start = sig_len + 64 * sigs;
        let msg = tx.get(msg_start..).filter(|m| !m.is_empty()).ok_or_else(bad)?;
        // Versioned messages start with 0x80 | version; legacy ones with the header.
        let h = usize::from(msg[0] & 0x80 != 0);
        let required = usize::from(*msg.get(h).ok_or_else(bad)?);
        let (keys, keys_len) = compact_u16(msg, h + 3).ok_or_else(bad)?;
        let first = h + 3 + keys_len;
        let me = self.public_bytes();
        let index = (0..required.min(keys))
            .find(|i| msg.get(first + 32 * i..first + 32 * (i + 1)) == Some(&me[..]))
            .ok_or_else(|| invalid("this transaction does not ask this wallet to sign"))?;
        if index >= sigs {
            return Err(bad());
        }
        let mut out = tx.to_vec();
        let at = sig_len + 64 * index;
        out[at..at + 64].copy_from_slice(&self.sign(msg));
        Ok(out)
    }
}

/// Solana's compact-u16: 7 bits per byte, high bit set while more follow.
fn compact_u16(bytes: &[u8], at: usize) -> Option<(usize, usize)> {
    let mut value = 0usize;
    for i in 0..3 {
        let b = *bytes.get(at + i)?;
        value |= usize::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
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
    fn signs_versioned_transactions() {
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let key = SolanaKey::parse(&bs58::encode(pair()).into_string()).unwrap();
        // v0 message: prefix, header (1 signer), 2 keys, blockhash, no instructions.
        let mut msg = vec![0x80, 1, 0, 1, 2];
        msg.extend_from_slice(&key.public_bytes());
        msg.extend_from_slice(&[7u8; 32]);
        msg.extend_from_slice(&[9u8; 32]);
        msg.extend_from_slice(&[0, 0]);
        let mut tx = vec![1];
        tx.extend_from_slice(&[0u8; 64]);
        tx.extend_from_slice(&msg);

        let signed = key.sign_transaction(&tx).unwrap();
        let sig = Signature::from_slice(&signed[1..65]).unwrap();
        let vk = VerifyingKey::from_bytes(&key.public_bytes()).unwrap();
        assert!(vk.verify(&msg, &sig).is_ok());
        assert_eq!(&signed[65..], &msg[..], "the message is untouched");

        // A transaction for someone else is refused.
        let mut other = tx.clone();
        other[1 + 64 + 5] ^= 1;
        assert!(key.sign_transaction(&other).is_err());
        assert!(key.sign_transaction(&[1, 2, 3]).is_err());
    }

    #[test]
    fn reads_compact_u16() {
        assert_eq!(compact_u16(&[0x05], 0), Some((5, 1)));
        assert_eq!(compact_u16(&[0x80, 0x01], 0), Some((128, 2)));
        assert_eq!(compact_u16(&[0xff, 0xff, 0x03], 0), Some((65535, 3)));
        assert_eq!(compact_u16(&[0x80], 0), None);
    }

    #[test]
    fn rejects_mismatched_pairs() {
        let mut bad = pair();
        bad[40] ^= 1;
        assert!(SolanaKey::parse(&bs58::encode(bad).into_string()).is_err());
        assert!(SolanaKey::parse("abc").is_err());
    }
}
