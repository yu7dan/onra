//! What an account holds: balances, open positions, resting orders.

use rust_decimal::Decimal;
use serde::Serialize;

use crate::VenueId;

#[derive(Debug, Clone, Serialize)]
pub struct Balance {
    pub venue: VenueId,
    pub asset: String,
    /// Where on the venue this lives when a venue has several (e.g. spot
    /// versus perps margin on Hyperliquid).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wallet: Option<String>,
    pub total: Decimal,
    pub available: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usd_value: Option<Decimal>,
    /// Token mint, for on-chain assets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mint: Option<String>,
    pub verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PositionSide {
    Long,
    Short,
}

#[derive(Debug, Clone, Serialize)]
pub struct Position {
    pub venue: VenueId,
    pub symbol: String,
    pub side: PositionSide,
    /// Absolute size in base units.
    pub size: Decimal,
    pub entry_price: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mark_price: Option<Decimal>,
    pub unrealized_pnl: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leverage: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liquidation_price: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin: Option<Decimal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Serialize)]
pub struct Order {
    pub venue: VenueId,
    pub id: String,
    pub symbol: String,
    pub side: OrderSide,
    /// The venue's order type, normalized to lowercase (`limit`, `stop market`, ...).
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<Decimal>,
    pub size: Decimal,
    pub filled: Decimal,
    /// Milliseconds since the Unix epoch.
    pub created_ms: u64,
}
