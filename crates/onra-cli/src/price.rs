//! `onra price`: every venue, one view. The cross-venue comparison (reference
//! price, spread, cheapest and dearest venue) is the point, so it leads both
//! the rendered view and the JSON.

use std::cmp::Ordering;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use onra_core::{Error, ErrorCode, MarketQuery, Result, Ticker, VenueId};
use onra_proto::Warning;
use onra_ui::{Align, Cell, Rgb, Table, Theme, format, palette, sparkline_styled};
use onra_venues::Venues;
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::json;
use tokio::sync::mpsc::UnboundedSender;

use crate::i18n::{self, fill};

#[derive(Serialize)]
pub struct Report {
    pub assets: Vec<Asset>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Warning>,
}

/// One requested market across every venue that answered for it.
#[derive(Serialize)]
pub struct Asset {
    pub query: String,
    /// Median of the venue prices.
    pub reference_price: Decimal,
    /// (highest − lowest) / lowest, in percent. Needs two or more venues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spread_pct: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lowest: Option<VenueId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highest: Option<VenueId>,
    pub quotes: Vec<Quote>,
}

#[derive(Serialize)]
pub struct Quote {
    #[serde(flatten)]
    pub ticker: Ticker,
    pub latency_ms: u64,
}

/// Progress events from [`fetch`]. The caller decides how to show them.
#[derive(Debug)]
pub enum Progress {
    /// A lookup on this venue has started.
    Expect(VenueId),
    /// A lookup on this venue finished.
    Done { venue: VenueId, error: Option<String>, elapsed: Duration },
}

pub async fn fetch(
    venues: &Venues,
    inputs: &[String],
    progress: Option<&UnboundedSender<Progress>>,
) -> Result<Report> {
    let send = |event: Progress| {
        if let Some(tx) = progress {
            let _ = tx.send(event);
        }
    };
    let queries = inputs.iter().map(|s| s.parse()).collect::<Result<Vec<MarketQuery>>>()?;
    let queries = &queries;

    let mut pending: FuturesUnordered<_> = queries
        .iter()
        .enumerate()
        .flat_map(|(qi, q)| {
            VenueId::ALL.into_iter().filter(|&v| q.applies_to(v)).map(move |v| (qi, v))
        })
        .map(|(qi, venue)| async move {
            let started = Instant::now();
            let result = venues.ticker(venue, &queries[qi]).await;
            (qi, venue, result, started.elapsed())
        })
        .collect();
    for q in queries {
        for venue in VenueId::ALL.into_iter().filter(|&v| q.applies_to(v)) {
            send(Progress::Expect(venue));
        }
    }

    let mut results = Vec::new();
    while let Some(result) = pending.next().await {
        let (_, venue, outcome, elapsed) = &result;
        let error = outcome.as_ref().err().map(|e| e.message.clone());
        send(Progress::Done { venue: *venue, error, elapsed: *elapsed });
        results.push(result);
    }
    // Results arrive in completion order; present them in request order,
    // then in the canonical venue order.
    results.sort_by_key(|(qi, venue, ..)| (*qi, VenueId::ALL.iter().position(|v| v == venue)));

    let mut quotes: Vec<Vec<Quote>> = (0..queries.len()).map(|_| Vec::new()).collect();
    let mut errored = vec![false; queries.len()];
    let mut warnings: Vec<Warning> = Vec::new();
    for (qi, venue, result, elapsed) in results {
        match result {
            Ok(Some(ticker)) => quotes[qi].push(Quote {
                ticker,
                latency_ms: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
            }),
            Ok(None) => {}
            Err(error) => {
                errored[qi] = true;
                // One warning per venue: if it's down, it's down for every input.
                if !warnings.iter().any(|w| w.venue == venue) {
                    warnings.push(Warning { venue, error });
                }
            }
        }
    }

    // A market no venue lists is almost always a typo; say so loudly rather
    // than returning a view with a silent hole in it.
    if let Some(qi) = (0..queries.len()).find(|&i| quotes[i].is_empty() && !errored[i]) {
        let tried: Vec<_> = VenueId::ALL
            .into_iter()
            .filter(|&v| queries[qi].applies_to(v))
            .map(|v| v.alias())
            .collect();
        return Err(Error::new(
            ErrorCode::MarketNotFound,
            format!("no venue lists `{}`", inputs[qi]),
        )
        .with_hint(json!({ "query": inputs[qi], "tried": tried })));
    }
    if quotes.iter().all(Vec::is_empty) {
        // Nothing succeeded; surface the first failure as the error.
        return Err(warnings.swap_remove(0).error);
    }

    let assets = inputs
        .iter()
        .zip(quotes)
        .filter(|(_, q)| !q.is_empty())
        .map(|(input, quotes)| summarize(input, quotes))
        .collect();
    Ok(Report { assets, warnings })
}

fn summarize(query: &str, quotes: Vec<Quote>) -> Asset {
    let mut prices: Vec<Decimal> = quotes.iter().map(|q| q.ticker.price).collect();
    prices.sort();
    let mid = prices.len() / 2;
    let reference_price = if prices.len().is_multiple_of(2) {
        (prices[mid - 1] + prices[mid]) / Decimal::TWO
    } else {
        prices[mid]
    };

    let (mut spread_pct, mut lowest, mut highest) = (None, None, None);
    if quotes.len() > 1 {
        let low = quotes.iter().min_by_key(|q| q.ticker.price).expect("non-empty");
        let high = quotes.iter().max_by_key(|q| q.ticker.price).expect("non-empty");
        if !low.ticker.price.is_zero() {
            spread_pct = Some(
                ((high.ticker.price - low.ticker.price) / low.ticker.price * Decimal::ONE_HUNDRED)
                    .round_dp(4),
            );
        }
        lowest = Some(low.ticker.market.venue);
        highest = Some(high.ticker.market.venue);
    }
    Asset { query: query.to_string(), reference_price, spread_pct, lowest, highest, quotes }
}

// ---------------------------------------------------------------------------
// Rendering

/// Spreads at or above this are worth a second look, so they're highlighted.
const NOTABLE_SPREAD_PCT: Decimal = Decimal::from_parts(5, 0, 0, false, 1);

pub fn venue_color(venue: VenueId) -> Rgb {
    match venue {
        VenueId::Binance => Rgb(240, 185, 11),
        VenueId::Hyperliquid => Rgb(80, 227, 194),
        VenueId::Jupiter => Rgb(199, 242, 132),
    }
}

fn direction(value: Decimal) -> Ordering {
    value.round_dp(2).cmp(&Decimal::ZERO)
}

pub fn render(report: &Report, theme: &Theme, elapsed: Duration) -> Vec<String> {
    let mut table = Table::new(vec![
        Align::Left,
        Align::Left,
        Align::Right,
        Align::Right,
        Align::Left,
        Align::Right,
    ])
    .indent(2);

    for (i, asset) in report.assets.iter().enumerate() {
        if i > 0 {
            table.gap();
        }
        // One precision per asset so venues line up digit for digit.
        let dp = format::price_dp(asset.reference_price);
        table.line(move |width| asset_header(asset, dp, theme, width));
        for quote in &asset.quotes {
            table.row(quote_row(quote, dp, theme));
        }
        if let Some(line) = spread_line(asset, theme) {
            table.line(move |_| line.clone());
        }
    }

    let mut lines = vec![String::new()];
    lines.extend(table.render());
    lines.push(String::new());

    let mut venues: Vec<VenueId> =
        report.assets.iter().flat_map(|a| a.quotes.iter().map(|q| q.ticker.market.venue)).collect();
    venues.sort_by_key(|v| VenueId::ALL.iter().position(|x| x == v));
    venues.dedup();
    let now = onra_proto::Meta::new(elapsed).ts;
    let clock = now.get(11..19).unwrap_or_default();
    let s = i18n::s();
    let count = venues.len().to_string();
    let venue_count = if venues.len() == 1 {
        s.venue_one.to_string()
    } else {
        fill(s.venue_many, &[("n", &count)])
    };
    let ms = elapsed.as_millis().to_string();
    let footer = fill(s.footer, &[("venues", &venue_count), ("ms", &ms), ("time", clock)]);
    lines.push(format!("  {}", theme.faint(&footer)));

    if !report.warnings.is_empty() {
        lines.push(String::new());
        for w in &report.warnings {
            lines.push(format!(
                "  {} {}",
                theme.warn("!"),
                theme.warn(&i18n::error_text(&w.error))
            ));
        }
        // Venues usually fail for the same reason; give each fix once.
        let mut suggestions: Vec<&str> = Vec::new();
        for s in report.warnings.iter().filter_map(|w| w.error.suggestion()) {
            if !suggestions.contains(&s) {
                suggestions.push(s);
            }
        }
        for s in suggestions {
            lines.push(format!("    {}", theme.dim(&format!("→ {s}"))));
        }
    }
    lines.push(String::new());
    lines
}

/// `SOL   71.2484  −2.35%                 24h  69.09 ──────●───── 72.83`
fn asset_header(asset: &Asset, dp: u32, theme: &Theme, width: usize) -> String {
    let name = asset.quotes[0].ticker.market.base.clone();
    let price = format::price(asset.reference_price, dp);
    // The change of the first venue that reports one stands for the asset.
    let change = asset.quotes.iter().find_map(|q| q.ticker.change_24h_pct);

    let mut left_plain = format!("{name}   {price}");
    let mut left = format!("{}   {}", theme.strong(&name), theme.strong(&price));
    if let Some(pct) = change {
        let text = format::percent(pct);
        left_plain.push_str(&format!("  {text}"));
        left.push_str(&format!("  {}", theme.movement(&text, direction(pct))));
    }

    let Some((range_plain, range)) = range_bar(asset, dp, theme) else {
        return left;
    };
    let used = display_width(&left_plain) + display_width(&range_plain);
    let pad = width.saturating_sub(used).max(3);
    format!("{left}{}{range}", " ".repeat(pad))
}

/// The 24h range with the reference price placed on it.
fn range_bar(asset: &Asset, dp: u32, theme: &Theme) -> Option<(String, String)> {
    const TRACK: usize = 14;
    let (low, high) =
        asset.quotes.iter().find_map(|q| Some((q.ticker.low_24h?, q.ticker.high_24h?)))?;
    if high <= low {
        return None;
    }
    let t = ((asset.reference_price - low) / (high - low)).clamp(Decimal::ZERO, Decimal::ONE);
    let pos = (t * Decimal::from(TRACK - 1)).round().try_into().unwrap_or(0usize);

    // Range labels are context, not quotes: two decimals are plenty above 1.
    let dp = if asset.reference_price >= Decimal::ONE { dp.min(2) } else { dp };
    let (low_s, high_s) = (format::price(low, dp), format::price(high, dp));
    let label = i18n::s().range_24h;
    let plain = format!("{label}  {low_s} {} {high_s}", "─".repeat(TRACK));
    let styled = format!(
        "{}  {} {}{}{} {}",
        theme.faint(label),
        theme.dim(&low_s),
        theme.rgb(&"─".repeat(pos), palette::MUTED),
        theme.brand("●"),
        theme.faint(&"─".repeat(TRACK - 1 - pos)),
        theme.dim(&high_s),
    );
    Some((plain, styled))
}

fn quote_row(quote: &Quote, dp: u32, theme: &Theme) -> Vec<Cell> {
    let t = &quote.ticker;
    let venue = t.market.venue.name();
    let venue_cell = Cell::new(
        format!("  ● {venue}"),
        format!("  {} {}", theme.rgb("●", venue_color(t.market.venue)), theme.dim(venue)),
    );

    let market = if t.market.verified {
        Cell::new(&t.market.symbol, theme.rgb(&t.market.symbol, palette::MUTED))
    } else {
        let unverified = i18n::s().unverified;
        Cell::new(
            format!("{} {unverified}", t.market.symbol),
            format!("{} {}", theme.rgb(&t.market.symbol, palette::MUTED), theme.warn(unverified)),
        )
    };

    let price = format::price(t.price, dp);
    let (digits, zeros) = format::split_trailing_zeros(&price);
    let price_cell = Cell::new(&price, format!("{}{}", theme.strong(digits), theme.faint(zeros)));

    let change = match t.change_24h_pct {
        Some(pct) => {
            let text = format::percent(pct);
            Cell::new(&text, theme.movement(&text, direction(pct)))
        }
        None => Cell::new("—", theme.faint("—")),
    };

    // The trend takes the color of the 24h change, so every column of a row
    // tells the same story.
    let trend = match (t.trend_24h.first(), t.trend_24h.last()) {
        (Some(first), Some(last)) => {
            let dir = t.change_24h_pct.map_or_else(|| last.cmp(first), direction);
            let color = theme.movement_color(dir).rgb;
            let plain = onra_ui::sparkline(&t.trend_24h);
            Cell::new(plain, sparkline_styled(&t.trend_24h, color, theme))
        }
        _ => Cell::empty(),
    };

    let latency = format!("{}ms", quote.latency_ms);
    vec![venue_cell, market, price_cell, change, trend, Cell::new(&latency, theme.faint(&latency))]
}

/// `spread 0.05%  ·  low hyperliquid  ·  high binance`
fn spread_line(asset: &Asset, theme: &Theme) -> Option<String> {
    let spread = asset.spread_pct?;
    let value = format!("{}%", spread.round_dp(2));
    let value = if spread >= NOTABLE_SPREAD_PCT { theme.warn(&value) } else { theme.dim(&value) };
    let sep = theme.faint("  ·  ");
    Some(format!(
        "  {} {value}{sep}{} {}{sep}{} {}",
        theme.faint(i18n::s().spread),
        theme.faint(i18n::s().low),
        theme.dim(asset.lowest?.name()),
        theme.faint(i18n::s().high),
        theme.dim(asset.highest?.name()),
    ))
}

/// Everything in a header is single-width (ASCII, box drawing, `−`, `●`).
fn display_width(s: &str) -> usize {
    s.chars().count()
}

// ---------------------------------------------------------------------------
// Live progress while venues answer

/// Per-venue progress while a fetch is in flight, as renderable lines.
/// Owns no terminal: callers draw `lines` wherever they display progress.
#[derive(Default)]
pub struct Loader {
    venues: Vec<(VenueId, VenueState)>,
}

enum VenueState {
    Waiting { remaining: usize, slowest: Duration, failed: Option<String> },
    Done { slowest: Duration, failed: Option<String> },
}

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// Progress stays hidden this long, so fast responses never flash.
pub const LOADER_DELAY: Duration = Duration::from_millis(90);

impl Loader {
    pub fn apply(&mut self, event: Progress) {
        match event {
            Progress::Expect(venue) => {
                match self.venues.iter_mut().find(|(v, _)| *v == venue) {
                    Some((_, VenueState::Waiting { remaining, .. })) => *remaining += 1,
                    Some(_) => {}
                    None => self.venues.push((
                        venue,
                        VenueState::Waiting { remaining: 1, slowest: Duration::ZERO, failed: None },
                    )),
                }
                self.venues.sort_by_key(|(v, _)| VenueId::ALL.iter().position(|x| x == v));
            }
            Progress::Done { venue, error, elapsed } => {
                let Some((_, state)) = self.venues.iter_mut().find(|(v, _)| *v == venue) else {
                    return;
                };
                if let VenueState::Waiting { remaining, slowest, failed } = state {
                    *remaining = remaining.saturating_sub(1);
                    *slowest = (*slowest).max(elapsed);
                    if let Some(e) = error {
                        failed.get_or_insert(e);
                    }
                    if *remaining == 0 {
                        *state = VenueState::Done { slowest: *slowest, failed: failed.take() };
                    }
                }
            }
        }
    }

    /// The progress block at `elapsed` since the fetch began.
    pub fn lines(&self, theme: &Theme, elapsed: Duration) -> Vec<String> {
        let frame = (elapsed.as_millis() / 80) as usize;
        let spin = SPINNER[frame % SPINNER.len()].to_string();
        let done = self.venues.iter().filter(|(_, s)| matches!(s, VenueState::Done { .. })).count();

        let mut lines = vec![format!(
            "  {} {}  {}",
            theme.brand(&spin),
            theme.shimmer(i18n::s().reaching_markets, elapsed.as_secs_f32() / 1.4),
            theme.faint(&format!("{done}/{}", self.venues.len())),
        )];
        let name_width = self.venues.iter().map(|(v, _)| v.name().len()).max().unwrap_or(0);
        for (venue, state) in &self.venues {
            let name = format!("{:<name_width$}", venue.name());
            let status = match state {
                VenueState::Waiting { .. } => theme.faint(&spin),
                VenueState::Done { slowest, failed: None } => format!(
                    "{} {}",
                    theme.brand("✓"),
                    theme.faint(&format!("{}ms", slowest.as_millis()))
                ),
                VenueState::Done { failed: Some(msg), .. } => {
                    format!("{} {}", theme.warn("✗"), theme.faint(msg))
                }
            };
            lines.push(format!(
                "    {} {}   {status}",
                theme.rgb("●", venue_color(*venue)),
                theme.dim(&name)
            ));
        }
        lines
    }
}
