use onra_core::{
    Balance, Error, ErrorCode, Market, MarketKind, MarketQuery, Order, OrderSide, Result,
    SearchHit, Ticker, VenueId,
};
use reqwest::{Client, StatusCode};
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::http::{decimal, status_error, transport_error, unexpected};

const VENUE: VenueId = VenueId::Binance;
const HOST: &str = "https://api.binance.com";
const API: &str = "https://api.binance.com/api/v3";
const STABLES: [&str; 5] = ["USDT", "USDC", "FDUSD", "BUSD", "DAI"];
const DEFAULT_QUOTE: &str = "USDT";
/// Binance's error code for a symbol that doesn't exist.
const INVALID_SYMBOL: i64 = -1121;

/// An API key and its secret. The secret is wiped from memory on drop.
pub struct BinanceKey {
    pub key: String,
    pub secret: zeroize::Zeroizing<String>,
}

/// What a key is allowed to do, as Binance reports it.
#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Restrictions {
    pub enable_reading: bool,
    pub enable_withdrawals: bool,
    #[serde(default)]
    pub enable_spot_and_margin_trading: bool,
    #[serde(default)]
    pub ip_restrict: bool,
}

pub struct Binance {
    http: Client,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ticker24h {
    last_price: String,
    price_change_percent: String,
    high_price: String,
    low_price: String,
}

#[derive(Deserialize)]
struct ApiError {
    code: i64,
    msg: String,
}

impl Binance {
    pub fn new(http: Client) -> Self {
        Self { http }
    }

    pub async fn ping(&self) -> Result<()> {
        let resp = self
            .http
            .get(format!("{API}/ping"))
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?;
        if !resp.status().is_success() {
            return Err(status_error(VENUE, resp.status()));
        }
        Ok(())
    }

    /// Last price only, for polling an already-resolved market.
    pub async fn quote(&self, pair: &str) -> Result<Decimal> {
        #[derive(Deserialize)]
        struct Price {
            price: String,
        }
        let resp = self
            .http
            .get(format!("{API}/ticker/price"))
            .query(&[("symbol", pair)])
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?;
        if !resp.status().is_success() {
            return Err(status_error(VENUE, resp.status()));
        }
        let p: Price = resp.json().await.map_err(|e| transport_error(VENUE, e))?;
        decimal(VENUE, &p.price)
    }

    /// Spot pairs whose base matches `query`, most relevant first.
    pub async fn search(&self, query: &str) -> Result<Vec<SearchHit>> {
        // In display priority: the deepest books first. None of these is a
        // suffix of another, so matching order doesn't matter.
        const QUOTES: [&str; 6] = ["USDT", "USDC", "FDUSD", "BTC", "ETH", "BNB"];
        #[derive(Deserialize)]
        struct Price {
            symbol: String,
            price: String,
        }
        let resp = self
            .http
            .get(format!("{API}/ticker/price"))
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?;
        if !resp.status().is_success() {
            return Err(status_error(VENUE, resp.status()));
        }
        let all: Vec<Price> = resp.json().await.map_err(|e| transport_error(VENUE, e))?;

        let q = query.to_ascii_uppercase();
        let mut hits: Vec<(bool, usize, SearchHit)> = Vec::new();
        for p in all {
            let Some((qi, quote)) = QUOTES
                .iter()
                .enumerate()
                .find(|(_, quote)| p.symbol.len() > quote.len() && p.symbol.ends_with(*quote))
            else {
                continue;
            };
            let base = &p.symbol[..p.symbol.len() - quote.len()];
            let exact = base == q;
            if !exact && !(q.len() >= 2 && base.starts_with(&q)) {
                continue;
            }
            // Delisted pairs report a zero price.
            let Some(price) = decimal(VENUE, &p.price).ok().filter(|d| !d.is_zero()) else {
                continue;
            };
            hits.push((
                !exact,
                qi,
                SearchHit {
                    market: Market {
                        venue: VENUE,
                        symbol: format!("{base}/{quote}"),
                        base: base.to_string(),
                        kind: MarketKind::Spot,
                        venue_symbol: p.symbol.clone(),
                        verified: true,
                    },
                    name: None,
                    price: Some(price),
                    liquidity_usd: None,
                    holders: None,
                },
            ));
        }
        hits.sort_by(|a, b| {
            (a.0, a.1, a.2.market.base.len()).cmp(&(b.0, b.1, b.2.market.base.len()))
        });
        Ok(hits.into_iter().take(4).map(|(_, _, h)| h).collect())
    }

    pub async fn ticker(&self, query: &MarketQuery) -> Result<Option<Ticker>> {
        let quote = query.quote.as_deref().unwrap_or(DEFAULT_QUOTE);
        let pair = format!("{}{quote}", query.base);

        let resp = self
            .http
            .get(format!("{API}/ticker/24hr"))
            .query(&[("symbol", &pair)])
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?;

        match resp.status() {
            s if s.is_success() => {}
            StatusCode::BAD_REQUEST => {
                let err: ApiError = resp.json().await.map_err(|e| transport_error(VENUE, e))?;
                if err.code == INVALID_SYMBOL {
                    return Ok(None);
                }
                return Err(unexpected(VENUE, err.msg));
            }
            StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS | StatusCode::FORBIDDEN => {
                return Err(Error::new(
                    ErrorCode::Venue,
                    "binance is not available from your current location",
                ));
            }
            s => return Err(status_error(VENUE, s)),
        }

        let t: Ticker24h = resp.json().await.map_err(|e| transport_error(VENUE, e))?;
        let trend = self.hourly_closes(&pair).await.unwrap_or_default();

        Ok(Some(Ticker {
            market: Market {
                venue: VENUE,
                symbol: format!("{}/{quote}", query.base),
                base: query.base.clone(),
                kind: MarketKind::Spot,
                venue_symbol: pair,
                verified: true,
            },
            price: decimal(VENUE, &t.last_price)?,
            change_24h_pct: Some(decimal(VENUE, &t.price_change_percent)?),
            low_24h: Some(decimal(VENUE, &t.low_price)?),
            high_24h: Some(decimal(VENUE, &t.high_price)?),
            trend_24h: trend,
        }))
    }

    async fn hourly_closes(&self, pair: &str) -> Result<Vec<Decimal>> {
        // Kline rows are positional arrays; index 4 is the close.
        let rows: Vec<Vec<serde_json::Value>> = self
            .http
            .get(format!("{API}/klines"))
            .query(&[("symbol", pair), ("interval", "1h"), ("limit", "24")])
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?
            .json()
            .await
            .map_err(|e| transport_error(VENUE, e))?;
        rows.iter()
            .map(|row| match row.get(4).and_then(|v| v.as_str()) {
                Some(close) => decimal(VENUE, close),
                None => Err(unexpected(VENUE, "malformed kline")),
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Signed account endpoints (HMAC-SHA256 over the query string).

#[derive(Deserialize)]
struct AccountInfo {
    balances: Vec<AssetBalance>,
}

#[derive(Deserialize)]
struct AssetBalance {
    asset: String,
    free: String,
    locked: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenOrder {
    symbol: String,
    order_id: u64,
    price: String,
    orig_qty: String,
    executed_qty: String,
    #[serde(rename = "type")]
    kind: String,
    side: String,
    time: u64,
}

/// `BTCUSDT` as `BTC/USDT` when the quote is recognizable.
fn pair_symbol(pair: &str) -> String {
    const QUOTES: [&str; 6] = ["USDT", "USDC", "FDUSD", "BTC", "ETH", "BNB"];
    QUOTES
        .iter()
        .find(|q| pair.len() > q.len() && pair.ends_with(*q))
        .map(|q| format!("{}/{q}", &pair[..pair.len() - q.len()]))
        .unwrap_or_else(|| pair.to_string())
}

impl Binance {
    async fn signed<T: serde::de::DeserializeOwned>(
        &self,
        key: &BinanceKey,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        use hmac::{Hmac, Mac};
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let mut query: String = params.iter().map(|(k, v)| format!("{k}={v}&")).collect();
        query.push_str(&format!("timestamp={ts}&recvWindow=10000"));
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key.secret.as_bytes())
            .map_err(|_| Error::new(ErrorCode::Credentials, "invalid binance secret"))?;
        mac.update(query.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());

        let resp = self
            .http
            .get(format!("{HOST}{path}?{query}&signature={signature}"))
            .header("X-MBX-APIKEY", &key.key)
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?;
        let status = resp.status();
        if status.is_success() {
            return resp.json().await.map_err(|e| transport_error(VENUE, e));
        }
        let err: Option<ApiError> = resp.json().await.ok();
        Err(match err.map(|e| e.code) {
            // Clock skew: the request timestamp fell outside recvWindow.
            Some(-1021) => Error::new(ErrorCode::Credentials, "binance rejected the request time; check the system clock")
                .with_hint(serde_json::json!({ "venue": "binance", "cause": "clock" })),
            Some(-2014 | -2015) => Error::new(ErrorCode::Credentials, "binance rejected the API key (invalid, expired, or IP not allowed)")
                .with_hint(serde_json::json!({ "venue": "binance", "cause": "key_rejected", "suggest": "onra account login bn" })),
            _ if status == StatusCode::UNAUTHORIZED => Error::new(ErrorCode::Credentials, "binance rejected the API key")
                .with_hint(serde_json::json!({ "venue": "binance", "suggest": "onra account login bn" })),
            _ => status_error(VENUE, status),
        })
    }

    pub async fn restrictions(&self, key: &BinanceKey) -> Result<Restrictions> {
        self.signed(key, "/sapi/v1/account/apiRestrictions", &[]).await
    }

    pub async fn balances(&self, key: &BinanceKey) -> Result<Vec<Balance>> {
        let (account, prices) = tokio::try_join!(
            self.signed::<AccountInfo>(key, "/api/v3/account", &[("omitZeroBalances", "true")]),
            self.all_prices(),
        )?;
        let mut out = Vec::new();
        for b in account.balances {
            let free = decimal(VENUE, &b.free)?;
            let total = free + decimal(VENUE, &b.locked)?;
            if total.is_zero() {
                continue;
            }
            // Simple Earn positions show up as LD-prefixed assets.
            let (asset, wallet) = match b.asset.strip_prefix("LD") {
                Some(underlying) if !underlying.is_empty() => (underlying.to_string(), "earn"),
                _ => (b.asset.clone(), "spot"),
            };
            let usd_value = if STABLES.contains(&asset.as_str()) {
                Some(total)
            } else {
                prices.get(&format!("{asset}USDT")).map(|p| (total * p).round_dp(2))
            };
            out.push(Balance {
                venue: VENUE,
                asset,
                wallet: Some(wallet.into()),
                total,
                available: if wallet == "spot" { free } else { Decimal::ZERO },
                usd_value,
                mint: None,
                verified: true,
            });
        }
        Ok(out)
    }

    pub async fn orders(&self, key: &BinanceKey) -> Result<Vec<Order>> {
        let orders: Vec<OpenOrder> = self.signed(key, "/api/v3/openOrders", &[]).await?;
        orders
            .into_iter()
            .map(|o| {
                let size = decimal(VENUE, &o.orig_qty)?;
                let price = decimal(VENUE, &o.price)?;
                Ok(Order {
                    venue: VENUE,
                    id: o.order_id.to_string(),
                    symbol: pair_symbol(&o.symbol),
                    side: if o.side == "BUY" { OrderSide::Buy } else { OrderSide::Sell },
                    kind: o.kind.to_ascii_lowercase().replace('_', " "),
                    price: (!price.is_zero()).then_some(price),
                    size,
                    filled: decimal(VENUE, &o.executed_qty)?,
                    created_ms: o.time,
                })
            })
            .collect()
    }

    async fn all_prices(&self) -> Result<std::collections::HashMap<String, Decimal>> {
        #[derive(Deserialize)]
        struct Price {
            symbol: String,
            price: String,
        }
        let resp = self
            .http
            .get(format!("{API}/ticker/price"))
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?;
        if !resp.status().is_success() {
            return Err(status_error(VENUE, resp.status()));
        }
        let all: Vec<Price> = resp.json().await.map_err(|e| transport_error(VENUE, e))?;
        Ok(all
            .into_iter()
            .filter_map(|p| Some((p.symbol, decimal(VENUE, &p.price).ok()?)))
            .collect())
    }
}
