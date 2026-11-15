use std::fmt;
use std::str::FromStr;

use serde::Serialize;

use crate::{Error, ErrorCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VenueId {
    Binance,
    Hyperliquid,
    Jupiter,
}

impl VenueId {
    pub const ALL: [VenueId; 3] = [VenueId::Binance, VenueId::Hyperliquid, VenueId::Jupiter];

    /// Short alias used in market identifiers, e.g. `SOL@jup`.
    pub fn alias(self) -> &'static str {
        match self {
            VenueId::Binance => "bn",
            VenueId::Hyperliquid => "hl",
            VenueId::Jupiter => "jup",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            VenueId::Binance => "binance",
            VenueId::Hyperliquid => "hyperliquid",
            VenueId::Jupiter => "jupiter",
        }
    }
}

impl fmt::Display for VenueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for VenueId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        let s = s.to_ascii_lowercase();
        VenueId::ALL.into_iter().find(|v| v.alias() == s || v.name() == s).ok_or_else(|| {
            Error::new(ErrorCode::UnknownVenue, format!("unknown venue `{s}`"))
                .with_hint(serde_json::json!({ "venues": VenueId::ALL.map(|v| v.alias()) }))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MarketKind {
    Spot,
    Perp,
}

/// A parsed market identifier: `<base>[/<quote>][-PERP][@<venue>]`.
///
/// | input         | meaning                              |
/// |---------------|--------------------------------------|
/// | `SOL`         | SOL on every venue that lists it     |
/// | `SOL@jup`     | SOL spot on Jupiter                  |
/// | `ETH-PERP@hl` | ETH perpetual on Hyperliquid         |
/// | `BTC/USDT@bn` | BTC/USDT spot on Binance             |
/// | `<mint>@jup`  | a Solana token by mint address       |
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketQuery {
    pub base: String,
    pub quote: Option<String>,
    pub kind: Option<MarketKind>,
    pub venue: Option<VenueId>,
}

impl MarketQuery {
    /// Solana mint addresses are base58 and 32–44 chars long. Tickers are
    /// never that long, so length alone is a reliable discriminator.
    pub fn is_address(&self) -> bool {
        self.base.len() >= 32
    }

    /// Whether `venue` could possibly list this market. Used to avoid asking
    /// venues for things they can't have (e.g. perps on Jupiter).
    pub fn applies_to(&self, venue: VenueId) -> bool {
        if let Some(v) = self.venue {
            return v == venue;
        }
        if self.is_address() {
            return venue == VenueId::Jupiter;
        }
        match (self.kind, venue) {
            (Some(MarketKind::Perp), v) => v == VenueId::Hyperliquid,
            // An explicit quote currency only means something on an order-book
            // spot venue; perps and Jupiter are USD-denominated.
            (_, VenueId::Binance) => true,
            _ => self.quote.is_none(),
        }
    }
}

impl FromStr for MarketQuery {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self, Error> {
        let usage = |why: &str| {
            Error::new(ErrorCode::Usage, format!("invalid market `{input}`: {why}")).with_hint(
                serde_json::json!({
                    "format": "<base>[/<quote>][-PERP][@<venue>]",
                    "examples": ["SOL", "SOL@jup", "ETH-PERP@hl", "BTC/USDT@bn"],
                }),
            )
        };

        let (symbol, venue) = match input.split_once('@') {
            Some((s, v)) => (s, Some(v.parse::<VenueId>()?)),
            None => (input, None),
        };
        if symbol.is_empty() {
            return Err(usage("missing symbol"));
        }

        // `-PERP` is ASCII, so if the uppercased string ends with it the last
        // five bytes are a valid split point in the original too.
        let (symbol, kind) = if symbol.to_ascii_uppercase().ends_with("-PERP") {
            (&symbol[..symbol.len() - 5], Some(MarketKind::Perp))
        } else {
            (symbol, None)
        };

        let (base, quote) = match symbol.split_once('/') {
            Some((b, q)) if !b.is_empty() && !q.is_empty() => (b, Some(q.to_ascii_uppercase())),
            Some(_) => return Err(usage("expected <base>/<quote>")),
            None => (symbol, None),
        };
        if !base.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(usage("symbols may only contain letters and digits"));
        }

        let kind = kind.or(quote.as_ref().map(|_| MarketKind::Spot));
        let mut query = MarketQuery { base: base.to_string(), quote, kind, venue };
        // Mint addresses are case-sensitive; tickers are not.
        if !query.is_address() {
            query.base = query.base.to_ascii_uppercase();
        }
        Ok(query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> MarketQuery {
        s.parse().unwrap()
    }

    #[test]
    fn parses_identifiers() {
        assert_eq!(
            q("sol"),
            MarketQuery { base: "SOL".into(), quote: None, kind: None, venue: None }
        );
        assert_eq!(q("SOL@jup").venue, Some(VenueId::Jupiter));
        let perp = q("eth-perp@HL");
        assert_eq!((perp.base.as_str(), perp.kind), ("ETH", Some(MarketKind::Perp)));
        assert_eq!(perp.venue, Some(VenueId::Hyperliquid));
        let pair = q("btc/usdt@bn");
        assert_eq!(pair.quote.as_deref(), Some("USDT"));
        assert_eq!(pair.kind, Some(MarketKind::Spot));
    }

    #[test]
    fn keeps_mint_case() {
        let mint = "So11111111111111111111111111111111111111112";
        let m = q(mint);
        assert_eq!(m.base, mint);
        assert!(m.is_address());
        assert!(m.applies_to(VenueId::Jupiter));
        assert!(!m.applies_to(VenueId::Binance));
    }

    #[test]
    fn rejects_bad_input() {
        for bad in ["", "@hl", "SOL@nowhere", "SO$L", "/USDT", "SOL/"] {
            assert!(bad.parse::<MarketQuery>().is_err(), "{bad} should fail");
        }
        assert_eq!("SOL@nowhere".parse::<MarketQuery>().unwrap_err().code, ErrorCode::UnknownVenue);
    }

    #[test]
    fn routes_to_venues() {
        assert!(q("SOL").applies_to(VenueId::Jupiter));
        assert!(!q("SOL-PERP").applies_to(VenueId::Binance));
        assert!(q("SOL/USDC").applies_to(VenueId::Binance));
        assert!(!q("SOL/USDC").applies_to(VenueId::Jupiter));
    }
}
