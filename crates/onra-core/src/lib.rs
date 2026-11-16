//! Onra domain model.
//!
//! Everything here is venue-agnostic: the CLI, the renderers and the venue
//! adapters all speak these types.

mod error;
mod market;
mod ticker;

pub use error::{Error, ErrorCode, Result};
pub use market::{MarketKind, MarketQuery, VenueId};
pub use ticker::{Market, SearchHit, Ticker};
