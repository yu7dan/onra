//! Hyperliquid perps trading: market rules, account state, signed actions.
//!
//! Only the signer daemon uses this, and it signs; this module only knows
//! the API. Orders are always limit orders, like on Binance: a "market"
//! order is a limit at the best price plus the slippage bound, IOC.

use onra_core::{Error, ErrorCode, Result, VenueId};
use reqwest::Client;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::http::{decimal, status_error, transport_error};

const VENUE: VenueId = VenueId::Hyperliquid;
const MAINNET: &str = "https://api.hyperliquid.xyz";
const TESTNET: &str = "https://api.hyperliquid-testnet.xyz";

pub struct HyperliquidTrade {
    http: Client,
    base: &'static str,
    pub testnet: bool,
}

/// How a perp trades.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerpRules {
    pub coin: String,
    /// Position in the venue's asset list: orders name assets by index.
    pub asset: u32,
    pub sz_decimals: u32,
    pub max_leverage: u32,
}

impl PerpRules {
    /// Sizes round down to the asset's precision.
    pub fn size(&self, v: Decimal) -> Decimal {
        v.round_dp_with_strategy(self.sz_decimals, RoundingStrategy::ToZero).normalize()
    }

    /// Prices: at most five significant figures and `6 - szDecimals`
    /// decimals (whole numbers are always fine). Buys round down and sells
    /// up, so a bound never loosens.
    pub fn price(&self, v: Decimal, buy: bool) -> Decimal {
        let max_dec = 6u32.saturating_sub(self.sz_decimals);
        let whole_digits = {
            let mut n = 0u32;
            let mut x = v.trunc();
            while x >= Decimal::ONE {
                x = (x / Decimal::TEN).trunc();
                n += 1;
            }
            n
        };
        let sig_dec = if whole_digits > 0 {
            5u32.saturating_sub(whole_digits)
        } else {
            // 0.000123: leading zeros after the point, then five figures.
            let mut zeros = 0u32;
            let mut x = v;
            while x < Decimal::new(1, 1) && zeros < 20 && !x.is_zero() {
                x *= Decimal::TEN;
                zeros += 1;
            }
            zeros + 5
        };
        let strategy = if buy { RoundingStrategy::ToZero } else { RoundingStrategy::AwayFromZero };
        v.round_dp_with_strategy(max_dec.min(sig_dec), strategy).normalize()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Top {
    pub bid: Decimal,
    pub ask: Decimal,
}

/// The parts of an account that planning needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerpAccount {
    /// Margin free to open new positions.
    pub withdrawable: Decimal,
    pub account_value: Decimal,
    /// Signed size of the position in the coin asked about (negative: short).
    pub position: Decimal,
}

/// What happened to one order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerpFill {
    pub order_id: String,
    /// `filled`, `resting`, or `canceled` (an IOC order that found nothing).
    pub status: String,
    pub executed_qty: Decimal,
    pub avg_price: Option<Decimal>,
}

impl HyperliquidTrade {
    pub fn new(http: Client, testnet: bool) -> Self {
        Self { http, base: if testnet { TESTNET } else { MAINNET }, testnet }
    }

    async fn post<T: serde::de::DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?;
        let status = resp.status();
        if !status.is_success() {
            // Exchange errors come back as text with a 4xx/5xx.
            let text = resp.text().await.unwrap_or_default();
            if !text.is_empty() && status.is_client_error() {
                return Err(venue_error(&text));
            }
            return Err(status_error(VENUE, status));
        }
        resp.json().await.map_err(|e| transport_error(VENUE, e))
    }

    pub async fn rules(&self, coin: &str) -> Result<PerpRules> {
        #[derive(Deserialize)]
        struct Meta {
            universe: Vec<Asset>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Asset {
            name: String,
            sz_decimals: u32,
            max_leverage: u32,
            #[serde(default)]
            is_delisted: bool,
        }
        let meta: Meta = self.post("/info", &json!({ "type": "meta" })).await?;
        meta.universe
            .into_iter()
            .enumerate()
            .find(|(_, a)| a.name.eq_ignore_ascii_case(coin) && !a.is_delisted)
            .map(|(i, a)| PerpRules {
                coin: a.name,
                asset: i as u32,
                sz_decimals: a.sz_decimals,
                max_leverage: a.max_leverage,
            })
            .ok_or_else(|| {
                Error::new(ErrorCode::MarketNotFound, format!("hyperliquid has no perp {coin}"))
                    .with_hint(json!({ "venue": "hyperliquid", "query": coin }))
            })
    }

    pub async fn top(&self, coin: &str) -> Result<Top> {
        #[derive(Deserialize)]
        struct Book {
            levels: Vec<Vec<Level>>,
        }
        #[derive(Deserialize)]
        struct Level {
            px: String,
        }
        let book: Book = self.post("/info", &json!({ "type": "l2Book", "coin": coin })).await?;
        let best = |side: usize| -> Result<Decimal> {
            let level = book.levels.get(side).and_then(|l| l.first()).ok_or_else(|| {
                Error::new(ErrorCode::Venue, format!("hyperliquid has no {coin} book right now"))
            })?;
            decimal(VENUE, &level.px)
        };
        Ok(Top { bid: best(0)?, ask: best(1)? })
    }

    pub async fn account(&self, user: &str, coin: &str) -> Result<PerpAccount> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct State {
            withdrawable: String,
            margin_summary: Summary,
            asset_positions: Vec<Wrapped>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Summary {
            account_value: String,
        }
        #[derive(Deserialize)]
        struct Wrapped {
            position: Pos,
        }
        #[derive(Deserialize)]
        struct Pos {
            coin: String,
            szi: String,
        }
        let s: State =
            self.post("/info", &json!({ "type": "clearinghouseState", "user": user })).await?;
        let position =
            match s.asset_positions.iter().find(|p| p.position.coin.eq_ignore_ascii_case(coin)) {
                Some(p) => decimal(VENUE, &p.position.szi)?,
                None => Decimal::ZERO,
            };
        Ok(PerpAccount {
            withdrawable: decimal(VENUE, &s.withdrawable)?,
            account_value: decimal(VENUE, &s.margin_summary.account_value)?,
            position,
        })
    }

    /// Sends a signed action. `signature` is `{ r, s, v }` from the signer.
    pub async fn exchange(&self, action: &Value, nonce: u64, signature: Value) -> Result<Value> {
        let body = json!({ "action": action, "nonce": nonce, "signature": signature, "vaultAddress": null });
        let reply: Value = self.post("/exchange", &body).await?;
        if reply["status"] != "ok" {
            return Err(venue_error(reply["response"].as_str().unwrap_or("request refused")));
        }
        Ok(reply["response"].clone())
    }
}

/// The first status of an order response.
pub fn parse_order_status(response: &Value) -> Result<PerpFill> {
    let status = &response["data"]["statuses"][0];
    if let Some(err) = status["error"].as_str() {
        return Err(venue_error(err));
    }
    let num = |v: &Value| -> Result<Decimal> {
        match v.as_str() {
            Some(s) => decimal(VENUE, s),
            None => Ok(Decimal::ZERO),
        }
    };
    if let Some(f) = status.get("filled") {
        return Ok(PerpFill {
            order_id: f["oid"].to_string(),
            status: "filled".into(),
            executed_qty: num(&f["totalSz"])?,
            avg_price: Some(num(&f["avgPx"])?),
        });
    }
    if let Some(r) = status.get("resting") {
        return Ok(PerpFill {
            order_id: r["oid"].to_string(),
            status: "resting".into(),
            executed_qty: Decimal::ZERO,
            avg_price: None,
        });
    }
    Err(Error::new(ErrorCode::Venue, format!("hyperliquid: unexpected reply {status}")))
}

fn venue_error(message: &str) -> Error {
    let lower = message.to_ascii_lowercase();
    let cause = if lower.contains("does not exist") && lower.contains("wallet") {
        // The API wallet was never approved for this account (or expired).
        "agent_not_approved"
    } else if lower.contains("insufficient margin") || lower.contains("not enough") {
        "insufficient_balance"
    } else if lower.contains("could not immediately match") {
        "no_match"
    } else if lower.contains("minimum value") {
        "min_order"
    } else {
        "rejected"
    };
    let code =
        if cause == "agent_not_approved" { ErrorCode::Credentials } else { ErrorCode::Venue };
    Error::new(code, format!("hyperliquid: {message}"))
        .with_hint(json!({ "venue": "hyperliquid", "cause": cause }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(sz_decimals: u32) -> PerpRules {
        PerpRules { coin: "X".into(), asset: 0, sz_decimals, max_leverage: 50 }
    }

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn prices_follow_the_five_figure_rule() {
        let btc = rules(5); // 1 decimal allowed
        assert_eq!(btc.price(d("65069.62"), true).to_string(), "65069");
        assert_eq!(btc.price(d("65069.62"), false).to_string(), "65070");
        assert_eq!(btc.price(d("105123.4"), true).to_string(), "105123", "whole numbers pass");
        let eth = rules(4); // 2 decimals
        assert_eq!(eth.price(d("1891.456"), true).to_string(), "1891.4");
        let small = rules(0); // 6 decimals
        assert_eq!(small.price(d("0.0123456"), true).to_string(), "0.012345");
        assert_eq!(small.price(d("0.0123456"), false).to_string(), "0.012346");
    }

    #[test]
    fn sizes_round_down() {
        assert_eq!(rules(4).size(d("0.123456")).to_string(), "0.1234");
        assert_eq!(rules(0).size(d("12.9")).to_string(), "12");
    }

    #[test]
    fn reads_order_statuses() {
        let filled = json!({ "type": "order", "data": { "statuses": [ { "filled": { "totalSz": "0.02", "avgPx": "1891.4", "oid": 77738308 } } ] } });
        let f = parse_order_status(&filled).unwrap();
        assert_eq!((f.status.as_str(), f.order_id.as_str()), ("filled", "77738308"));
        assert_eq!(f.executed_qty, d("0.02"));
        let resting = json!({ "data": { "statuses": [ { "resting": { "oid": 5 } } ] } });
        assert_eq!(parse_order_status(&resting).unwrap().status, "resting");
        let err = json!({ "data": { "statuses": [ { "error": "Order could not immediately match against any resting orders." } ] } });
        assert_eq!(parse_order_status(&err).unwrap_err().hint.unwrap()["cause"], "no_match");
        let agent = venue_error("User or API Wallet 0xabc does not exist.");
        assert_eq!(agent.code, ErrorCode::Credentials);
    }
}
