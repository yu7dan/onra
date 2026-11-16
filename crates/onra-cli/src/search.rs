//! `onra search`: find an asset's markets on every venue, with the
//! verification status up front, because a ticker alone proves nothing on
//! Solana.

use futures::future::join_all;
use onra_core::{Error, ErrorCode, Result, SearchHit, VenueId};
use onra_proto::Warning;
use onra_ui::{Align, Cell, Table, Theme, format, palette};
use onra_venues::Venues;
use serde::Serialize;
use serde_json::json;

use crate::price::venue_color;

#[derive(Serialize)]
pub struct Results {
    pub query: String,
    pub results: Vec<SearchHit>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Warning>,
}

pub async fn search(venues: &Venues, query: &str) -> Result<Results> {
    let query = query.trim();
    if query.is_empty() {
        return Err(Error::new(ErrorCode::Usage, "search needs something to look for"));
    }
    let answers =
        join_all(VenueId::ALL.map(|v| async move { (v, venues.search(v, query).await) })).await;

    let mut results = Vec::new();
    let mut warnings = Vec::new();
    for (venue, answer) in answers {
        match answer {
            Ok(hits) => results.extend(hits),
            Err(error) => warnings.push(Warning { venue, error }),
        }
    }
    if results.is_empty() {
        if let Some(w) = warnings.into_iter().next() {
            return Err(w.error);
        }
        return Err(Error::new(ErrorCode::MarketNotFound, format!("nothing matches `{query}`"))
            .with_hint(json!({ "tried": VenueId::ALL.map(|v| v.alias()) })));
    }
    Ok(Results { query: query.to_string(), results, warnings })
}

pub fn render(r: &Results, theme: &Theme) -> Vec<String> {
    let mut table =
        Table::new(vec![Align::Left, Align::Left, Align::Right, Align::Left, Align::Left])
            .indent(2);
    let mut last_venue = None;
    for hit in &r.results {
        let venue = hit.market.venue;
        if last_venue.is_some() && last_venue != Some(venue) {
            table.gap();
        }
        let venue_cell = if last_venue == Some(venue) {
            Cell::empty()
        } else {
            Cell::new(
                format!("  ● {}", venue.name()),
                format!("  {} {}", theme.rgb("●", venue_color(venue)), theme.dim(venue.name())),
            )
        };
        last_venue = Some(venue);

        // Verified status leads the symbol for token venues.
        let symbol = &hit.market.symbol;
        let symbol_cell = match (venue, hit.market.verified) {
            (VenueId::Jupiter, true) => Cell::new(
                format!("✓ {symbol}"),
                format!("{} {}", theme.brand("✓"), theme.strong(symbol)),
            ),
            (VenueId::Jupiter, false) => Cell::new(
                format!("⚠ {symbol}"),
                format!("{} {}", theme.warn("⚠"), theme.warn(symbol)),
            ),
            _ => Cell::new(symbol, theme.strong(symbol)),
        };

        let price = hit.price.map(|p| format::price(p, format::price_dp(p))).unwrap_or_default();
        let price_cell = Cell::new(&price, theme.rgb(&price, palette::PAPER));

        let name = hit.name.clone().unwrap_or_default();
        let name_cell = Cell::new(&name, theme.dim(&name));

        let mut detail = Vec::new();
        if venue == VenueId::Jupiter {
            detail.push(short_mint(&hit.market.venue_symbol));
        }
        if let Some(liq) = hit.liquidity_usd {
            detail.push(format!("liq ${}", format::compact(liq)));
        }
        if let Some(h) = hit.holders {
            detail.push(format!("{} holders", format::compact(h.into())));
        }
        if !hit.market.verified {
            detail.push("unverified".into());
        }
        let detail = detail.join(" · ");
        let detail_cell = if hit.market.verified {
            Cell::new(&detail, theme.faint(&detail))
        } else {
            Cell::new(&detail, theme.warn(&detail))
        };

        table.row(vec![venue_cell, symbol_cell, price_cell, name_cell, detail_cell]);
    }

    let mut lines = vec![String::new()];
    lines.extend(table.render());
    lines.push(String::new());
    let hint = "use the ✓ symbol, or a mint address with @jup, to price a Solana token";
    lines.push(format!("  {}", theme.faint(hint)));
    for w in &r.warnings {
        lines.push(format!("  {} {}", theme.warn("!"), theme.warn(&w.error.message)));
    }
    lines.push(String::new());
    lines
}

fn short_mint(mint: &str) -> String {
    match (mint.get(..4), mint.get(mint.len().saturating_sub(4)..)) {
        (Some(head), Some(tail)) if mint.len() > 10 => format!("{head}…{tail}"),
        _ => mint.to_string(),
    }
}
