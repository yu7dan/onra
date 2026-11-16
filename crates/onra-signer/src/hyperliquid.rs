//! Hyperliquid's L1 action signatures.
//!
//! An action (an order, a cancel, a leverage change) is packed with
//! MessagePack, hashed together with its nonce, and the hash is signed as an
//! EIP-712 "phantom agent". Field order inside the action matters: it is
//! part of the bytes being hashed. Verified against the test vectors of the
//! official Python SDK.

use onra_core::{Error, ErrorCode, Result};
use serde_json::Value;

use crate::evm::{EvmKey, EvmSignature, keccak};

/// The hash an action is signed under: MessagePack bytes, the nonce, and
/// whether it trades for a vault.
pub fn action_hash(action: &Value, nonce: u64, vault: Option<[u8; 20]>) -> Result<[u8; 32]> {
    let mut data = rmp_serde::to_vec_named(action)
        .map_err(|e| Error::new(ErrorCode::Internal, format!("cannot encode action: {e}")))?;
    data.extend_from_slice(&nonce.to_be_bytes());
    match vault {
        None => data.push(0),
        Some(address) => {
            data.push(1);
            data.extend_from_slice(&address);
        }
    }
    Ok(keccak(&data))
}

/// EIP-712 digest of the phantom agent `{ source, connectionId }`.
fn agent_digest(connection_id: &[u8; 32], mainnet: bool) -> [u8; 32] {
    let domain_type = keccak(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    );
    let mut chain_id = [0u8; 32];
    chain_id[30..].copy_from_slice(&1337u16.to_be_bytes());
    let domain = keccak(
        &[
            domain_type.as_slice(),
            &keccak(b"Exchange"),
            &keccak(b"1"),
            &chain_id,
            &[0u8; 32], // verifyingContract: the zero address
        ]
        .concat(),
    );
    let agent_type = keccak(b"Agent(string source,bytes32 connectionId)");
    let source: &[u8] = if mainnet { b"a" } else { b"b" };
    let agent = keccak(&[agent_type.as_slice(), &keccak(source), connection_id].concat());
    keccak(&[b"\x19\x01".as_slice(), &domain, &agent].concat())
}

pub fn sign_l1_action(
    key: &EvmKey,
    action: &Value,
    nonce: u64,
    vault: Option<[u8; 20]>,
    mainnet: bool,
) -> Result<EvmSignature> {
    let hash = action_hash(action, nonce, vault)?;
    Ok(key.sign_hash(&agent_digest(&hash, mainnet)))
}

/// Hyperliquid's number format: at most 8 decimals, no trailing zeros.
pub fn wire_decimal(v: rust_decimal::Decimal) -> String {
    let s = v.round_dp(8).normalize().to_string();
    if s == "-0" { "0".into() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KEY: &str = "0x0123456789012345678901234567890123456789012345678901234567890123";

    fn hex32(sig: &[u8; 32]) -> String {
        hex::encode(sig)
    }

    /// The SDK prints r and s without leading zeros.
    fn pad(s: &str) -> String {
        format!("{:0>64}", s.trim_start_matches("0x"))
    }

    fn order(a: u32, px: &str, sz: &str, tif: &str) -> Value {
        json!({
            "type": "order",
            "orders": [{ "a": a, "b": true, "p": px, "s": sz, "r": false, "t": { "limit": { "tif": tif } } }],
            "grouping": "na",
        })
    }

    #[test]
    fn phantom_agent_matches_production() {
        let action = order(4, "1670.1", "0.0147", "Ioc");
        let hash = action_hash(&action, 1_677_777_606_040, None).unwrap();
        assert_eq!(
            hex32(&hash),
            "0fcbeda5ae3c4950a548021552a4fea2226858c4453571bf3f24ba017eac2908"
        );
    }

    #[test]
    fn signs_like_the_sdk() {
        let key = EvmKey::parse(KEY).unwrap();
        let dummy = json!({ "type": "dummy", "num": 100_000_000_000u64 });
        let cases = [
            (
                &dummy,
                true,
                "0x53749d5b30552aeb2fca34b530185976545bb22d0b3ce6f62e31be961a59298",
                "0x755c40ba9bf05223521753995abb2f73ab3229be8ec921f350cb447e384d8ed8",
                27,
            ),
            (
                &dummy,
                false,
                "0x542af61ef1f429707e3c76c5293c80d01f74ef853e34b76efffcb57e574f9510",
                "0x17b8b32f086e8cdede991f1e2c529f5dd5297cbe8128500e00cbaf766204a613",
                28,
            ),
        ];
        let gtc = order(1, "100", "100", "Gtc");
        let orders = [
            (
                &gtc,
                true,
                "0xd65369825a9df5d80099e513cce430311d7d26ddf477f5b3a33d2806b100d78e",
                "0x2b54116ff64054968aa237c20ca9ff68000f977c93289157748a3162b6ea940e",
                28,
            ),
            (
                &gtc,
                false,
                "0x82b2ba28e76b3d761093aaded1b1cdad4960b3af30212b343fb2e6cdfa4e3d54",
                "0x6b53878fc99d26047f4d7e8c90eb98955a109f44209163f52d8dc4278cbbd9f5",
                27,
            ),
        ];
        for (action, mainnet, r, s, v) in cases.into_iter().chain(orders) {
            let sig = sign_l1_action(&key, action, 0, None, mainnet).unwrap();
            assert_eq!(hex32(&sig.r), pad(r), "r, mainnet={mainnet}");
            assert_eq!(hex32(&sig.s), pad(s), "s, mainnet={mainnet}");
            assert_eq!(sig.v, v, "v, mainnet={mainnet}");
        }
    }

    #[test]
    fn signs_for_a_vault() {
        let key = EvmKey::parse(KEY).unwrap();
        let dummy = json!({ "type": "dummy", "num": 100_000_000_000u64 });
        let mut vault = [0u8; 20];
        vault.copy_from_slice(&hex::decode("1719884eb866cb12b2287399b15f7db5e7d775ea").unwrap());
        let sig = sign_l1_action(&key, &dummy, 0, Some(vault), true).unwrap();
        assert_eq!(
            hex32(&sig.r),
            pad("0x3c548db75e479f8012acf3000ca3a6b05606bc2ec0c29c50c515066a326239")
        );
        assert_eq!(
            hex32(&sig.s),
            pad("0x4d402be7396ce74fbba3795769cda45aec00dc3125a984f2a9f23177b190da2c")
        );
        assert_eq!(sig.v, 28);
    }

    #[test]
    fn formats_wire_numbers() {
        assert_eq!(wire_decimal("1670.10000000".parse().unwrap()), "1670.1");
        assert_eq!(wire_decimal("0.000000001".parse().unwrap()), "0");
        assert_eq!(wire_decimal("100".parse().unwrap()), "100");
    }
}
