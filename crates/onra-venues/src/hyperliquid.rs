use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use onra_core::{
    Balance, Market, MarketKind, MarketQuery, Order, OrderSide, Position, PositionSide, Result,
    SearchHit, Ticker, VenueId,
};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::http::{decimal, pct_change, status_error, transport_error, unexpected};

const VENUE: VenueId = VenueId::Hyperliquid;
const INFO_URL: &str = "https://api.hyperliquid.xyz/info";

pub struct Hyperliquid {
    http: Client,
    /// Perp contexts keyed by uppercased coin name. One snapshot serves all
    /// lookups within a short window (a multi-asset `price` is one request),
    /// but a long-lived session still sees fresh prices.
    perps: Mutex<Option<Snapshot>>,
}

/// When it was taken, and the perps by uppercased coin name.
type Snapshot = (Instant, Arc<HashMap<String, Perp>>);

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
        Self { http, perps: Mutex::new(None) }
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
            return Err(status_error(VENUE, resp.status()));
        }
        resp.json().await.map_err(|e| transport_error(VENUE, e))
    }

    async fn perps(&self) -> Result<Arc<HashMap<String, Perp>>> {
        const FRESH: Duration = Duration::from_millis(1500);
        // Holding the lock while fetching makes concurrent callers share one
        // request instead of racing.
        let mut cache = self.perps.lock().await;
        if let Some((at, perps)) = cache.as_ref()
            && at.elapsed() < FRESH
        {
            return Ok(perps.clone());
        }
        let (meta, ctxs): (Meta, Vec<AssetCtx>) =
            self.info(json!({ "type": "metaAndAssetCtxs" })).await?;
        let perps: Arc<HashMap<_, _>> = Arc::new(
            meta.universe
                .into_iter()
                .zip(ctxs)
                .filter(|(asset, _)| !asset.is_delisted)
                .map(|(asset, ctx)| {
                    (asset.name.to_ascii_uppercase(), Perp { coin: asset.name, ctx })
                })
                .collect(),
        );
        *cache = Some((Instant::now(), perps.clone()));
        Ok(perps)
    }

    /// Mid price for a coin, from the shared short-lived snapshot.
    pub async fn quote(&self, coin: &str) -> Result<Decimal> {
        let perps = self.perps().await?;
        let perp = perps
            .get(&coin.to_ascii_uppercase())
            .ok_or_else(|| unexpected(VENUE, format!("unknown coin {coin}")))?;
        decimal(VENUE, perp.ctx.mid_px.as_deref().unwrap_or(&perp.ctx.mark_px))
    }

    /// Perps matching `query`, including thousand-unit contracts (`kBONK`).
    pub async fn search(&self, query: &str) -> Result<Vec<SearchHit>> {
        let q = query.to_ascii_uppercase();
        let thousand = format!("K{q}");
        let perps = self.perps().await?;
        let mut hits: Vec<(bool, SearchHit)> = perps
            .iter()
            .filter(|(key, _)| {
                **key == q || **key == thousand || (q.len() >= 2 && key.starts_with(&q))
            })
            .map(|(key, perp)| {
                let price = perp.ctx.mid_px.as_deref().unwrap_or(&perp.ctx.mark_px);
                let hit = SearchHit {
                    market: Market {
                        venue: VENUE,
                        symbol: format!("{}-PERP", perp.coin),
                        base: perp.coin.clone(),
                        kind: MarketKind::Perp,
                        venue_symbol: perp.coin.clone(),
                        verified: true,
                    },
                    name: None,
                    price: decimal(VENUE, price).ok(),
                    liquidity_usd: None,
                    holders: None,
                };
                (*key != q && *key != thousand, hit)
            })
            .collect();
        hits.sort_by(|a, b| (a.0, &a.1.market.symbol).cmp(&(b.0, &b.1.market.symbol)));
        Ok(hits.into_iter().take(6).map(|(_, h)| h).collect())
    }

    pub async fn ping(&self) -> Result<()> {
        self.perps().await.map(|_| ())
    }

    pub async fn ticker(&self, query: &MarketQuery) -> Result<Option<Ticker>> {
        let perps = self.perps().await?;
        let Some(perp) = perps.get(&query.base) else {
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

// ---------------------------------------------------------------------------
// Accounts. Reading needs only the address; nothing here can move funds.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Clearinghouse {
    margin_summary: MarginSummary,
    withdrawable: String,
    asset_positions: Vec<AssetPosition>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarginSummary {
    account_value: String,
}

#[derive(Deserialize)]
struct AssetPosition {
    position: PerpPosition,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PerpPosition {
    coin: String,
    szi: String,
    leverage: Leverage,
    entry_px: String,
    position_value: String,
    unrealized_pnl: String,
    liquidation_px: Option<String>,
    margin_used: String,
}

#[derive(Deserialize)]
struct Leverage {
    value: u32,
}

#[derive(Deserialize)]
struct SpotMeta {
    tokens: Vec<SpotToken>,
    universe: Vec<SpotPair>,
}

#[derive(Deserialize)]
struct SpotToken {
    name: String,
    index: usize,
}

#[derive(Deserialize)]
struct SpotPair {
    name: String,
    tokens: [usize; 2],
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpotCtx {
    coin: String,
    mark_px: String,
    mid_px: Option<String>,
    day_ntl_vlm: String,
}

#[derive(Deserialize)]
struct SpotState {
    balances: Vec<SpotBalance>,
}

#[derive(Deserialize)]
struct SpotBalance {
    coin: String,
    total: String,
    hold: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenOrder {
    coin: String,
    side: String,
    limit_px: String,
    sz: String,
    orig_sz: String,
    oid: u64,
    timestamp: u64,
    order_type: String,
    is_trigger: bool,
    trigger_px: String,
}

/// Perp coins trade as `ETH-PERP`; spot pairs (`PURR/USDC`, `@107`) keep
/// Hyperliquid's own name.
fn order_symbol(coin: &str) -> String {
    if coin.contains('/') || coin.starts_with('@') {
        coin.to_string()
    } else {
        format!("{coin}-PERP")
    }
}

impl Hyperliquid {
    /// USD price per spot token, from its most traded USDC pair.
    async fn spot_prices(&self) -> Result<HashMap<String, Decimal>> {
        const USDC: usize = 0;
        let (meta, ctxs): (SpotMeta, Vec<SpotCtx>) =
            self.info(json!({ "type": "spotMetaAndAssetCtxs" })).await?;
        let names: HashMap<usize, &str> =
            meta.tokens.iter().map(|t| (t.index, t.name.as_str())).collect();
        let ctx_by_coin: HashMap<&str, &SpotCtx> =
            ctxs.iter().map(|c| (c.coin.as_str(), c)).collect();
        let mut best: HashMap<String, (Decimal, Decimal)> = HashMap::new();
        for pair in &meta.universe {
            let [base, quote] = pair.tokens;
            let (Some(ctx), Some(name)) = (ctx_by_coin.get(pair.name.as_str()), names.get(&base))
            else {
                continue;
            };
            if quote != USDC {
                continue;
            }
            let (Ok(price), Ok(volume)) = (
                decimal(VENUE, ctx.mid_px.as_deref().unwrap_or(&ctx.mark_px)),
                decimal(VENUE, &ctx.day_ntl_vlm),
            ) else {
                continue;
            };
            let entry = best.entry(name.to_string()).or_insert((price, volume));
            if volume > entry.1 {
                *entry = (price, volume);
            }
        }
        let mut prices: HashMap<String, Decimal> =
            best.into_iter().map(|(k, (p, _))| (k, p)).collect();
        prices.insert("USDC".into(), Decimal::ONE);
        Ok(prices)
    }

    pub async fn balances(&self, address: &str) -> Result<Vec<Balance>> {
        let (perps, spot, prices): (Clearinghouse, SpotState, _) = tokio::try_join!(
            self.info(json!({ "type": "clearinghouseState", "user": address })),
            self.info(json!({ "type": "spotClearinghouseState", "user": address })),
            async { Ok(self.spot_prices().await.unwrap_or_default()) },
        )?;
        let mut out = Vec::new();
        let margin = decimal(VENUE, &perps.margin_summary.account_value)?;
        if !margin.is_zero() {
            out.push(Balance {
                venue: VENUE,
                asset: "USDC".into(),
                wallet: Some("perps".into()),
                total: margin,
                available: decimal(VENUE, &perps.withdrawable)?,
                usd_value: Some(margin),
                mint: None,
                verified: true,
            });
        }
        for b in spot.balances {
            let total = decimal(VENUE, &b.total)?;
            if total.is_zero() {
                continue;
            }
            let hold = decimal(VENUE, &b.hold)?;
            let usd_value = prices.get(&b.coin).map(|p| (total * p).round_dp(2));
            out.push(Balance {
                venue: VENUE,
                asset: b.coin,
                wallet: Some("spot".into()),
                total,
                available: total - hold,
                usd_value,
                mint: None,
                verified: true,
            });
        }
        Ok(out)
    }

    pub async fn positions(&self, address: &str) -> Result<Vec<Position>> {
        let state: Clearinghouse =
            self.info(json!({ "type": "clearinghouseState", "user": address })).await?;
        state
            .asset_positions
            .into_iter()
            .map(|ap| {
                let p = ap.position;
                let szi = decimal(VENUE, &p.szi)?;
                let size = szi.abs();
                let value = decimal(VENUE, &p.position_value)?;
                Ok(Position {
                    venue: VENUE,
                    symbol: format!("{}-PERP", p.coin),
                    side: if szi.is_sign_negative() {
                        PositionSide::Short
                    } else {
                        PositionSide::Long
                    },
                    size,
                    entry_price: decimal(VENUE, &p.entry_px)?,
                    mark_price: (!size.is_zero()).then(|| (value / size).round_dp(8).normalize()),
                    unrealized_pnl: decimal(VENUE, &p.unrealized_pnl)?,
                    leverage: Some(Decimal::from(p.leverage.value)),
                    liquidation_price: p
                        .liquidation_px
                        .as_deref()
                        .map(|l| decimal(VENUE, l))
                        .transpose()?,
                    margin: Some(decimal(VENUE, &p.margin_used)?),
                })
            })
            .collect()
    }

    pub async fn orders(&self, address: &str) -> Result<Vec<Order>> {
        let orders: Vec<OpenOrder> =
            self.info(json!({ "type": "frontendOpenOrders", "user": address })).await?;
        orders
            .into_iter()
            .map(|o| {
                let size = decimal(VENUE, &o.sz)?;
                let price = if o.is_trigger { &o.trigger_px } else { &o.limit_px };
                Ok(Order {
                    venue: VENUE,
                    id: o.oid.to_string(),
                    symbol: order_symbol(&o.coin),
                    side: if o.side == "B" { OrderSide::Buy } else { OrderSide::Sell },
                    kind: o.order_type.to_ascii_lowercase(),
                    price: Some(decimal(VENUE, price)?),
                    size,
                    filled: (decimal(VENUE, &o.orig_sz)? - size).max(Decimal::ZERO),
                    created_ms: o.timestamp,
                })
            })
            .collect()
    }
}
