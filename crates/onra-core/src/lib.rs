//! Onra domain model.
//!
//! Everything here is venue-agnostic: the CLI, the renderers and the venue
//! adapters all speak these types.

mod account;
mod book;
mod error;
mod market;
mod ticker;

pub use account::{Balance, Order, OrderSide, Position, PositionSide};
pub use book::{FundingRate, Level, OrderBook};
pub use error::{Error, ErrorCode, Result};
pub use market::{MarketKind, MarketQuery, VenueId};
pub use ticker::{Market, SearchHit, Ticker};
