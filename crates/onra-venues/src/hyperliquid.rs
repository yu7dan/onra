use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use onra_core::{Market, MarketKind, MarketQuery, Result, Ticker, VenueId};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::OnceCell;

use crate::http::{decimal, pct_change, transport_error, unexpected};

const VENUE: VenueId = VenueId::Hyperliquid;
const INFO_URL: &str = "https://api.hyperliquid.xyz/info";

pub struct Hyperliquid {
    http: Client,
    /// Perp contexts keyed by uppercased coin name. Fetched once per process
    /// and shared by concurrent lookups.
    perps: OnceCell<HashMap<String, Perp>>,
}

struct Perp {
    /// Hyperliquid's own name; case matters (`kPEPE`).
    coin: String,
    ctx: AssetCtx,
}

#[derive(Deserialize)]
struct Meta {
    universe: Vec<Asset>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Asset {
    name: String,
    #[serde(default)]
    is_delisted: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetCtx {
    mark_px: String,
    mid_px: Option<String>,
    prev_day_px: String,
}

#[derive(Deserialize)]
struct Candle {
    c: String,
    h: String,
    l: String,
}

/// The last 24 hourly candles, reduced to what a ticker needs.
#[derive(Default)]
struct Day {
    closes: Vec<Decimal>,
    low: Option<Decimal>,
    high: Option<Decimal>,
}

impl Hyperliquid {
    pub fn new(http: Client) -> Self {
        Self { http, perps: OnceCell::new() }
    }

    async fn info<T: serde::de::DeserializeOwned>(&self, body: serde_json::Value) -> Result<T> {
        let resp = self
            .http
            .post(INFO_URL)
            .json(&body)
            .send()
            .await
            .map_err(|e| transport_error(VENUE, e))?;
        if !resp.status().is_success() {
            return Err(unexpected(VENUE, format!("HTTP {}", resp.status())));
        }
        resp.json().await.map_err(|e| transport_error(VENUE, e))
    }

    async fn perps(&self) -> Result<&HashMap<String, Perp>> {
        self.perps
            .get_or_try_init(|| async {
                let (meta, ctxs): (Meta, Vec<AssetCtx>) =
                    self.info(json!({ "type": "metaAndAssetCtxs" })).await?;
                Ok(meta
                    .universe
                    .into_iter()
                    .zip(ctxs)
                    .filter(|(asset, _)| !asset.is_delisted)
                    .map(|(asset, ctx)| {
                        (asset.name.to_ascii_uppercase(), Perp { coin: asset.name, ctx })
                    })
                    .collect())
            })
            .await
    }

    pub async fn ticker(&self, query: &MarketQuery) -> Result<Option<Ticker>> {
        let Some(perp) = self.perps().await?.get(&query.base) else {
            return Ok(None);
        };
        let price = decimal(VENUE, perp.ctx.mid_px.as_deref().unwrap_or(&perp.ctx.mark_px))?;
        let prev = decimal(VENUE, &perp.ctx.prev_day_px)?;
        // The sparkline is decoration: a failed candle fetch must not hide
        // the price.
        let day = self.day(&perp.coin).await.unwrap_or_default();

        Ok(Some(Ticker {
            market: Market {
                venue: VENUE,
                symbol: format!("{}-PERP", query.base),
                base: query.base.clone(),
                kind: MarketKind::Perp,
                venue_symbol: perp.coin.clone(),
                verified: true,
            },
            price,
            change_24h_pct: pct_change(prev, price),
            low_24h: day.low,
            high_24h: day.high,
            trend_24h: day.closes,
        }))
    }

    async fn day(&self, coin: &str) -> Result<Day> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default();
        let candles: Vec<Candle> = self
            .info(json!({
                "type": "candleSnapshot",
                "req": {
                    "coin": coin,
                    "interval": "1h",
                    "startTime": now.saturating_sub(24 * 3600 * 1000),
                    "endTime": now,
                },
            }))
            .await?;
        // The range can include the current, unfinished hour; keep the last
        // 24 so every venue draws the same window.
        let skip = candles.len().saturating_sub(24);
        let mut day = Day::default();
        for candle in &candles[skip..] {
            day.closes.push(decimal(VENUE, &candle.c)?);
            let (h, l) = (decimal(VENUE, &candle.h)?, decimal(VENUE, &candle.l)?);
            day.high = Some(day.high.map_or(h, |x| x.max(h)));
            day.low = Some(day.low.map_or(l, |x| x.min(l)));
        }
        Ok(day)
    }
}
