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
    account: AccountData,
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
