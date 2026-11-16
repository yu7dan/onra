use onra_core::{Error, ErrorCode, Market, MarketKind, MarketQuery, Result, Ticker, VenueId};
use reqwest::{Client, StatusCode};
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::http::{decimal, transport_error, unexpected};

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
            s => return Err(unexpected(VENUE, format!("HTTP {s}"))),
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
