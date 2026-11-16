use onra_core::{
    Error, ErrorCode, Market, MarketKind, MarketQuery, Result, SearchHit, Ticker, VenueId,
};
use reqwest::{Client, StatusCode};
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::http::{decimal, status_error, transport_error, unexpected};

const VENUE: VenueId = VenueId::Binance;
const API: &str = "https://api.binance.com/api/v3";
const DEFAULT_QUOTE: &str = "USDT";
/// Binance's error code for a symbol that doesn't exist.
const INVALID_SYMBOL: i64 = -1121;

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
