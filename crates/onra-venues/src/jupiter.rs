use onra_core::{Market, MarketKind, MarketQuery, Result, Ticker, VenueId};
use reqwest::Client;
use serde::Deserialize;

use crate::http::{decimal_f64, transport_error, unexpected};

const VENUE: VenueId = VenueId::Jupiter;
const API: &str = "https://lite-api.jup.ag";

pub struct Jupiter {
    http: Client,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Token {
    id: String,
    symbol: String,
    #[serde(default)]
    is_verified: bool,
    usd_price: Option<f64>,
    liquidity: Option<f64>,
    stats24h: Option<Stats>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stats {
    price_change: Option<f64>,
}

impl Jupiter {
    pub fn new(http: Client) -> Self {
        Self { http }
    }

    pub async fn ping(&self) -> Result<()> {
        const SOL: &str = "So11111111111111111111111111111111111111112";
        let resp = self
            .http
            .get(format!("{API}/price/v3"))
            .query(&[("ids", SOL)])
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?;
        if !resp.status().is_success() {
            return Err(unexpected(VENUE, format!("HTTP {}", resp.status())));
        }
        Ok(())
    }

    pub async fn ticker(&self, query: &MarketQuery) -> Result<Option<Ticker>> {
        let resp = self
            .http
            .get(format!("{API}/tokens/v2/search"))
            .query(&[("query", &query.base)])
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?;
        if !resp.status().is_success() {
            return Err(unexpected(VENUE, format!("HTTP {}", resp.status())));
        }
        let tokens: Vec<Token> = resp.json().await.map_err(|e| transport_error(VENUE, e))?;

        // Solana is full of tokens impersonating real tickers, so a bare
        // symbol only ever resolves to a verified token. Unverified tokens are
        // reachable by mint address and are flagged as such.
        //
        // Several verified tokens can share a symbol (there are two verified
        // WBTCs). For a read-only price the deepest pool is the sensible
        // answer and the mint is reported, but order routing must not guess.
        let token = if query.is_address() {
            tokens.into_iter().find(|t| t.id == query.base)
        } else {
            tokens
                .into_iter()
                .filter(|t| t.is_verified && t.symbol.eq_ignore_ascii_case(&query.base))
                .max_by(|a, b| a.liquidity.unwrap_or(0.0).total_cmp(&b.liquidity.unwrap_or(0.0)))
        };
        let Some(token) = token else {
            return Ok(None);
        };
        let Some(price) = token.usd_price.and_then(decimal_f64) else {
            return Ok(None);
        };

        let symbol = token.symbol.to_ascii_uppercase();
        Ok(Some(Ticker {
            market: Market {
                venue: VENUE,
                symbol: symbol.clone(),
                base: symbol,
                kind: MarketKind::Spot,
                venue_symbol: token.id,
                verified: token.is_verified,
            },
            price,
            change_24h_pct: token
                .stats24h
                .and_then(|s| s.price_change)
                .and_then(decimal_f64)
                .map(|d| d.round_dp(4)),
            low_24h: None,
            high_24h: None,
            trend_24h: Vec::new(),
        }))
    }
}
