use rust_decimal::Decimal;
use serde::Serialize;

use crate::{MarketKind, VenueId};

/// A concrete, resolved market on one venue.
#[derive(Debug, Clone, Serialize)]
pub struct Market {
    pub venue: VenueId,
    /// Canonical Onra symbol, e.g. `SOL/USDT`, `SOL-PERP`, `SOL`.
    pub symbol: String,
    pub base: String,
    pub kind: MarketKind,
    /// The venue's own identifier: `SOLUSDT`, `SOL`, or a mint address.
    pub venue_symbol: String,
    /// False when the venue does not vouch for this asset (e.g. an
    /// unverified Solana token). Renderers must flag these.
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ticker {
    pub market: Market,
    pub price: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_24h_pct: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low_24h: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high_24h: Option<Decimal>,
    /// Hourly closes over the last 24h, oldest first. Empty when the venue
    /// has no candle data.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trend_24h: Vec<Decimal>,
}
