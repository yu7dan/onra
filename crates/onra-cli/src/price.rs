//! `onra price`: one table, every venue. This is the "single entry point"
//! idea in its smallest form.

use futures::future::join_all;
use onra_core::{Error, ErrorCode, MarketQuery, Result, Ticker, VenueId};
use onra_proto::Warning;
use onra_ui::{Align, Cell, Table, Theme, format, sparkline};
use onra_venues::Venues;
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::json;

#[derive(Serialize)]
pub struct Report {
    pub tickers: Vec<Ticker>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Warning>,
    /// Which input each ticker answers, for grouping rows on screen.
    #[serde(skip)]
    groups: Vec<usize>,
}

pub async fn fetch(venues: &Venues, inputs: &[String]) -> Result<Report> {
    let queries = inputs.iter().map(|s| s.parse()).collect::<Result<Vec<MarketQuery>>>()?;

    let queries = &queries;
    let lookups = queries.iter().enumerate().flat_map(|(qi, q)| {
        VenueId::ALL.into_iter().filter(|&v| q.applies_to(v)).map(move |v| (qi, v))
    });
    // join_all preserves order: rows come out grouped by input, then by venue.
    let results =
        join_all(lookups.map(|(qi, venue)| async move {
            (qi, venue, venues.ticker(venue, &queries[qi]).await)
        }))
        .await;

    let mut report = Report { tickers: Vec::new(), warnings: Vec::new(), groups: Vec::new() };
    let mut answered = vec![false; queries.len()];
    let mut errored = vec![false; queries.len()];
    for (qi, venue, result) in results {
        match result {
            Ok(Some(ticker)) => {
                answered[qi] = true;
                report.tickers.push(ticker);
                report.groups.push(qi);
            }
            Ok(None) => {}
            Err(error) => {
                errored[qi] = true;
                // One warning per venue: if it's down, it's down for every input.
                if !report.warnings.iter().any(|w| w.venue == venue) {
                    report.warnings.push(Warning { venue, error });
                }
            }
        }
    }

    // A market no venue lists is almost always a typo; say so loudly rather
    // than returning a table with a silent hole in it.
    if let Some(qi) = (0..queries.len()).find(|&i| !answered[i] && !errored[i]) {
        let tried: Vec<_> = VenueId::ALL
            .into_iter()
            .filter(|&v| queries[qi].applies_to(v))
            .map(|v| v.alias())
            .collect();
        return Err(Error::new(
            ErrorCode::MarketNotFound,
            format!("no venue lists `{}`", inputs[qi]),
        )
        .with_hint(json!({ "tried": tried })));
    }
    if report.tickers.is_empty() {
        // Nothing succeeded; surface the first failure as the error.
        return Err(report.warnings.swap_remove(0).error);
    }
    Ok(report)
}

pub fn render(report: &Report, theme: &Theme) -> String {
    let header = |s: &str| Cell::new(s, theme.dim(s));
    let mut table = Table::new(vec![
        (header("MARKET"), Align::Left),
        (header("VENUE"), Align::Left),
        (header("PRICE"), Align::Right),
        (header("24H"), Align::Right),
        (header("24H TREND"), Align::Left),
    ]);

    let mut i = 0;
    while i < report.tickers.len() {
        let group = report.groups[i];
        let end = (i..report.tickers.len())
            .find(|&j| report.groups[j] != group)
            .unwrap_or(report.tickers.len());
        if i > 0 {
            table.gap();
        }
        // One precision per asset so venues line up digit for digit.
        let dp = format::price_dp(report.tickers[i].price);
        for t in &report.tickers[i..end] {
            table.row(row(t, dp, theme));
        }
        i = end;
    }

    let mut out = format!("\n{}", table.render());
    if !report.warnings.is_empty() {
        out.push('\n');
        for w in &report.warnings {
            out.push_str(&format!("  {} {}\n", theme.warn("!"), theme.warn(&w.error.message)));
        }
        // Venues usually fail for the same reason; give each fix once.
        let mut suggestions: Vec<&str> = Vec::new();
        for s in report.warnings.iter().filter_map(|w| w.error.suggestion()) {
            if !suggestions.contains(&s) {
                suggestions.push(s);
            }
        }
        for s in suggestions {
            out.push_str(&format!("    {}\n", theme.dim(&format!("→ {s}"))));
        }
    }
    out.push('\n');
    out
}

fn row(t: &Ticker, dp: u32, theme: &Theme) -> Vec<Cell> {
    let market = if t.market.verified {
        Cell::plain(&t.market.symbol)
    } else {
        let plain = format!("{} unverified", t.market.symbol);
        Cell::new(plain, format!("{} {}", t.market.symbol, theme.warn("unverified")))
    };

    let price = format::price(t.price, dp);
    let change = match t.change_24h_pct {
        Some(pct) => {
            let text = format::percent(pct);
            Cell::new(&text, theme.movement(&text, pct.round_dp(2).cmp(&Decimal::ZERO)))
        }
        None => Cell::new("—", theme.dim("—")),
    };

    let spark = sparkline(&t.trend_24h);
    let trend = match (t.trend_24h.first(), t.trend_24h.last()) {
        (Some(first), Some(last)) => Cell::new(&spark, theme.movement(&spark, last.cmp(first))),
        _ => Cell::new("", ""),
    };

    vec![
        market,
        Cell::new(t.market.venue.name(), theme.dim(t.market.venue.name())),
        Cell::new(&price, theme.bold(&price)),
        change,
        trend,
    ]
}
