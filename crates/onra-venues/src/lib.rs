//! Venue adapters. Each adapter translates one venue's API into
//! [`onra_core`] types; nothing venue-specific leaks past this crate.

mod binance;
mod http;
mod hyperliquid;
mod jupiter;

use onra_core::{Market, MarketQuery, Result, SearchHit, Ticker, VenueId};
use rust_decimal::Decimal;

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

    /// Latest price of an already-resolved market. Much lighter than
    /// [`Venues::ticker`]: meant for polling.
    pub async fn quote(&self, market: &Market) -> Result<Decimal> {
        match market.venue {
            VenueId::Binance => self.binance.quote(&market.venue_symbol).await,
            VenueId::Hyperliquid => self.hyperliquid.quote(&market.venue_symbol).await,
            VenueId::Jupiter => self.jupiter.quote(&market.venue_symbol).await,
        }
    }

    /// Markets on `venue` matching a free-text `query`.
    pub async fn search(&self, venue: VenueId, query: &str) -> Result<Vec<SearchHit>> {
        match venue {
            VenueId::Binance => self.binance.search(query).await,
            VenueId::Hyperliquid => self.hyperliquid.search(query).await,
            VenueId::Jupiter => self.jupiter.search(query).await,
        }
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
