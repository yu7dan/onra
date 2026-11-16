//! Venue adapters. Each adapter translates one venue's API into
//! [`onra_core`] types; nothing venue-specific leaks past this crate.

mod binance;
mod binance_clock;
mod binance_spot;
mod http;
mod hyperliquid;
mod hyperliquid_trade;
mod jupiter;
mod solana;

use onra_core::{
    Balance, Error, ErrorCode, FundingRate, Market, MarketQuery, Order, OrderBook, Position,
    Result, SearchHit, Ticker, VenueId,
};
use rust_decimal::Decimal;

pub use binance::{BinanceKey, Restrictions};
pub use binance_spot::{BinanceSpot, BookTop, SpotFill, SpotOrder, SymbolRules, TimeInForce};
pub use http::{DEFAULT_SOLANA_RPC, NetConfig};
pub use hyperliquid_trade::{
    HyperliquidTrade, PerpAccount, PerpFill, PerpRules, Top as PerpTop, parse_order_status,
};

pub struct Venues {
    http: reqwest::Client,
    binance: binance::Binance,
    hyperliquid: hyperliquid::Hyperliquid,
    jupiter: jupiter::Jupiter,
    solana: solana::Solana,
}

/// The accounts a person has linked. Addresses are public; only the
/// Binance key carries a secret.
#[derive(Default)]
pub struct Accounts {
    pub binance: Option<BinanceKey>,
    pub hyperliquid: Option<String>,
    pub solana: Option<String>,
}

impl Venues {
    pub fn new(net: &NetConfig) -> Result<Self> {
        let client = http::client(net)?;
        Ok(Self {
            http: client.clone(),
            binance: binance::Binance::new(client.clone()),
            hyperliquid: hyperliquid::Hyperliquid::new(client.clone()),
            jupiter: jupiter::Jupiter::new(client.clone()),
            solana: solana::Solana::new(client, net.solana_rpc.clone()),
        })
    }

    /// Spot trading on Binance, or its test network.
    pub fn binance_spot(&self, testnet: bool) -> BinanceSpot {
        BinanceSpot::new(self.http.clone(), testnet)
    }

    /// Perps trading on Hyperliquid, or its test network.
    pub fn hyperliquid_trade(&self, testnet: bool) -> HyperliquidTrade {
        HyperliquidTrade::new(self.http.clone(), testnet)
    }

    /// Balances on `venue`, or `None` if no account is linked there.
    pub async fn balances(
        &self,
        venue: VenueId,
        accounts: &Accounts,
    ) -> Option<Result<Vec<Balance>>> {
        Some(match venue {
            VenueId::Binance => self.binance.balances(accounts.binance.as_ref()?).await,
            VenueId::Hyperliquid => self.hyperliquid.balances(accounts.hyperliquid.as_ref()?).await,
            VenueId::Jupiter => self.solana_balances(accounts.solana.as_ref()?).await,
        })
    }

    /// Open perp positions; only Hyperliquid has them today.
    pub async fn positions(
        &self,
        venue: VenueId,
        accounts: &Accounts,
    ) -> Option<Result<Vec<Position>>> {
        match venue {
            VenueId::Hyperliquid => {
                Some(self.hyperliquid.positions(accounts.hyperliquid.as_ref()?).await)
            }
            VenueId::Binance | VenueId::Jupiter => None,
        }
    }

    /// Resting orders. Jupiter swaps execute at once, so it has none.
    pub async fn orders(&self, venue: VenueId, accounts: &Accounts) -> Option<Result<Vec<Order>>> {
        match venue {
            VenueId::Binance => Some(self.binance.orders(accounts.binance.as_ref()?).await),
            VenueId::Hyperliquid => {
                Some(self.hyperliquid.orders(accounts.hyperliquid.as_ref()?).await)
            }
            VenueId::Jupiter => None,
        }
    }

    /// What a Binance key may do. Used to refuse keys that can withdraw.
    pub async fn binance_restrictions(&self, key: &BinanceKey) -> Result<Restrictions> {
        self.binance.restrictions(key).await
    }

    async fn solana_balances(&self, owner: &str) -> Result<Vec<Balance>> {
        let holdings = self.solana.holdings(owner).await?;
        // SOL first, so it is resolved even if later batches are throttled.
        let mut mints: Vec<String> =
            holdings.keys().filter(|m| *m != solana::SOL_MINT).cloned().collect();
        mints.insert(0, solana::SOL_MINT.to_string());
        // Without metadata the balances are still true, just unnamed.
        let mut meta = self.jupiter.tokens(&mints).await.unwrap_or_default();
        // Native SOL never depends on a lookup succeeding.
        if !meta.contains_key(solana::SOL_MINT) {
            let price = self.jupiter.quote(solana::SOL_MINT).await.ok();
            meta.insert(
                solana::SOL_MINT.to_string(),
                jupiter::TokenMeta { symbol: "SOL".into(), verified: true, usd_price: price },
            );
        }
        Ok(holdings
            .into_iter()
            .map(|(mint, amount)| {
                let info = meta.get(&mint);
                let short = format!(
                    "{}…{}",
                    &mint[..4.min(mint.len())],
                    &mint[mint.len().saturating_sub(4)..]
                );
                Balance {
                    venue: VenueId::Jupiter,
                    asset: info.map_or(short, |m| m.symbol.clone()),
                    wallet: Some("solana".into()),
                    total: amount,
                    available: amount,
                    usd_value: info.and_then(|m| m.usd_price).map(|p| (amount * p).round_dp(2)),
                    mint: Some(mint),
                    verified: info.is_some_and(|m| m.verified),
                }
            })
            .collect())
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

    /// The order book for `query` on `venue`, `depth` levels a side.
    /// `Ok(None)` means the venue doesn't list the market.
    pub async fn book(
        &self,
        venue: VenueId,
        query: &MarketQuery,
        depth: usize,
    ) -> Result<Option<OrderBook>> {
        match venue {
            VenueId::Binance => self.binance.book(query, depth).await,
            VenueId::Hyperliquid => self.hyperliquid.book(query, depth).await,
            VenueId::Jupiter => Err(Error::new(
                ErrorCode::Venue,
                "jupiter has no order book: it routes swaps through AMM pools",
            )
            .with_hint(serde_json::json!({ "venue": "jupiter", "cause": "no_book" }))),
        }
    }

    /// Funding rates for every perp on `venue`; `None` where there are no perps.
    pub async fn funding(&self, venue: VenueId) -> Option<Result<Vec<FundingRate>>> {
        match venue {
            VenueId::Binance => Some(self.binance.funding().await),
            VenueId::Hyperliquid => Some(self.hyperliquid.funding().await),
            VenueId::Jupiter => None,
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
