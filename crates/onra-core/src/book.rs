//! Order books and funding rates.

use rust_decimal::Decimal;
use serde::Serialize;

use crate::{Market, VenueId};

#[derive(Debug, Clone, Serialize)]
pub struct Level {
    pub price: Decimal,
    pub size: Decimal,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderBook {
    pub market: Market,
    /// Best (highest) bid first.
    pub bids: Vec<Level>,
    /// Best (lowest) ask first.
    pub asks: Vec<Level>,
}

impl OrderBook {
    pub fn mid(&self) -> Option<Decimal> {
        Some((self.bids.first()?.price + self.asks.first()?.price) / Decimal::TWO)
    }

    pub fn spread(&self) -> Option<Decimal> {
        Some(self.asks.first()?.price - self.bids.first()?.price)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FundingRate {
    pub venue: VenueId,
    pub asset: String,
    /// The rate paid each interval, as a fraction (0.0001 = 0.01%).
    pub rate: Decimal,
    pub interval_hours: u32,
    /// `rate` compounded simply over a year, in percent, so venues with
    /// different intervals compare directly.
    pub apr_pct: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_interest_usd: Option<Decimal>,
}

impl FundingRate {
    pub fn new(
        venue: VenueId,
        asset: String,
        rate: Decimal,
        interval_hours: u32,
        open_interest_usd: Option<Decimal>,
    ) -> Self {
        let per_year = Decimal::from(24 * 365) / Decimal::from(interval_hours.max(1));
        let apr_pct = (rate * per_year * Decimal::ONE_HUNDRED).round_dp(2);
        Self { venue, asset, rate, interval_hours, apr_pct, open_interest_usd }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn annualizes_across_intervals() {
        // 0.00125% hourly and 0.01% every 8h are both 10.95% a year.
        let hourly = FundingRate::new(
            VenueId::Hyperliquid,
            "BTC".into(),
            Decimal::from_str("0.0000125").unwrap(),
            1,
            None,
        );
        let eight = FundingRate::new(
            VenueId::Binance,
            "BTC".into(),
            Decimal::from_str("0.0001").unwrap(),
            8,
            None,
        );
        assert_eq!(hourly.apr_pct, eight.apr_pct);
        assert_eq!(hourly.apr_pct, Decimal::from_str("10.95").unwrap());
    }
}
