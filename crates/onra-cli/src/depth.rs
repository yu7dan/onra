//! `onra book` and `onra funding`: market depth and the cost of holding perps.

use std::cmp::Ordering;

use futures::future::join_all;
use onra_core::{
    Error, ErrorCode, FundingRate, MarketKind, MarketQuery, OrderBook, Result, VenueId,
};
use onra_proto::Warning;
use onra_ui::{Align, Cell, Table, Theme, format, palette};
use onra_venues::Venues;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::Serialize;
use serde_json::json;

use crate::commands::Outcome;
use crate::i18n::{self, fill};
use crate::price::venue_color;

// ---------------------------------------------------------------------------
// book

#[derive(Serialize)]
pub struct Books {
    pub books: Vec<OrderBook>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Warning>,
}

/// Which venues have a book for `query`: the one asked for, or every venue
/// with a book (Binance spot and Hyperliquid perps) for a bare symbol.
fn book_venues(query: &MarketQuery) -> Vec<VenueId> {
    match (query.venue, query.kind, &query.quote) {
        (Some(v), ..) => vec![v],
        (None, Some(MarketKind::Perp), _) => vec![VenueId::Hyperliquid],
        (None, _, Some(_)) => vec![VenueId::Binance],
        (None, ..) => vec![VenueId::Binance, VenueId::Hyperliquid],
    }
}

pub async fn book(venues: &Venues, market: &str, depth: usize) -> Result<Outcome> {
    let query: MarketQuery = market.parse()?;
    let depth = depth.clamp(1, 50);
    let answers = join_all(book_venues(&query).into_iter().map(|v| {
        let query = &query;
        async move { (v, venues.book(v, query, depth).await) }
    }))
    .await;

    let mut books = Vec::new();
    let mut warnings = Vec::new();
    for (venue, answer) in answers {
        match answer {
            Ok(Some(b)) => books.push(b),
            Ok(None) => {}
            Err(error) => warnings.push(Warning { venue, error }),
        }
    }
    if books.is_empty() {
        if let Some(w) = warnings.into_iter().next() {
            return Err(w.error);
        }
        return Err(Error::new(ErrorCode::MarketNotFound, format!("no venue lists `{market}`"))
            .with_hint(json!({ "query": market })));
    }
    let data = Books { books, warnings };
    let json = serde_json::to_value(&data).expect("books serialize");
    Ok(Outcome::new(json, move |theme| render_books(&data, theme)))
}

const BAR: usize = 24;
const EIGHTHS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// A bar `fraction` of `BAR` cells long, smooth to an eighth of a cell.
fn bar(fraction: f64) -> String {
    let eighths = (fraction.clamp(0.0, 1.0) * (BAR * 8) as f64).round() as usize;
    let mut s = "█".repeat(eighths / 8);
    if eighths % 8 > 0 {
        s.push(EIGHTHS[eighths % 8 - 1]);
    }
    s
}

fn render_books(data: &Books, theme: &Theme) -> Vec<String> {
    let s = i18n::s();
    let mut lines = Vec::new();
    for book in &data.books {
        let Some(mid) = book.mid() else { continue };
        let dp = format::price_dp(mid);
        let spread = book.spread().unwrap_or_default();
        let spread_pct =
            if mid.is_zero() { Decimal::ZERO } else { spread / mid * Decimal::ONE_HUNDRED };

        lines.push(String::new());
        let venue = book.market.venue;
        lines.push(format!(
            "  {}  {} {}      {} {}   {} {} · {}%",
            theme.strong(&book.market.symbol),
            theme.rgb("●", venue_color(venue)),
            theme.dim(venue.name()),
            theme.faint(s.book_mid),
            theme.strong(&format::price(mid, dp)),
            theme.faint(s.spread),
            theme.dim(&format::price(spread, dp)),
            theme.dim(&spread_pct.round_dp(4).normalize().to_string()),
        ));
        lines.push(String::new());

        // Depth bars show cumulative size from the touch outward, on one
        // scale for both sides so the book's balance is visible.
        let cumulative = |levels: &[onra_core::Level]| -> Vec<Decimal> {
            levels
                .iter()
                .scan(Decimal::ZERO, |acc, l| {
                    *acc += l.size;
                    Some(*acc)
                })
                .collect()
        };
        let ask_cum = cumulative(&book.asks);
        let bid_cum = cumulative(&book.bids);
        let max = ask_cum
            .last()
            .copied()
            .unwrap_or_default()
            .max(bid_cum.last().copied().unwrap_or_default());
        let scale =
            |c: Decimal| if max.is_zero() { 0.0 } else { (c / max).to_f64().unwrap_or(0.0) };

        let mut table = Table::new(vec![Align::Right, Align::Right, Align::Left]).indent(4);
        let (up, down) =
            (theme.movement_color(Ordering::Greater), theme.movement_color(Ordering::Less));
        // Asks print highest first so the best ask sits next to the mid line.
        for (i, level) in book.asks.iter().enumerate().rev() {
            let price = format::price(level.price, dp);
            let size = format::quantity_at(level.size, level.price);
            let fade = 0.35 + 0.65 * (1.0 - i as f32 / book.asks.len().max(1) as f32);
            table.row(vec![
                Cell::new(&price, theme.rgb(&price, down.rgb)),
                Cell::new(&size, theme.rgb(&size, palette::PAPER)),
                Cell::new(
                    bar(scale(ask_cum[i])),
                    theme.rgb(&bar(scale(ask_cum[i])), down.rgb.fade(fade * 0.7)),
                ),
            ]);
        }
        let mid_text = format::price(mid, dp);
        table.line(move |width| {
            let pad = width.saturating_sub(mid_text.chars().count() + 2) / 2;
            let rule = "─".repeat(pad);
            format!("{} {} {}", theme.faint(&rule), theme.brand(&mid_text), theme.faint(&rule))
        });
        for (i, level) in book.bids.iter().enumerate() {
            let price = format::price(level.price, dp);
            let size = format::quantity_at(level.size, level.price);
            let fade = 0.35 + 0.65 * (1.0 - i as f32 / book.bids.len().max(1) as f32);
            table.row(vec![
                Cell::new(&price, theme.rgb(&price, up.rgb)),
                Cell::new(&size, theme.rgb(&size, palette::PAPER)),
                Cell::new(
                    bar(scale(bid_cum[i])),
                    theme.rgb(&bar(scale(bid_cum[i])), up.rgb.fade(fade * 0.7)),
                ),
            ]);
        }
        lines.extend(table.render());
    }
    for w in &data.warnings {
        lines.push(String::new());
        lines.push(format!("  {} {}", theme.warn("!"), theme.warn(&i18n::error_text(&w.error))));
    }
    lines.push(String::new());
    lines
}

// ---------------------------------------------------------------------------
// funding

#[derive(Serialize)]
pub struct Funding {
    pub rates: Vec<FundingRate>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Warning>,
}

pub async fn funding(venues: &Venues, assets: &[String], top: usize) -> Result<Outcome> {
    let answers = join_all(VenueId::ALL.map(|v| async move { (v, venues.funding(v).await) })).await;
    let mut all = Vec::new();
    let mut warnings = Vec::new();
    for (venue, answer) in answers {
        match answer {
            Some(Ok(rates)) => all.extend(rates),
            Some(Err(error)) => warnings.push(Warning { venue, error }),
            None => {}
        }
    }
    if all.is_empty() {
        return Err(warnings
            .into_iter()
            .next()
            .map(|w| w.error)
            .unwrap_or_else(|| Error::new(ErrorCode::Internal, "no venue reported funding")));
    }

    // Which assets to show: the ones asked for, or the deepest Hyperliquid
    // markets by open interest.
    let wanted: Vec<String> = if assets.is_empty() {
        let mut hl: Vec<&FundingRate> =
            all.iter().filter(|r| r.venue == VenueId::Hyperliquid).collect();
        hl.sort_by(|a, b| b.open_interest_usd.cmp(&a.open_interest_usd));
        hl.into_iter().take(top).map(|r| r.asset.clone()).collect()
    } else {
        assets.iter().map(|a| a.to_ascii_uppercase()).collect()
    };
    let rates: Vec<FundingRate> = wanted
        .iter()
        .flat_map(|asset| {
            VenueId::ALL
                .into_iter()
                .filter_map(|v| all.iter().find(|r| r.venue == v && r.asset == *asset).cloned())
        })
        .collect();
    if rates.is_empty() {
        return Err(Error::new(
            ErrorCode::MarketNotFound,
            format!("no perp lists `{}`", wanted.join(" ")),
        )
        .with_hint(json!({ "query": wanted.join(" ") })));
    }

    let data = Funding { rates, warnings };
    let json = serde_json::to_value(&data).expect("funding serializes");
    Ok(Outcome::new(json, move |theme| render_funding(&data, &wanted, theme)))
}

fn apr_cell(rate: Option<&FundingRate>, theme: &Theme) -> Cell {
    let s = i18n::s();
    match rate {
        Some(r) => {
            let apr = format::percent(r.apr_pct);
            // The per-interval rate, signed like everything else (true minus).
            let pct = (r.rate * Decimal::ONE_HUNDRED).round_dp(4);
            let sign = if pct.is_sign_negative() && !pct.is_zero() { "−" } else { "" };
            let raw = format!("{sign}{:.4}%", pct.abs());
            let per = fill(s.funding_per, &[("h", &r.interval_hours.to_string())]);
            let plain = format!("{apr}  {raw} {per}");
            let styled = format!(
                "{}  {}",
                theme.movement(&apr, r.apr_pct.round_dp(2).cmp(&Decimal::ZERO)),
                theme.faint(&format!("{raw} {per}")),
            );
            Cell::new(plain, styled)
        }
        None => Cell::new("—", theme.faint("—")),
    }
}

fn render_funding(data: &Funding, wanted: &[String], theme: &Theme) -> Vec<String> {
    let s = i18n::s();
    let header = |t: &str| Cell::new(t, theme.faint(t));
    let mut table = Table::new(vec![Align::Left, Align::Left, Align::Left, Align::Right])
        .indent(2)
        .header(vec![
            header(""),
            header(VenueId::Hyperliquid.name()),
            header(VenueId::Binance.name()),
            header(s.funding_diff),
        ]);
    for asset in wanted {
        let find = |v: VenueId| data.rates.iter().find(|r| r.venue == v && r.asset == *asset);
        let (hl, bn) = (find(VenueId::Hyperliquid), find(VenueId::Binance));
        if hl.is_none() && bn.is_none() {
            continue;
        }
        let diff = match (hl, bn) {
            (Some(a), Some(b)) => {
                let d = a.apr_pct - b.apr_pct;
                let text = format::percent(d);
                Cell::new(&text, theme.dim(&text))
            }
            _ => Cell::empty(),
        };
        table.row(vec![
            Cell::new(format!("  {asset}"), format!("  {}", theme.strong(asset))),
            apr_cell(hl, theme),
            apr_cell(bn, theme),
            diff,
        ]);
    }
    let mut lines =
        vec![String::new(), format!("  {}", theme.strong(s.funding_title)), String::new()];
    lines.extend(table.render());
    lines.push(String::new());
    lines.push(format!("  {}", theme.faint(s.funding_note)));
    for w in &data.warnings {
        lines.push(format!("  {} {}", theme.warn("!"), theme.warn(&i18n::error_text(&w.error))));
    }
    lines.push(String::new());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_are_smooth() {
        assert_eq!(bar(0.0), "");
        assert_eq!(bar(1.0).chars().count(), BAR);
        assert_eq!(bar(1.0 / (BAR * 8) as f64), "▏");
    }

    #[test]
    fn picks_book_venues() {
        let q = |s: &str| s.parse::<MarketQuery>().unwrap();
        assert_eq!(book_venues(&q("ETH")), vec![VenueId::Binance, VenueId::Hyperliquid]);
        assert_eq!(book_venues(&q("ETH-PERP")), vec![VenueId::Hyperliquid]);
        assert_eq!(book_venues(&q("ETH/USDC")), vec![VenueId::Binance]);
    }
}
