//! Jupiter swaps: resolving tokens, quoting a route, and building the
//! transaction for it. Only the signer daemon uses this. Jupiter builds the
//! transaction, so the signer never signs it on trust: see
//! [`crate::Solana::simulate`] and the balance checks in the daemon.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use onra_core::{Error, ErrorCode, Result, VenueId};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::http::{decimal_f64, status_error, transport_error, unexpected};

const VENUE: VenueId = VenueId::Jupiter;
const API: &str = "https://lite-api.jup.ag";
/// Token metadata barely changes; the signer is long-lived, so remember it.
const RESOLVE_FRESH: Duration = Duration::from_secs(600);
static RESOLVED: LazyLock<Mutex<HashMap<String, (SwapToken, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct JupiterSwap {
    http: Client,
}

/// A token as a swap needs it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapToken {
    pub mint: String,
    pub symbol: String,
    pub decimals: u32,
    pub verified: bool,
    pub usd_price: Option<Decimal>,
}

/// A route, with Jupiter's raw quote kept for building the transaction.
#[derive(Debug, Clone)]
pub struct SwapQuote {
    pub raw: Value,
    pub in_amount: u64,
    pub out_amount: u64,
    /// The least the route may deliver after slippage.
    pub min_out: u64,
    pub price_impact_pct: Decimal,
    /// The pools the route goes through, e.g. ["Orca", "Raydium"].
    pub route: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Token {
    id: String,
    symbol: String,
    decimals: u32,
    #[serde(default)]
    is_verified: bool,
    usd_price: Option<f64>,
}

fn looks_like_mint(s: &str) -> bool {
    const BASE58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    (32..=44).contains(&s.len()) && s.chars().all(|c| BASE58.contains(c))
}

impl JupiterSwap {
    pub fn new(http: Client) -> Self {
        Self { http }
    }

    /// Sends a request, waiting out the free tier's rate limit a couple of
    /// times before giving up.
    async fn send(&self, req: impl Fn() -> reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let mut wait = Duration::from_millis(1200);
        for attempt in 0..3 {
            let resp = req().send().await.map_err(|e| transport_error(VENUE, e))?;
            if resp.status() != reqwest::StatusCode::TOO_MANY_REQUESTS || attempt == 2 {
                return Ok(resp);
            }
            tokio::time::sleep(wait).await;
            wait *= 2;
        }
        unreachable!("the last attempt returns")
    }

    async fn search(&self, query: &str) -> Result<Vec<Token>> {
        let resp = self
            .send(|| self.http.get(format!("{API}/tokens/v2/search")).query(&[("query", query)]))
            .await?;
        if !resp.status().is_success() {
            return Err(status_error(VENUE, resp.status()));
        }
        resp.json().await.map_err(|e| transport_error(VENUE, e))
    }

    /// A symbol resolves only to a verified token, and only if exactly one
    /// has it. A mint address resolves to that token, verified or not.
    pub async fn resolve(&self, query: &str) -> Result<SwapToken> {
        let key = query.to_ascii_uppercase();
        let cached = RESOLVED.lock().expect("cache lock").get(&key).cloned();
        if let Some((token, at)) = cached
            && at.elapsed() < RESOLVE_FRESH
        {
            return Ok(token);
        }
        let token = self.resolve_uncached(query).await?;
        RESOLVED.lock().expect("cache lock").insert(key, (token.clone(), Instant::now()));
        Ok(token)
    }

    async fn resolve_uncached(&self, query: &str) -> Result<SwapToken> {
        let tokens = self.search(query).await?;
        let to_swap = |t: Token| SwapToken {
            mint: t.id,
            symbol: t.symbol.to_ascii_uppercase(),
            decimals: t.decimals,
            verified: t.is_verified,
            usd_price: t.usd_price.and_then(decimal_f64),
        };
        if looks_like_mint(query) {
            return tokens.into_iter().find(|t| t.id == query).map(to_swap).ok_or_else(|| {
                Error::new(
                    ErrorCode::MarketNotFound,
                    format!("jupiter does not know the mint {query}"),
                )
                .with_hint(json!({ "venue": "jupiter", "query": query }))
            });
        }
        let named: Vec<Token> =
            tokens.into_iter().filter(|t| t.symbol.eq_ignore_ascii_case(query)).collect();
        let verified: Vec<&Token> = named.iter().filter(|t| t.is_verified).collect();
        match verified.len() {
            1 => {
                let id = verified[0].id.clone();
                Ok(to_swap(named.into_iter().find(|t| t.id == id).expect("present")))
            }
            0 if named.is_empty() => Err(Error::new(
                ErrorCode::MarketNotFound,
                format!("no token called {query} on jupiter"),
            )
            .with_hint(json!({ "venue": "jupiter", "query": query }))),
            0 => Err(Error::new(
                ErrorCode::Usage,
                format!("every token called {query} is unverified; name it by mint address if you are sure"),
            )
            .with_hint(json!({ "venue": "jupiter", "cause": "unverified", "query": query, "suggest": format!("onra search {query}") }))),
            _ => Err(Error::new(
                ErrorCode::Usage,
                format!("more than one verified token is called {query}; name it by mint address"),
            )
            .with_hint(json!({ "venue": "jupiter", "cause": "ambiguous", "query": query, "candidates": verified.iter().map(|t| &t.id).collect::<Vec<_>>() }))),
        }
    }

    pub async fn quote(
        &self,
        input: &str,
        output: &str,
        amount: u64,
        slippage_bps: u32,
    ) -> Result<SwapQuote> {
        let (amount, bps) = (amount.to_string(), slippage_bps.to_string());
        let resp = self
            .send(|| {
                self.http.get(format!("{API}/swap/v1/quote")).query(&[
                    ("inputMint", input),
                    ("outputMint", output),
                    ("amount", amount.as_str()),
                    ("slippageBps", bps.as_str()),
                    ("restrictIntermediateTokens", "true"),
                ])
            })
            .await?;
        let status = resp.status();
        let raw: Value = resp.json().await.map_err(|e| transport_error(VENUE, e))?;
        if !status.is_success() {
            let msg = raw["error"].as_str().unwrap_or("no route");
            return Err(Error::new(ErrorCode::Venue, format!("jupiter: {msg}"))
                .with_hint(json!({ "venue": "jupiter", "cause": "no_route" })));
        }
        let int = |k: &str| -> Result<u64> {
            raw[k]
                .as_str()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| unexpected(VENUE, format!("quote without {k}")))
        };
        let route = raw["routePlan"]
            .as_array()
            .map(|steps| {
                let mut labels: Vec<String> = Vec::new();
                for s in steps {
                    if let Some(l) = s["swapInfo"]["label"].as_str()
                        && !labels.iter().any(|x| x == l)
                    {
                        labels.push(l.to_string());
                    }
                }
                labels
            })
            .unwrap_or_default();
        Ok(SwapQuote {
            in_amount: int("inAmount")?,
            out_amount: int("outAmount")?,
            min_out: int("otherAmountThreshold")?,
            price_impact_pct: raw["priceImpactPct"]
                .as_str()
                .and_then(|s| s.parse::<Decimal>().ok())
                .map(|p| (p * Decimal::ONE_HUNDRED).round_dp(4))
                .unwrap_or_default(),
            route,
            raw,
        })
    }

    /// The unsigned transaction for a quote, for `user` to sign.
    pub async fn transaction(&self, quote: &SwapQuote, user: &str) -> Result<Vec<u8>> {
        use base64::Engine;
        let body = json!({
            "quoteResponse": quote.raw,
            "userPublicKey": user,
            "dynamicComputeUnitLimit": true,
            "prioritizationFeeLamports": { "priorityLevelWithMaxLamports": { "maxLamports": 200_000, "priorityLevel": "high" } },
        });
        let resp = self.send(|| self.http.post(format!("{API}/swap/v1/swap")).json(&body)).await?;
        if !resp.status().is_success() {
            return Err(status_error(VENUE, resp.status()));
        }
        let reply: Value = resp.json().await.map_err(|e| transport_error(VENUE, e))?;
        let tx = reply["swapTransaction"]
            .as_str()
            .ok_or_else(|| unexpected(VENUE, "swap reply without a transaction"))?;
        base64::engine::general_purpose::STANDARD
            .decode(tx)
            .map_err(|_| unexpected(VENUE, "swap transaction is not base64"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_mints_from_symbols() {
        assert!(looks_like_mint("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"));
        assert!(!looks_like_mint("USDC"));
        assert!(!looks_like_mint("0x1111111111111111111111111111111111111111"));
    }
}
