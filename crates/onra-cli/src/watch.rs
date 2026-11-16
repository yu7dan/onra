//! `onra watch`: live prices. One full lookup establishes context (24h
//! change, which venues list the market); after that only light quotes are
//! polled. Price moves flash in the up/down color and fade; each quote keeps
//! a micro-trend of the session's ticks.

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use futures::future::join_all;
use onra_core::{Market, Result, VenueId};
use onra_ui::{Align, Cell, Rgb, Table, Theme, format, palette, sparkline, sparkline_styled};
use onra_venues::Venues;
use rust_decimal::Decimal;
use serde_json::{Value, json};

use crate::price::{self, Report};

const TICKS: usize = 32;
const FLASH: Duration = Duration::from_millis(700);
/// Failures in a row before a quote is shown as failing. A single miss
/// (a timeout, a throttled request) keeps the last price instead.
const TOLERATED_FAILURES: u32 = 3;

/// The fastest each venue may be polled. Jupiter's free API throttles
/// aggressive clients.
fn min_gap(venue: VenueId) -> Duration {
    match venue {
        VenueId::Jupiter => Duration::from_millis(2500),
        VenueId::Binance | VenueId::Hyperliquid => Duration::ZERO,
    }
}

pub struct Watch {
    assets: Vec<WatchAsset>,
    started: Instant,
    updated: Instant,
    pub polls: u64,
    last_polled: Vec<(VenueId, Instant)>,
}

struct WatchAsset {
    name: String,
    dp: u32,
    quotes: Vec<LiveQuote>,
}

struct LiveQuote {
    market: Market,
    price: Decimal,
    change_24h_pct: Option<Decimal>,
    /// When the price last moved, and which way.
    moved: Option<(Instant, Ordering)>,
    ticks: VecDeque<Decimal>,
    error: Option<String>,
    fails: u32,
}

/// (asset index, quote index, result) for one polled quote.
pub type Update = (usize, usize, Result<Decimal>);

impl Watch {
    pub fn from_report(report: &Report) -> Self {
        let assets = report
            .assets
            .iter()
            .map(|a| WatchAsset {
                name: a.quotes[0].ticker.market.base.clone(),
                dp: format::price_dp(a.reference_price),
                quotes: a
                    .quotes
                    .iter()
                    .map(|q| LiveQuote {
                        market: q.ticker.market.clone(),
                        price: q.ticker.price,
                        change_24h_pct: q.ticker.change_24h_pct,
                        moved: None,
                        ticks: VecDeque::from([q.ticker.price]),
                        error: None,
                        fails: 0,
                    })
                    .collect(),
            })
            .collect();
        let now = Instant::now();
        Self { assets, started: now, updated: now, polls: 0, last_polled: Vec::new() }
    }

    /// Markets due for a refresh at `interval`, honoring each venue's
    /// minimum gap. Marks the returned venues as polled.
    pub fn markets_due(&mut self, interval: Duration) -> Vec<(usize, usize, Market)> {
        let now = Instant::now();
        let due: Vec<VenueId> = VenueId::ALL
            .into_iter()
            .filter(|v| {
                let gap = interval.max(min_gap(*v));
                self.last_polled.iter().find(|(x, _)| x == v).is_none_or(|(_, at)| {
                    now.duration_since(*at) + Duration::from_millis(50) >= gap
                })
            })
            .collect();
        for v in &due {
            self.last_polled.retain(|(x, _)| x != v);
            self.last_polled.push((*v, now));
        }
        self.assets
            .iter()
            .enumerate()
            .flat_map(|(ai, a)| {
                a.quotes.iter().enumerate().map(move |(qi, q)| (ai, qi, q.market.clone()))
            })
            .filter(|(_, _, m)| due.contains(&m.venue))
            .collect()
    }

    pub fn apply(&mut self, updates: Vec<Update>) {
        let now = Instant::now();
        for (ai, qi, result) in updates {
            let Some(q) = self.assets.get_mut(ai).and_then(|a| a.quotes.get_mut(qi)) else {
                continue;
            };
            match result {
                Ok(price) => {
                    if price != q.price {
                        // Keep the 24h change honest as the price moves.
                        if let Some(pct) = q.change_24h_pct {
                            let open = q.price / (Decimal::ONE + pct / Decimal::ONE_HUNDRED);
                            if !open.is_zero() {
                                q.change_24h_pct = Some(
                                    ((price - open) / open * Decimal::ONE_HUNDRED).round_dp(4),
                                );
                            }
                        }
                        q.moved = Some((now, price.cmp(&q.price)));
                        q.price = price;
                    }
                    q.ticks.push_back(price);
                    if q.ticks.len() > TICKS {
                        q.ticks.pop_front();
                    }
                    q.error = None;
                    q.fails = 0;
                }
                Err(e) => {
                    q.fails += 1;
                    if q.fails >= TOLERATED_FAILURES {
                        q.error = Some(e.message);
                    }
                }
            }
        }
        self.updated = now;
        self.polls += 1;
    }

    /// One event per poll for machine mode (NDJSON).
    pub fn tick_json(&self) -> Value {
        let quotes: Vec<Value> = self
            .assets
            .iter()
            .flat_map(|a| a.quotes.iter())
            .map(|q| {
                let mut v = json!({
                    "venue": q.market.venue,
                    "symbol": q.market.symbol,
                    "price": q.price,
                });
                if let Some(e) = &q.error {
                    v["error"] = json!(e);
                }
                v
            })
            .collect();
        let ts = onra_proto::Meta::new(Duration::ZERO).ts;
        json!({ "event": "tick", "ts": ts, "seq": self.polls, "quotes": quotes })
    }

    pub fn render(&self, theme: &Theme, interval: Duration, live: bool) -> Vec<String> {
        let now = Instant::now();
        let mut lines = vec![self.header(theme, interval, live, now), String::new()];

        let mut table = Table::new(vec![
            Align::Left,
            Align::Left,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Left,
        ])
        .indent(2);
        for (i, asset) in self.assets.iter().enumerate() {
            if i > 0 {
                table.gap();
            }
            table.line(move |width| asset_line(asset, theme, width));
            for q in &asset.quotes {
                table.row(quote_row(q, asset.dp, theme, now));
            }
        }
        lines.extend(table.render());
        lines
    }

    fn header(&self, theme: &Theme, interval: Duration, live: bool, now: Instant) -> String {
        let markets: usize = self.assets.iter().map(|a| a.quotes.len()).sum();
        let ago = now.duration_since(self.updated).as_secs_f32();
        let running = now.duration_since(self.started).as_secs();
        let badge = if live {
            // The dot breathes once a second.
            let phase =
                (now.duration_since(self.started).as_secs_f32() * std::f32::consts::TAU).sin();
            let glow = 0.55 + 0.45 * phase.abs();
            format!("{} {}", theme.rgb("●", palette::AMBER.rgb.fade(glow)), theme.brand("LIVE"))
        } else {
            format!("{} {}", theme.faint("●"), theme.dim("STOPPED"))
        };
        format!(
            "  {badge}   {}",
            theme.faint(&format!(
                "{markets} markets · every {:.1}s · updated {ago:.1}s ago · {}m{:02}s",
                interval.as_secs_f32(),
                running / 60,
                running % 60
            ))
        )
    }
}

fn reference(asset: &WatchAsset) -> Decimal {
    let mut prices: Vec<Decimal> = asset.quotes.iter().map(|q| q.price).collect();
    prices.sort();
    let mid = prices.len() / 2;
    if prices.len().is_multiple_of(2) {
        (prices[mid - 1] + prices[mid]) / Decimal::TWO
    } else {
        prices[mid]
    }
}

fn asset_line(asset: &WatchAsset, theme: &Theme, width: usize) -> String {
    let price = format::price(reference(asset), asset.dp);
    let mut left = format!("{}   {}", theme.strong(&asset.name), theme.strong(&price));
    if let Some(pct) = asset.quotes.iter().find_map(|q| q.change_24h_pct) {
        let text = format::percent(pct);
        left.push_str(&format!("  {}", theme.movement(&text, pct.round_dp(2).cmp(&Decimal::ZERO))));
    }
    let lows = asset.quotes.iter().map(|q| q.price).min();
    let highs = asset.quotes.iter().map(|q| q.price).max();
    let right = match (lows, highs) {
        (Some(lo), Some(hi)) if asset.quotes.len() > 1 && !lo.is_zero() => {
            let spread = ((hi - lo) / lo * Decimal::ONE_HUNDRED).round_dp(2);
            format!("{} {}", theme.faint("spread"), theme.dim(&format!("{spread}%")))
        }
        _ => String::new(),
    };
    let pad =
        width.saturating_sub(onra_ui::text::width(&left) + onra_ui::text::width(&right)).max(3);
    format!("{left}{}{right}", " ".repeat(pad))
}

fn quote_row(q: &LiveQuote, dp: u32, theme: &Theme, now: Instant) -> Vec<Cell> {
    let venue = q.market.venue.name();
    let venue_cell = Cell::new(
        format!("  ● {venue}"),
        format!("  {} {}", theme.rgb("●", price::venue_color(q.market.venue)), theme.dim(venue)),
    );
    let market = Cell::new(&q.market.symbol, theme.rgb(&q.market.symbol, palette::MUTED));

    // The last move: an arrow that stays, and a flash that fades.
    let (arrow, flash) = match q.moved {
        Some((at, dir)) => {
            let glyph = if dir == Ordering::Greater { "▲" } else { "▼" };
            let fade = 1.0 - now.duration_since(at).as_secs_f32() / FLASH.as_secs_f32();
            (Cell::new(glyph, theme.movement(glyph, dir)), (fade > 0.0).then_some((dir, fade)))
        }
        None => (Cell::new("·", theme.faint("·")), None),
    };

    let price = format::price(q.price, dp);
    let (digits, zeros) = format::split_trailing_zeros(&price);
    let styled = match flash {
        Some((dir, fade)) => {
            let color: Rgb = theme.movement_color(dir).rgb;
            theme.on(&price, palette::PAPER, color.fade(0.15 + 0.45 * fade))
        }
        None => format!("{}{}", theme.strong(digits), theme.faint(zeros)),
    };
    let price_cell = Cell::new(&price, styled);

    let change = match q.change_24h_pct {
        Some(pct) => {
            let text = format::percent(pct);
            Cell::new(&text, theme.movement(&text, pct.round_dp(2).cmp(&Decimal::ZERO)))
        }
        None => Cell::new("—", theme.faint("—")),
    };

    let ticks: Vec<Decimal> = q.ticks.iter().copied().collect();
    let trend = match &q.error {
        Some(e) => Cell::new(e, theme.warn(e)),
        None if ticks.len() > 1 => {
            let dir = ticks[ticks.len() - 1].cmp(&ticks[0]);
            Cell::new(
                sparkline(&ticks),
                sparkline_styled(&ticks, theme.movement_color(dir).rgb, theme),
            )
        }
        None => Cell::new("", ""),
    };

    vec![venue_cell, market, arrow, price_cell, change, trend]
}

/// Poll every watched market once, concurrently.
pub async fn poll(venues: &Venues, markets: Vec<(usize, usize, Market)>) -> Vec<Update> {
    join_all(markets.into_iter().map(|(ai, qi, m)| async move { (ai, qi, venues.quote(&m).await) }))
        .await
}

type Polling<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Vec<Update>> + 'a>>;

/// Watch on a terminal until q, Esc or Ctrl+C (or `count` refreshes).
pub async fn run_terminal(
    venues: &Venues,
    markets: &[String],
    theme: Theme,
    interval: Duration,
    count: Option<u64>,
) -> Result<()> {
    use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
    use futures::StreamExt;

    struct RawMode;
    impl Drop for RawMode {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }

    let report = crate::commands::report_with_live_progress(venues, markets, Some(theme)).await?;
    let mut watch = Watch::from_report(&report);
    let raw = crossterm::terminal::enable_raw_mode().ok().map(|_| RawMode);

    let frame = |w: &Watch, live: bool| {
        let mut lines = vec![String::new()];
        lines.extend(w.render(&theme, interval, live));
        if live {
            lines.push(String::new());
            lines.push(format!("    {}", theme.faint("q / esc to stop")));
        }
        lines
    };
    let mut live = onra_ui::motion::Live::new();
    // Without a keyboard (recording, scripted runs with --count) there is
    // nothing to listen to.
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
    let mut events = EventStream::new();
    let mut redraw = tokio::time::interval(Duration::from_millis(80));
    let mut next_poll = Instant::now() + interval;
    let mut pending: Option<Polling<'_>> = None;

    loop {
        tokio::select! {
            event = events.next(), if interactive => match event {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                    let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c');
                    let stop = matches!(key.code, KeyCode::Esc | KeyCode::Char('q' | 'Q'));
                    if ctrl_c || stop {
                        break;
                    }
                }
                Some(Ok(_)) => {}
                _ => break,
            },
            updates = async { pending.as_mut().expect("guarded").await }, if pending.is_some() => {
                pending = None;
                watch.apply(updates);
                if count.is_some_and(|n| watch.polls >= n) {
                    break;
                }
            }
            _ = redraw.tick() => {
                if pending.is_none() && Instant::now() >= next_poll {
                    let due = watch.markets_due(interval);
                    if !due.is_empty() {
                        pending = Some(Box::pin(poll(venues, due)));
                    }
                    next_poll = Instant::now() + interval;
                }
                live.draw(&frame(&watch, true));
            }
        }
    }
    // Leave the last frame on screen, marked stopped.
    live.draw(&frame(&watch, false));
    live.finish();
    drop(raw);
    println!("\r");
    Ok(())
}

/// Watch in machine mode: NDJSON events on stdout.
pub async fn run_json(
    venues: &Venues,
    markets: &[String],
    interval: Duration,
    count: Option<u64>,
) -> Result<()> {
    let report = price::fetch(venues, markets, None).await?;
    let ts = onra_proto::Meta::new(Duration::ZERO).ts;
    println!("{}", json!({ "event": "snapshot", "ts": ts, "data": report }));
    let mut watch = Watch::from_report(&report);
    while count.is_none_or(|n| watch.polls < n) {
        tokio::time::sleep(interval).await;
        let due = watch.markets_due(interval);
        if !due.is_empty() {
            watch.apply(poll(venues, due).await);
            println!("{}", watch.tick_json());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::price::{Asset, Quote};
    use onra_core::{MarketKind, Ticker};
    use onra_ui::{ColorMode, Convention};
    use std::str::FromStr;

    fn quote(venue: VenueId, symbol: &str, price: &str, change: &str) -> Quote {
        Quote {
            ticker: Ticker {
                market: Market {
                    venue,
                    symbol: symbol.into(),
                    base: "SOL".into(),
                    kind: MarketKind::Spot,
                    venue_symbol: symbol.into(),
                    verified: true,
                },
                price: Decimal::from_str(price).unwrap(),
                change_24h_pct: Some(Decimal::from_str(change).unwrap()),
                low_24h: None,
                high_24h: None,
                trend_24h: Vec::new(),
            },
            latency_ms: 100,
        }
    }

    #[test]
    fn renders_live_frame() {
        let report = Report {
            assets: vec![Asset {
                query: "SOL".into(),
                reference_price: Decimal::from_str("72.04").unwrap(),
                spread_pct: None,
                lowest: None,
                highest: None,
                quotes: vec![
                    quote(VenueId::Binance, "SOL/USDT", "72.04", "2.1"),
                    quote(VenueId::Hyperliquid, "SOL-PERP", "71.99", "2.0"),
                    quote(VenueId::Jupiter, "SOL", "72.01", "2.2"),
                ],
            }],
            warnings: Vec::new(),
        };
        let mut watch = Watch::from_report(&report);
        let prices = ["72.05", "72.03", "72.06", "72.08", "72.07", "72.10", "72.12"];
        for p in prices {
            let updates = vec![
                (0, 0, Ok(Decimal::from_str(p).unwrap())),
                (0, 1, Ok(Decimal::from_str(p).unwrap() - Decimal::from_str("0.05").unwrap())),
            ];
            watch.apply(updates);
        }
        let theme = Theme { mode: ColorMode::TrueColor, convention: Convention::East };
        let lines = watch.render(&theme, Duration::from_millis(1500), true);
        assert!(lines.len() >= 5);
        if let Ok(dir) = std::env::var("ONRA_SNAPSHOT") {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(format!("{dir}/watch.ansi"), lines.join("\n")).unwrap();
        }
    }
}
