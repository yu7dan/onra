//! What a trade request says, what the signer plans to do about it, and
//! what happened. Shared by the command line and the daemon.

use onra_core::{Error, ErrorCode, OrderSide, Result, VenueId};
use onra_policy::{Initiator, Verdict};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// How much to trade, as typed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Amount {
    /// `0.01 BTC`, `100 USDT`: a quantity of a named asset.
    Units { qty: Decimal, asset: String },
    /// `$100`: US dollars of notional.
    Usd { usd: Decimal },
    /// `50%`: a share of the available balance.
    Percent { pct: Decimal },
    /// `max`: all of the available balance.
    Max,
}

impl Amount {
    /// Parses the amount words of a trade command. A bare number is refused:
    /// `buy BTC 100` could mean 100 BTC or 100 dollars, and guessing wrong
    /// with money is not acceptable.
    pub fn parse(words: &[String]) -> Result<Self> {
        let joined = words.join(" ");
        let text = joined.trim();
        let bad = || {
            Error::new(ErrorCode::Usage, format!("cannot read the amount `{text}`")).with_hint(
                json!({ "cause": "amount", "examples": ["0.01 BTC", "100 USDT", "$100", "50%", "max"] }),
            )
        };
        let number = |s: &str| -> Result<Decimal> {
            let v: Decimal = s.replace(',', "").parse().map_err(|_| bad())?;
            if v <= Decimal::ZERO {
                return Err(Error::new(ErrorCode::Usage, "the amount must be positive"));
            }
            Ok(v)
        };
        if text.eq_ignore_ascii_case("max") || text.eq_ignore_ascii_case("all") {
            return Ok(Amount::Max);
        }
        if let Some(n) = text.strip_suffix('%') {
            let pct = number(n.trim())?;
            if pct > Decimal::ONE_HUNDRED {
                return Err(Error::new(ErrorCode::Usage, "a percentage can be at most 100%"));
            }
            return Ok(Amount::Percent { pct });
        }
        if let Some(n) = text.strip_prefix('$') {
            return Ok(Amount::Usd { usd: number(n.trim())? });
        }
        // `0.01 BTC` or `0.01BTC`.
        let split = text.find(|c: char| c.is_ascii_alphabetic()).ok_or_else(|| {
            Error::new(
                ErrorCode::Usage,
                format!("`{text}` needs a unit: say `{text} USDT`, `${text}`, or the asset"),
            )
            .with_hint(json!({ "cause": "amount_unit", "examples": [format!("${text}"), format!("{text} USDT")] }))
        })?;
        let (n, asset) = text.split_at(split);
        if asset.contains(char::is_whitespace) {
            return Err(bad());
        }
        Ok(Amount::Units { qty: number(n.trim())?, asset: asset.to_ascii_uppercase() })
    }
}

/// A trade as asked for, before anything is priced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRequest {
    pub market: String,
    pub side: OrderSide,
    pub amount: Amount,
    /// A limit price: the order rests on the book. Without it the order
    /// fills now or not at all, within the slippage bound.
    pub limit: Option<Decimal>,
    /// Percent the fill may be worse than the best price now.
    pub slippage_pct: Option<Decimal>,
    pub testnet: bool,
    pub client_id: Option<String>,
}

pub const DEFAULT_SLIPPAGE_PCT: Decimal = Decimal::from_parts(5, 0, 0, false, 1);

/// What the signer will do, priced and judged. Valid until `expires_ms`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub venue: VenueId,
    pub testnet: bool,
    pub symbol: String,
    pub base: String,
    pub quote: String,
    pub side: OrderSide,
    /// `ioc`: fill now within the bound, cancel the rest. `gtc`: rest.
    pub time_in_force: String,
    pub qty: Decimal,
    /// The limit sent to the venue: the worst price the order can fill at.
    pub price: Decimal,
    /// Best ask (buys) or bid (sells) when planned.
    pub reference_price: Decimal,
    /// Upper bound in USD: quantity times the limit price.
    pub notional_usd: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slippage_pct: Option<Decimal>,
    /// The venue accepted the order in a dry run.
    pub simulated: bool,
    pub verdict: Verdict,
    pub initiator: Initiator,
    pub created_ms: u64,
    pub expires_ms: u64,
    pub client_id: String,
}

impl Plan {
    /// One line for logs and lists: `BUY 0.00154 BTC @ ≤65,100 · BTCUSDT@bn`.
    pub fn summary(&self) -> String {
        let side = match self.side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        };
        let bound = if self.time_in_force == "gtc" {
            ""
        } else if self.side == OrderSide::Buy {
            "≤"
        } else {
            "≥"
        };
        let net = if self.testnet { " testnet" } else { "" };
        format!(
            "{side} {} {} @ {bound}{} {} · {}@{}{net}",
            self.qty.normalize(),
            self.base,
            self.price.normalize(),
            self.quote,
            self.symbol,
            self.venue.alias()
        )
    }
}

/// What the venue did with the order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Execution {
    pub plan_id: String,
    pub side: OrderSide,
    pub base: String,
    pub quote: String,
    pub order_id: String,
    pub status: String,
    pub executed_qty: Decimal,
    pub quote_qty: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_price: Option<Decimal>,
    pub fee: Vec<(Decimal, String)>,
    pub summary: String,
    pub testnet: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_amounts() {
        assert_eq!(
            Amount::parse(&words("0.01 BTC")).unwrap(),
            Amount::Units { qty: Decimal::new(1, 2), asset: "BTC".into() }
        );
        assert_eq!(
            Amount::parse(&words("100usdt")).unwrap(),
            Amount::Units { qty: Decimal::from(100), asset: "USDT".into() }
        );
        assert_eq!(
            Amount::parse(&words("$1,000")).unwrap(),
            Amount::Usd { usd: Decimal::from(1000) }
        );
        assert_eq!(
            Amount::parse(&words("50%")).unwrap(),
            Amount::Percent { pct: Decimal::from(50) }
        );
        assert_eq!(Amount::parse(&words("max")).unwrap(), Amount::Max);
    }

    #[test]
    fn refuses_ambiguous_amounts() {
        let bare = Amount::parse(&words("100")).unwrap_err();
        assert_eq!(bare.hint.unwrap()["cause"], "amount_unit");
        assert!(Amount::parse(&words("150%")).is_err());
        assert!(Amount::parse(&words("$-5")).is_err());
        assert!(Amount::parse(&words("0 BTC")).is_err());
        assert!(Amount::parse(&words("1 BTC ETH")).is_err());
    }

    #[test]
    fn default_slippage_is_half_a_percent() {
        assert_eq!(DEFAULT_SLIPPAGE_PCT.to_string(), "0.5");
    }
}
