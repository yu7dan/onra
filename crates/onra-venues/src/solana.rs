//! Reading a Solana wallet: native SOL plus every SPL token account (both the
//! classic Token program and Token-2022). Only the public address is needed.

use std::collections::HashMap;

use futures::future::join_all;
use onra_core::{Result, VenueId};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::{status_error, transport_error, unexpected};

/// Holdings are reported under Jupiter, the venue that trades them.
const VENUE: VenueId = VenueId::Jupiter;
pub const SOL_MINT: &str = "So11111111111111111111111111111111111111112";
const TOKEN_PROGRAMS: [&str; 2] =
    ["TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"];

pub struct Solana {
    http: Client,
    rpc: String,
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<Value>,
}

#[derive(Deserialize)]
struct WithValue<T> {
    value: T,
}

#[derive(Deserialize)]
struct TokenAccount {
    #[serde(default)]
    pubkey: String,
    account: AccountData,
}

/// One account of a wallet with its raw balance: the wallet itself (SOL, in
/// lamports, under [`SOL_MINT`]) or one of its token accounts.
#[derive(Debug, Clone)]
pub struct WalletAccount {
    pub address: String,
    pub mint: String,
    pub raw: u64,
    /// The wallet's own SOL, as opposed to a token account.
    pub native: bool,
}

/// What a transaction would do, according to the cluster.
#[derive(Debug, Clone)]
pub struct Simulation {
    /// `None` when it would succeed.
    pub err: Option<Value>,
    pub logs: Vec<String>,
    /// Raw balance of each requested account afterwards (`None`: closed).
    pub after: Vec<Option<u64>>,
}

#[derive(Deserialize)]
struct AccountData {
    data: ParsedData,
}

#[derive(Deserialize)]
struct ParsedData {
    parsed: Parsed,
}

#[derive(Deserialize)]
struct Parsed {
    info: TokenInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenInfo {
    mint: String,
    token_amount: TokenAmount,
}

#[derive(Deserialize)]
struct TokenAmount {
    amount: String,
    decimals: u32,
}

impl Solana {
    pub fn new(http: Client, rpc: String) -> Self {
        Self { http, rpc }
    }

    async fn call<T: serde::de::DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let resp = self
            .http
            .post(&self.rpc)
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?;
        if !resp.status().is_success() {
            return Err(status_error(VENUE, resp.status()));
        }
        let body: RpcResponse<T> = resp.json().await.map_err(|e| transport_error(VENUE, e))?;
        match (body.result, body.error) {
            (Some(result), _) => Ok(result),
            (None, Some(e)) => Err(unexpected(VENUE, format!("solana rpc: {e}"))),
            (None, None) => Err(unexpected(VENUE, "solana rpc returned nothing")),
        }
    }

    /// Exact amounts by mint, native SOL included under its wrapped mint.
    pub async fn holdings(&self, owner: &str) -> Result<HashMap<String, Decimal>> {
        let lamports = self.call::<WithValue<u64>>("getBalance", json!([owner]));
        let accounts = join_all(TOKEN_PROGRAMS.map(|program| {
            self.call::<WithValue<Vec<TokenAccount>>>(
                "getTokenAccountsByOwner",
                json!([owner, { "programId": program }, { "encoding": "jsonParsed" }]),
            )
        }));
        let (lamports, accounts) = tokio::join!(lamports, accounts);

        let mut out: HashMap<String, Decimal> = HashMap::new();
        let sol = Decimal::from_i128_with_scale(lamports?.value as i128, 9);
        if !sol.is_zero() {
            out.insert(SOL_MINT.to_string(), sol.normalize());
        }
        for program in accounts {
            for acc in program?.value {
                let info = acc.account.data.parsed.info;
                let Ok(raw) = info.token_amount.amount.parse::<i128>() else { continue };
                if raw == 0 {
                    continue;
                }
                let amount = Decimal::try_from_i128_with_scale(raw, info.token_amount.decimals)
                    .map_err(|_| unexpected(VENUE, "token amount out of range"))?;
                *out.entry(info.mint).or_default() += amount;
            }
        }
        Ok(out.into_iter().map(|(m, a)| (m, a.normalize())).collect())
    }
}

impl Solana {
    /// Every account of the wallet that holds value, with exact raw amounts.
    pub async fn accounts(&self, owner: &str) -> Result<Vec<WalletAccount>> {
        let lamports = self.call::<WithValue<u64>>("getBalance", json!([owner]));
        let accounts = join_all(TOKEN_PROGRAMS.map(|program| {
            self.call::<WithValue<Vec<TokenAccount>>>(
                "getTokenAccountsByOwner",
                json!([owner, { "programId": program }, { "encoding": "jsonParsed" }]),
            )
        }));
        let (lamports, accounts) = tokio::join!(lamports, accounts);
        let mut out = vec![WalletAccount {
            address: owner.to_string(),
            mint: SOL_MINT.to_string(),
            raw: lamports?.value,
            native: true,
        }];
        for program in accounts {
            for acc in program?.value {
                let info = acc.account.data.parsed.info;
                let raw = info.token_amount.amount.parse().unwrap_or(0);
                out.push(WalletAccount {
                    address: acc.pubkey,
                    mint: info.mint,
                    raw,
                    native: false,
                });
            }
        }
        Ok(out)
    }

    /// Runs a transaction against the current state without sending it, and
    /// reads back `watch` afterwards. No signature needed.
    pub async fn simulate(&self, tx: &[u8], watch: &[WalletAccount]) -> Result<Simulation> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        #[derive(Deserialize)]
        struct Sim {
            err: Option<Value>,
            logs: Option<Vec<String>>,
            accounts: Option<Vec<Option<Acc>>>,
        }
        #[derive(Deserialize)]
        struct Acc {
            lamports: u64,
            data: (String, String),
        }
        let addresses: Vec<&str> = watch.iter().map(|a| a.address.as_str()).collect();
        let sim: WithValue<Sim> = self
            .call(
                "simulateTransaction",
                json!([b64.encode(tx), {
                    "encoding": "base64",
                    "sigVerify": false,
                    "replaceRecentBlockhash": true,
                    "commitment": "confirmed",
                    "accounts": { "encoding": "base64", "addresses": addresses },
                }]),
            )
            .await?;
        let accounts = sim.value.accounts.unwrap_or_default();
        let after = watch
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let acc = accounts.get(i)?.as_ref()?;
                if w.native {
                    return Some(acc.lamports);
                }
                // An SPL token account keeps its amount at bytes 64..72.
                let data = b64.decode(&acc.data.0).ok()?;
                Some(u64::from_le_bytes(data.get(64..72)?.try_into().ok()?))
            })
            .collect();
        Ok(Simulation { err: sim.value.err, logs: sim.value.logs.unwrap_or_default(), after })
    }

    /// Sends a signed transaction; returns its signature.
    pub async fn send(&self, tx: &[u8]) -> Result<String> {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(tx);
        self.call(
            "sendTransaction",
            json!([encoded, { "encoding": "base64", "preflightCommitment": "confirmed", "maxRetries": 3 }]),
        )
        .await
    }

    /// Waits until the transaction is confirmed. `Ok(false)` if it is still
    /// unknown when `patience` runs out (it may yet land).
    pub async fn confirm(&self, signature: &str, patience: std::time::Duration) -> Result<bool> {
        let started = std::time::Instant::now();
        loop {
            let st: WithValue<Vec<Option<Value>>> =
                self.call("getSignatureStatuses", json!([[signature]])).await?;
            if let Some(Some(status)) = st.value.first() {
                if !status["err"].is_null() {
                    return Err(onra_core::Error::new(
                        onra_core::ErrorCode::Venue,
                        format!("the swap failed on chain: {}", status["err"]),
                    )
                    .with_hint(json!({ "venue": "jupiter", "cause": "failed_on_chain", "signature": signature })));
                }
                if matches!(status["confirmationStatus"].as_str(), Some("confirmed" | "finalized"))
                {
                    return Ok(true);
                }
            }
            if started.elapsed() >= patience {
                return Ok(false);
            }
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        }
    }
}
