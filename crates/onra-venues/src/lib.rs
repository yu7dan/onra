//! Venue adapters. Each adapter translates one venue's API into
//! [`onra_core`] types; nothing venue-specific leaks past this crate.

mod binance;
mod http;
mod hyperliquid;
mod jupiter;

use onra_core::{MarketQuery, Result, Ticker, VenueId};

pub use http::NetConfig;

pub struct Venues {
    binance: binance::Binance,
    hyperliquid: hyperliquid::Hyperliquid,
    jupiter: jupiter::Jupiter,
}

impl Venues {
    pub fn new(net: &NetConfig) -> Result<Self> {
        let client = http::client(net)?;
        Ok(Self {
            binance: binance::Binance::new(client.clone()),
            hyperliquid: hyperliquid::Hyperliquid::new(client.clone()),
            jupiter: jupiter::Jupiter::new(client),
        })
    }

    /// A cheap request proving the venue is reachable.
    pub async fn ping(&self, venue: VenueId) -> Result<()> {
        match venue {
            VenueId::Binance => self.binance.ping().await,
            VenueId::Hyperliquid => self.hyperliquid.ping().await,
            VenueId::Jupiter => self.jupiter.ping().await,
        }
    }

    /// Look up `query` on `venue`. `Ok(None)` means the venue answered but
    /// doesn't list the market.
    pub async fn ticker(&self, venue: VenueId, query: &MarketQuery) -> Result<Option<Ticker>> {
        match venue {
            VenueId::Binance => self.binance.ticker(query).await,
            VenueId::Hyperliquid => self.hyperliquid.ticker(query).await,
            VenueId::Jupiter => self.jupiter.ticker(query).await,
        }
    }
}
