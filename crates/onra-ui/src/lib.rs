//! Terminal rendering for Onra.
//!
//! Design rules (see the visual spec): one accent color (sunrise amber), up /
//! down colors reserved for price movement, everything else in neutral grays.
//! Numbers are right-aligned with consistent precision per asset.

mod banner;
pub mod format;
mod spark;
mod table;
mod theme;

pub use banner::banner;
pub use spark::sparkline;
pub use table::{Align, Cell, Table};
pub use theme::{ColorMode, Convention, Theme};
