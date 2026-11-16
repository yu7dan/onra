//! Terminal rendering for Onra.
//!
//! Design rules (see the visual spec): one accent color (sunrise amber), up /
//! down colors reserved for price movement, everything else in neutral grays.
//! Numbers are right-aligned with consistent precision per asset.

mod canvas;
pub mod format;
mod logo;
pub mod motion;
mod spark;
mod table;
mod theme;

pub use logo::logo;
pub use spark::{sparkline, sparkline_styled};
pub use table::{Align, Cell, Table};
pub use theme::{Color, ColorMode, Convention, Rgb, Theme, palette};
