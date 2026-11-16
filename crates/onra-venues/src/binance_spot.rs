//! Binance spot trading: the rules a symbol trades under, and orders.
//!
//! Only the signer daemon uses this. Orders are always limit orders: a
//! "market" buy is a limit order at the ask plus the allowed slippage, good
//! only for an immediate fill (IOC), so a thin book can never fill it at a
//! price worse than the plan said.

use hmac::{Hmac, Mac};
use onra_core::{Error, ErrorCode, Result, VenueId};
use reqwest::{Client, Method, StatusCode};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::BinanceKey;
use crate::http::{decimal, status_error, transport_error};

const VENUE: VenueId = VenueId::Binance;
const MAINNET: &str = "https://api.binance.com";
/// Binance's spot test network: real API, play money.
const TESTNET: &str = "https://testnet.binance.vision";

pub struct BinanceSpot {
    http: Client,
    base: &'static str,
}

/// How a symbol trades: what its quantities and prices must be multiples of.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolRules {
    pub symbol: String,
    pub base: String,
    pub quote: String,
    pub trading: bool,
    pub step: Decimal,
    pub min_qty: Decimal,
    pub tick: Decimal,
    pub min_notional: Decimal,
}

impl SymbolRules {
    /// Rounds a quantity down to the lot step.
    pub fn qty(&self, qty: Decimal) -> Decimal {
        floor_to(qty, self.step)
    }

    /// Rounds a price to the tick: down for buys, up for sells, so the bound
    /// never loosens.
    pub fn price(&self, price: Decimal, buy: bool) -> Decimal {
        let floored = floor_to(price, self.tick);
        if buy || floored == price { floored } else { floored + self.tick }
    }
}

fn floor_to(v: Decimal, step: Decimal) -> Decimal {
    if step.is_zero() {
        return v;
    }
    ((v / step).floor() * step).normalize()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BookTop {
    pub bid: Decimal,
    pub ask: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeInForce {
    /// Fill now what can be filled, cancel the rest.
    Ioc,
    /// Rest on the book until filled or cancelled.
    Gtc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotOrder {
    pub symbol: String,
    pub buy: bool,
    pub qty: Decimal,
    pub price: Decimal,
    pub tif: TimeInForce,
    /// Idempotency: Binance rejects a second order with the same id.
    pub client_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotFill {
    pub order_id: String,
    pub status: String,
    pub executed_qty: Decimal,
    /// Quote currency actually spent or received.
    pub quote_qty: Decimal,
    pub avg_price: Option<Decimal>,
    pub fee: Vec<(Decimal, String)>,
}

#[derive(Deserialize)]
struct ApiError {
    code: i64,
    msg: String,
}

impl BinanceSpot {
    pub fn new(http: Client, testnet: bool) -> Self {
        Self { http, base: if testnet { TESTNET } else { MAINNET } }
    }

    async fn public<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?;
        let status = resp.status();
        if status.is_success() {
            return resp.json().await.map_err(|e| transport_error(VENUE, e));
        }
        Err(api_error(status, resp.json().await.ok()))
    }

    async fn signed<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        key: &BinanceKey,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<T> {
        let ts = crate::binance_clock::timestamp(&self.http, self.base).await?;
        let mut query: String = params.iter().map(|(k, v)| format!("{k}={v}&")).collect();
        query.push_str(&format!("timestamp={ts}&recvWindow=5000"));
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key.secret.as_bytes())
            .map_err(|_| Error::new(ErrorCode::Credentials, "invalid binance secret"))?;
        mac.update(query.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());
        let resp = self
            .http
            .request(method, format!("{}{path}?{query}&signature={signature}", self.base))
            .header("X-MBX-APIKEY", &key.key)
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?;
        let status = resp.status();
        if status.is_success() {
            return resp.json().await.map_err(|e| transport_error(VENUE, e));
        }
        Err(api_error(status, resp.json().await.ok()))
    }

    pub async fn rules(&self, symbol: &str) -> Result<SymbolRules> {
        #[derive(Deserialize)]
        struct Info {
            symbols: Vec<Sym>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Sym {
            symbol: String,
            status: String,
            base_asset: String,
            quote_asset: String,
            filters: Vec<serde_json::Value>,
        }
        let info: Info = self.public(&format!("/api/v3/exchangeInfo?symbol={symbol}")).await?;
        let s = info.symbols.into_iter().next().ok_or_else(|| not_listed(symbol))?;
        let filter = |kind: &str, field: &str| -> Option<Decimal> {
            s.filters
                .iter()
                .find(|f| f["filterType"] == kind)
                .and_then(|f| f[field].as_str())
                .and_then(|v| v.parse().ok())
        };
        Ok(SymbolRules {
            trading: s.status == "TRADING",
            step: filter("LOT_SIZE", "stepSize").unwrap_or_default(),
            min_qty: filter("LOT_SIZE", "minQty").unwrap_or_default(),
            tick: filter("PRICE_FILTER", "tickSize").unwrap_or_default(),
            min_notional: filter("NOTIONAL", "minNotional")
                .or_else(|| filter("MIN_NOTIONAL", "minNotional"))
                .unwrap_or_default(),
            symbol: s.symbol,
            base: s.base_asset,
            quote: s.quote_asset,
        })
    }

    pub async fn top(&self, symbol: &str) -> Result<BookTop> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Top {
            bid_price: String,
            ask_price: String,
        }
        let t: Top = self.public(&format!("/api/v3/ticker/bookTicker?symbol={symbol}")).await?;
        Ok(BookTop { bid: decimal(VENUE, &t.bid_price)?, ask: decimal(VENUE, &t.ask_price)? })
    }

    /// Free (not locked in orders) balance of one asset.
    pub async fn free(&self, key: &BinanceKey, asset: &str) -> Result<Decimal> {
        #[derive(Deserialize)]
        struct Account {
            balances: Vec<Bal>,
        }
        #[derive(Deserialize)]
        struct Bal {
            asset: String,
            free: String,
        }
        let a: Account = self.signed(Method::GET, key, "/api/v3/account", &[]).await?;
        match a.balances.iter().find(|b| b.asset == asset) {
            Some(b) => decimal(VENUE, &b.free),
            None => Ok(Decimal::ZERO),
        }
    }

    fn params(order: &SpotOrder) -> Vec<(&'static str, String)> {
        vec![
            ("symbol", order.symbol.clone()),
            ("side", if order.buy { "BUY" } else { "SELL" }.into()),
            ("type", "LIMIT".into()),
            (
                "timeInForce",
                match order.tif {
                    TimeInForce::Ioc => "IOC",
                    TimeInForce::Gtc => "GTC",
                }
                .into(),
            ),
            ("quantity", order.qty.normalize().to_string()),
            ("price", order.price.normalize().to_string()),
            ("newClientOrderId", order.client_id.clone()),
            ("newOrderRespType", "FULL".into()),
        ]
    }

    /// Validates an order with Binance without placing it.
    pub async fn test(&self, key: &BinanceKey, order: &SpotOrder) -> Result<()> {
        let _: serde_json::Value =
            self.signed(Method::POST, key, "/api/v3/order/test", &Self::params(order)).await?;
        Ok(())
    }

    pub async fn place(&self, key: &BinanceKey, order: &SpotOrder) -> Result<SpotFill> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Resp {
            order_id: i64,
            status: String,
            executed_qty: String,
            cummulative_quote_qty: String,
            #[serde(default)]
            fills: Vec<Fill>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Fill {
            commission: String,
            commission_asset: String,
        }
        let r: Resp = self.signed(Method::POST, key, "/api/v3/order", &Self::params(order)).await?;
        let executed = decimal(VENUE, &r.executed_qty)?;
        let quote = decimal(VENUE, &r.cummulative_quote_qty)?;
        let mut fee: Vec<(Decimal, String)> = Vec::new();
        for f in r.fills {
            let amount = decimal(VENUE, &f.commission)?;
            match fee.iter_mut().find(|(_, a)| *a == f.commission_asset) {
                Some((sum, _)) => *sum += amount,
                None => fee.push((amount, f.commission_asset)),
            }
        }
        Ok(SpotFill {
            order_id: r.order_id.to_string(),
            status: r.status,
            avg_price: (!executed.is_zero()).then(|| (quote / executed).normalize()),
            executed_qty: executed,
            quote_qty: quote,
            fee,
        })
    }
}

fn not_listed(symbol: &str) -> Error {
    Error::new(ErrorCode::MarketNotFound, format!("binance has no spot market {symbol}"))
        .with_hint(json!({ "venue": "binance", "query": symbol }))
}

fn api_error(status: StatusCode, err: Option<ApiError>) -> Error {
    let Some(err) = err else { return status_error(VENUE, status) };
    let (code, cause) = match err.code {
        -1121 => return not_listed("symbol"),
        -1021 => (ErrorCode::Credentials, "clock"),
        -2014 | -2015 => (ErrorCode::Credentials, "key_rejected"),
        -2010 if err.msg.contains("insufficient") => (ErrorCode::Venue, "insufficient_balance"),
        -2010 | -1013 | -1111 | -1100 | -1102 => (ErrorCode::Venue, "rejected"),
        _ if status == StatusCode::TOO_MANY_REQUESTS => return status_error(VENUE, status),
        _ => (ErrorCode::Venue, "rejected"),
    };
    Error::new(code, format!("binance: {}", err.msg))
        .with_hint(json!({ "venue": "binance", "cause": cause, "binance_code": err.code }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> SymbolRules {
        SymbolRules {
            symbol: "BTCUSDT".into(),
            base: "BTC".into(),
            quote: "USDT".into(),
            trading: true,
            step: Decimal::new(1, 5),
            min_qty: Decimal::new(1, 5),
            tick: Decimal::new(1, 2),
            min_notional: Decimal::from(5),
        }
    }

    #[test]
    fn rounds_to_exchange_rules() {
        let r = rules();
        assert_eq!(r.qty("0.0012345".parse().unwrap()).to_string(), "0.00123");
        // Buy bounds round down, sell bounds round up: never looser.
        assert_eq!(r.price("65000.129".parse().unwrap(), true).to_string(), "65000.12");
        assert_eq!(r.price("65000.121".parse().unwrap(), false).to_string(), "65000.13");
        assert_eq!(r.price("65000.12".parse().unwrap(), false).to_string(), "65000.12");
    }

    #[test]
    fn signs_like_the_binance_docs() {
        // The worked example from Binance's API documentation.
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(
            b"NhqPtmdSJYdKjVHjA7PZj4Mge3R5YNiP1e3UZjInClVN65XAbvqqM6A7H5fATj0j",
        )
        .unwrap();
        mac.update(b"symbol=LTCBTC&side=BUY&type=LIMIT&timeInForce=GTC&quantity=1&price=0.1&recvWindow=5000&timestamp=1499827319559");
        assert_eq!(
            hex::encode(mac.finalize().into_bytes()),
            "c8db56825ae71d6d79447849e617115f4a920fa2acdcab2b053c4b2838bd6b71"
        );
    }
}
