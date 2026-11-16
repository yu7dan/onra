//! Linked accounts and what they hold.
//!
//! Addresses are public and live in the config file. The one secret, a
//! Binance API key, lives in the keystore and is only pulled out for the
//! request that needs it. Keys that can withdraw are refused outright.

use std::io::{BufRead, IsTerminal, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use futures::future::join_all;
use onra_core::{
    Balance, Error, ErrorCode, Order, OrderSide, Position, PositionSide, Result, VenueId,
};
use onra_keystore::{Backend, Keystore, Secret};
use onra_proto::Warning;
use onra_ui::{Align, Cell, Table, Theme, format};
use onra_venues::{Accounts, BinanceKey, Venues};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use zeroize::Zeroizing;

use crate::cli::AccountAction;
use crate::commands::Outcome;
use crate::config::Config;
use crate::i18n::{self, fill};
use crate::price::venue_color;

const BINANCE_ENTRY: &str = "binance";
const HIDDEN: &str = "••••";
const MAX_ORDERS_SHOWN: usize = 15;

pub fn keystore() -> Result<Keystore> {
    let dir = dirs::data_dir()
        .ok_or_else(|| Error::new(ErrorCode::Credentials, "cannot locate the data directory"))?;
    Ok(Keystore::new(dir.join("onra").join("keystore.json")))
}

/// Only asked for when the system keychain is unavailable.
fn passphrase() -> Result<Secret> {
    if let Ok(p) = std::env::var("ONRA_KEYSTORE_PASSPHRASE") {
        return Ok(Zeroizing::new(p));
    }
    if !std::io::stdin().is_terminal() {
        return Err(Error::new(
            ErrorCode::Credentials,
            "the keystore needs a passphrase; set ONRA_KEYSTORE_PASSPHRASE",
        ));
    }
    rpassword::prompt_password(i18n::s().prompt_passphrase)
        .map(Zeroizing::new)
        .map_err(|e| Error::new(ErrorCode::Credentials, format!("cannot read passphrase: {e}")))
}

#[derive(Serialize, Deserialize)]
struct StoredKey {
    key: String,
    secret: String,
}

/// Which venue an account argument means. Solana wallets trade on Jupiter,
/// so `sol` and `solana` are accepted as well.
fn account_venue(name: &str) -> Result<VenueId> {
    match name.to_ascii_lowercase().as_str() {
        "sol" | "solana" => Ok(VenueId::Jupiter),
        other => other.parse(),
    }
}

/// The linked accounts, with the Binance secret pulled from the keystore.
pub fn load(config: &Config) -> Result<Accounts> {
    let linked = config.accounts();
    let binance = match &linked.binance {
        None => None,
        Some(_) => {
            let missing = || {
                Error::new(ErrorCode::Credentials, "the binance key is missing from the keystore")
                    .with_hint(json!({ "venue": "binance", "cause": "missing_key", "suggest": "onra account login bn" }))
            };
            let stored = keystore()?.get(BINANCE_ENTRY, &passphrase)?.ok_or_else(missing)?;
            let parsed: StoredKey = serde_json::from_str(&stored).map_err(|_| missing())?;
            Some(BinanceKey { key: parsed.key, secret: Zeroizing::new(parsed.secret) })
        }
    };
    Ok(Accounts { binance, hyperliquid: linked.hyperliquid.clone(), solana: linked.solana.clone() })
}

fn short(s: &str) -> String {
    if s.chars().count() <= 12 {
        return s.to_string();
    }
    let head: String = s.chars().take(6).collect();
    let tail: String = s.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}…{tail}")
}

fn valid_evm(address: &str) -> bool {
    address.len() == 42
        && address.starts_with("0x")
        && address[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn valid_solana(address: &str) -> bool {
    const BASE58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    (32..=44).contains(&address.len()) && address.chars().all(|c| BASE58.contains(c))
}

// ---------------------------------------------------------------------------
// account list | add | login | remove

pub async fn account(
    action: AccountAction,
    config: &mut Config,
    venues: &Venues,
) -> Result<Outcome> {
    match action {
        AccountAction::List => Ok(list(config)),
        AccountAction::Add { venue, address } => add(&venue, &address, config),
        AccountAction::Login { venue } => login(&venue, config, venues).await,
        AccountAction::Remove { venue } => remove(&venue, config),
    }
}

fn list(config: &Config) -> Outcome {
    let linked = config.accounts().clone();
    let rows: Vec<(VenueId, Option<String>, &str)> = vec![
        (VenueId::Binance, linked.binance.clone(), "onra account login bn"),
        (VenueId::Hyperliquid, linked.hyperliquid.clone(), "onra account add hl <address>"),
        (VenueId::Jupiter, linked.solana.clone(), "onra account add sol <address>"),
    ];
    let data = json!({
        "accounts": rows.iter().map(|(v, a, _)| json!({ "venue": v, "linked": a.is_some(), "account": a })).collect::<Vec<_>>()
    });
    Outcome::new(data, move |theme| {
        let s = i18n::s();
        let mut table = Table::new(vec![Align::Left, Align::Left, Align::Left]).indent(2);
        for (venue, account, how) in &rows {
            let dot = theme.rgb("●", venue_color(*venue));
            let name = Cell::new(
                format!("● {}", venue.name()),
                format!("{dot} {}", theme.dim(venue.name())),
            );
            let row = match account {
                Some(a) => {
                    let mode = if *venue == VenueId::Binance { s.account_read_only } else { "" };
                    vec![
                        name,
                        Cell::new(short(a), theme.strong(&short(a))),
                        Cell::new(mode, theme.faint(mode)),
                    ]
                }
                None => {
                    let hint = fill(s.account_how_add, &[("cmd", how)]);
                    vec![
                        name,
                        Cell::new(s.account_not_linked, theme.faint(s.account_not_linked)),
                        Cell::new(&hint, theme.faint(&hint)),
                    ]
                }
            };
            table.row(row);
        }
        let mut lines = vec![String::new()];
        lines.extend(table.render());
        lines.push(String::new());
        lines
    })
}

fn add(venue: &str, address: &str, config: &mut Config) -> Result<Outcome> {
    let venue = account_venue(venue)?;
    let address = address.trim().to_string();
    let invalid = |what: &str| {
        Error::new(ErrorCode::Usage, format!("`{address}` is not a valid {what} address"))
            .with_hint(json!({ "cause": "address", "address": address, "kind": what }))
    };
    match venue {
        VenueId::Hyperliquid if valid_evm(&address) => {
            config.accounts_mut().hyperliquid = Some(address.to_ascii_lowercase());
        }
        VenueId::Hyperliquid => return Err(invalid("Hyperliquid (0x…)")),
        VenueId::Jupiter if valid_solana(&address) => {
            config.accounts_mut().solana = Some(address.clone())
        }
        VenueId::Jupiter => return Err(invalid("Solana")),
        VenueId::Binance => {
            return Err(Error::new(ErrorCode::Usage, "binance links with an API key")
                .with_hint(json!({ "suggest": "onra account login bn" })));
        }
    }
    config.save()?;
    let data = json!({ "venue": venue, "account": address });
    Ok(Outcome::new(data, move |theme| {
        let text =
            fill(i18n::s().address_ok, &[("venue", venue.name()), ("address", &short(&address))]);
        vec![format!("  {} {text}", theme.brand("✓"))]
    }))
}

fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| Error::new(ErrorCode::Usage, format!("cannot read input: {e}")))?;
    Ok(line.trim().to_string())
}

async fn login(venue: &str, config: &mut Config, venues: &Venues) -> Result<Outcome> {
    if account_venue(venue)? != VenueId::Binance {
        return Err(Error::new(
            ErrorCode::Usage,
            "only binance logs in with a key; link wallets by address",
        )
        .with_hint(json!({ "suggest": "onra account add hl <address>" })));
    }
    let s = i18n::s();
    // Keys are typed by a person, never passed as arguments (shell history)
    // or handed over by an agent. Headless setups can use the environment.
    let key = match (std::env::var("ONRA_BINANCE_KEY"), std::env::var("ONRA_BINANCE_SECRET")) {
        (Ok(key), Ok(secret)) => BinanceKey { key, secret: Zeroizing::new(secret) },
        _ if !std::io::stdin().is_terminal() => {
            return Err(Error::new(
                ErrorCode::Credentials,
                "linking binance needs a terminal (or ONRA_BINANCE_KEY and ONRA_BINANCE_SECRET)",
            ));
        }
        _ => {
            let key = read_line(s.prompt_key)?;
            let secret = rpassword::prompt_password(s.prompt_secret)
                .map(Zeroizing::new)
                .map_err(|e| Error::new(ErrorCode::Usage, format!("cannot read input: {e}")))?;
            BinanceKey { key, secret }
        }
    };
    if key.key.is_empty() || key.secret.is_empty() {
        return Err(Error::new(ErrorCode::Usage, "both the API key and its secret are required"));
    }

    println!("  {}", s.checking_key);
    let can = venues.binance_restrictions(&key).await?;
    if can.enable_withdrawals {
        return Err(Error::new(
            ErrorCode::Credentials,
            "this key can withdraw funds; Onra refuses keys with withdrawal permission. Create a key without it",
        )
        .with_hint(json!({ "venue": "binance", "cause": "withdrawals" })));
    }
    if !can.enable_reading {
        return Err(Error::new(
            ErrorCode::Credentials,
            "this key cannot read the account; enable reading",
        ));
    }

    let stored = Zeroizing::new(
        serde_json::to_string(&StoredKey { key: key.key.clone(), secret: key.secret.to_string() })
            .expect("key serializes"),
    );
    let backend = keystore()?.set(BINANCE_ENTRY, &stored, &passphrase)?;
    config.accounts_mut().binance = Some(short(&key.key));
    config.save()?;

    let trade = can.enable_spot_and_margin_trading;
    let data = json!({
        "venue": "binance",
        "read": true,
        "trade": trade,
        "ip_restricted": can.ip_restrict,
        "stored_in": match backend { Backend::Keychain => "keychain", Backend::EncryptedFile => "file" },
    });
    Ok(Outcome::new(data, move |theme| {
        let s = i18n::s();
        let mode = if trade { "read + trade" } else { s.account_read_only };
        let where_ = match backend {
            Backend::Keychain => s.backend_keychain,
            Backend::EncryptedFile => s.backend_file,
        };
        let text = fill(s.linked_ok, &[("venue", "binance"), ("mode", mode), ("backend", where_)]);
        vec![format!("  {} {text}", theme.brand("✓"))]
    }))
}

fn remove(venue: &str, config: &mut Config) -> Result<Outcome> {
    let venue = account_venue(venue)?;
    match venue {
        VenueId::Binance => {
            keystore()?.delete(BINANCE_ENTRY)?;
            config.accounts_mut().binance = None;
        }
        VenueId::Hyperliquid => config.accounts_mut().hyperliquid = None,
        VenueId::Jupiter => config.accounts_mut().solana = None,
    }
    config.save()?;
    Ok(Outcome::new(json!({ "venue": venue, "linked": false }), move |theme| {
        vec![format!(
            "  {} {}",
            theme.brand("✓"),
            fill(i18n::s().removed, &[("venue", venue.name())])
        )]
    }))
}

// ---------------------------------------------------------------------------
// balance | positions | orders | portfolio

#[derive(Clone, Copy)]
pub struct Parts {
    pub balances: bool,
    pub positions: bool,
    pub orders: bool,
}

#[derive(Serialize)]
pub struct Portfolio {
    /// Sum of every balance with a known USD value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_usd: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub balances: Option<Vec<Balance>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub positions: Option<Vec<Position>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orders: Option<Vec<Order>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Warning>,
}

pub async fn fetch(venues: &Venues, accounts: &Accounts, parts: Parts) -> Result<Portfolio> {
    if accounts.binance.is_none() && accounts.hyperliquid.is_none() && accounts.solana.is_none() {
        return Err(Error::new(ErrorCode::Credentials, "no accounts are linked yet").with_hint(
            json!({ "cause": "no_accounts", "suggest": "onra account add hl <address>" }),
        ));
    }
    let per_venue = join_all(VenueId::ALL.map(|v| async move {
        let (b, p, o) = tokio::join!(
            async { if parts.balances { venues.balances(v, accounts).await } else { None } },
            async { if parts.positions { venues.positions(v, accounts).await } else { None } },
            async { if parts.orders { venues.orders(v, accounts).await } else { None } },
        );
        (v, b, p, o)
    }))
    .await;

    let mut out = Portfolio {
        total_usd: None,
        balances: parts.balances.then(Vec::new),
        positions: parts.positions.then(Vec::new),
        orders: parts.orders.then(Vec::new),
        warnings: Vec::new(),
    };
    for (venue, b, p, o) in per_venue {
        let mut warn = |error: Error| {
            if !out.warnings.iter().any(|w| w.venue == venue) {
                out.warnings.push(Warning { venue, error });
            }
        };
        match b {
            Some(Ok(list)) => out.balances.get_or_insert_with(Vec::new).extend(list),
            Some(Err(e)) => warn(e),
            None => {}
        }
        match p {
            Some(Ok(list)) => out.positions.get_or_insert_with(Vec::new).extend(list),
            Some(Err(e)) => warn(e),
            None => {}
        }
        match o {
            Some(Ok(list)) => out.orders.get_or_insert_with(Vec::new).extend(list),
            Some(Err(e)) => warn(e),
            None => {}
        }
    }
    if let Some(balances) = &mut out.balances {
        balances.sort_by(|a, b| {
            let pos = |v: VenueId| VenueId::ALL.iter().position(|x| *x == v);
            pos(a.venue)
                .cmp(&pos(b.venue))
                .then(b.usd_value.unwrap_or_default().cmp(&a.usd_value.unwrap_or_default()))
        });
        let priced: Vec<Decimal> = balances.iter().filter_map(|b| b.usd_value).collect();
        out.total_usd = (!priced.is_empty()).then(|| priced.iter().sum::<Decimal>().round_dp(2));
    }
    Ok(out)
}

/// Small or unverified balances are hidden unless asked for.
fn noteworthy(b: &Balance) -> bool {
    b.verified && b.usd_value.is_none_or(|v| v >= Decimal::ONE)
}

fn money(v: Decimal) -> String {
    format!("${}", format::price(v, 2))
}

/// Prices keep a fixed precision per magnitude; quantities keep their own.
fn amount(v: Decimal) -> String {
    format::price(v, format::price_dp(v))
}

fn qty(v: Decimal) -> String {
    format::quantity(v)
}

fn wallet_label(wallet: Option<&str>) -> &'static str {
    let s = i18n::s();
    match wallet {
        Some("spot") => s.wallet_spot,
        Some("perps") => s.wallet_perps,
        Some("earn") => s.wallet_earn,
        Some("solana") => s.wallet_solana,
        _ => "",
    }
}

fn venue_cell(venue: VenueId, first: bool, theme: &Theme) -> Cell {
    if !first {
        return Cell::empty();
    }
    Cell::new(
        format!("  ● {}", venue.name()),
        format!("  {} {}", theme.rgb("●", venue_color(venue)), theme.dim(venue.name())),
    )
}

pub fn render(p: &Portfolio, theme: &Theme, all: bool, hide: bool) -> Vec<String> {
    let s = i18n::s();
    let mask = |text: String| if hide { HIDDEN.to_string() } else { text };
    let mut lines = vec![String::new()];

    if let Some(total) = p.total_usd {
        lines.push(format!("  {}   {}", theme.faint(s.total), theme.strong(&mask(money(total)))));
        lines.push(String::new());
    }

    if let Some(balances) = &p.balances {
        let shown: Vec<&Balance> = balances.iter().filter(|b| all || noteworthy(b)).collect();
        let hidden = balances.len() - shown.len();
        let mut table =
            Table::new(vec![Align::Left, Align::Left, Align::Left, Align::Right, Align::Right])
                .indent(2);
        let mut last = None;
        for b in &shown {
            if last.is_some() && last != Some(b.venue) {
                table.gap();
            }
            let first = last != Some(b.venue);
            last = Some(b.venue);
            let asset = if b.venue == VenueId::Jupiter && !b.verified {
                Cell::new(
                    format!("⚠ {}", b.asset),
                    format!("{} {}", theme.warn("⚠"), theme.warn(&b.asset)),
                )
            } else {
                Cell::new(&b.asset, theme.strong(&b.asset))
            };
            let wallet = wallet_label(b.wallet.as_deref());
            let unit = b.usd_value.filter(|_| !b.total.is_zero()).map(|v| v / b.total);
            let qty = mask(unit.map_or_else(|| qty(b.total), |p| format::quantity_at(b.total, p)));
            let usd = b.usd_value.map(|v| mask(money(v))).unwrap_or_else(|| "—".into());
            table.row(vec![
                venue_cell(b.venue, first, theme),
                asset,
                Cell::new(wallet, theme.faint(wallet)),
                Cell::new(&qty, theme.rgb(&qty, onra_ui::palette::PAPER)),
                Cell::new(&usd, theme.dim(&usd)),
            ]);
        }
        if shown.is_empty() {
            lines.push(format!("  {}", theme.faint(s.no_balances)));
        } else {
            lines.extend(table.render());
        }
        if hidden > 0 {
            lines.push(String::new());
            lines.push(format!(
                "  {}",
                theme.faint(&fill(s.hidden_tokens, &[("n", &hidden.to_string())]))
            ));
        }
    }

    if let Some(positions) = &p.positions {
        lines.push(String::new());
        lines.push(format!("  {}", theme.strong(s.section_positions)));
        if positions.is_empty() {
            lines.push(format!("  {}", theme.faint(s.no_positions)));
        } else {
            lines.extend(positions_table(positions, theme, &mask));
        }
    }

    if let Some(orders) = &p.orders {
        lines.push(String::new());
        lines.push(format!("  {}", theme.strong(s.section_orders)));
        if orders.is_empty() {
            lines.push(format!("  {}", theme.faint(s.no_orders)));
        } else {
            lines.extend(orders_table(orders, theme, &mask));
        }
    }

    for w in &p.warnings {
        lines.push(String::new());
        lines.push(format!("  {} {}", theme.warn("!"), theme.warn(&i18n::error_text(&w.error))));
        if let Some(sg) = w.error.suggestion() {
            lines.push(format!("    {}", theme.dim(&format!("→ {sg}"))));
        }
    }
    lines.push(String::new());
    lines
}

fn positions_table(
    positions: &[Position],
    theme: &Theme,
    mask: &dyn Fn(String) -> String,
) -> Vec<String> {
    let s = i18n::s();
    let mut table = Table::new(vec![
        Align::Left,
        Align::Left,
        Align::Left,
        Align::Right,
        Align::Right,
        Align::Right,
        Align::Right,
        Align::Right,
    ])
    .indent(2);
    let mut last = None;
    for p in positions {
        let first = last != Some(p.venue);
        last = Some(p.venue);
        let (label, dir) = match p.side {
            PositionSide::Long => (s.long, std::cmp::Ordering::Greater),
            PositionSide::Short => (s.short, std::cmp::Ordering::Less),
        };
        let side = match p.leverage {
            Some(l) => format!("{label} {l}x"),
            None => label.to_string(),
        };
        let pnl_pct = p
            .margin
            .filter(|m| !m.is_zero())
            .map(|m| format!(" ({})", format::percent(p.unrealized_pnl / m * Decimal::ONE_HUNDRED)))
            .unwrap_or_default();
        let pnl = format!("{}{pnl_pct}", money_signed(p.unrealized_pnl));
        let pnl_dir = p.unrealized_pnl.round_dp(2).cmp(&Decimal::ZERO);
        let entry = format!("{} {}", s.entry, amount(p.entry_price));
        let mark = p.mark_price.map(|m| format!("{} {}", s.mark, amount(m))).unwrap_or_default();
        let liq = p
            .liquidation_price
            .map(|l| format!("{} {}", s.liq, amount(l)))
            .unwrap_or_else(|| format!("{} —", s.liq));
        let size = mask(qty(p.size));
        let pnl = mask(pnl);
        table.row(vec![
            venue_cell(p.venue, first, theme),
            Cell::new(&p.symbol, theme.strong(&p.symbol)),
            Cell::new(&side, theme.movement(&side, dir)),
            Cell::new(&size, theme.rgb(&size, onra_ui::palette::PAPER)),
            Cell::new(&entry, theme.dim(&entry)),
            Cell::new(&mark, theme.dim(&mark)),
            Cell::new(&pnl, theme.movement(&pnl, pnl_dir)),
            Cell::new(&liq, theme.faint(&liq)),
        ]);
    }
    table.render()
}

fn money_signed(v: Decimal) -> String {
    let sign = if v.is_sign_negative() && !v.round_dp(2).is_zero() { "−" } else { "+" };
    format!("{sign}{}", money(v.abs()))
}

fn orders_table(orders: &[Order], theme: &Theme, mask: &dyn Fn(String) -> String) -> Vec<String> {
    let s = i18n::s();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    let mut table = Table::new(vec![
        Align::Left,
        Align::Left,
        Align::Left,
        Align::Left,
        Align::Right,
        Align::Right,
        Align::Left,
        Align::Right,
    ])
    .indent(2);
    let mut last = None;
    for o in orders.iter().take(MAX_ORDERS_SHOWN) {
        let first = last != Some(o.venue);
        last = Some(o.venue);
        let (side, dir) = match o.side {
            OrderSide::Buy => (s.buy, std::cmp::Ordering::Greater),
            OrderSide::Sell => (s.sell, std::cmp::Ordering::Less),
        };
        let price = o.price.map(amount).unwrap_or_else(|| "—".into());
        let size = mask(qty(o.size));
        let filled = if o.size.is_zero() || o.filled.is_zero() {
            String::new()
        } else {
            let pct =
                format!("{}%", (o.filled / (o.size + o.filled) * Decimal::ONE_HUNDRED).round_dp(0));
            fill(s.filled, &[("pct", &pct)])
        };
        let age = age(now.saturating_sub(o.created_ms));
        table.row(vec![
            venue_cell(o.venue, first, theme),
            Cell::new(&o.symbol, theme.strong(&o.symbol)),
            Cell::new(side, theme.movement(side, dir)),
            Cell::new(&o.kind, theme.faint(&o.kind)),
            Cell::new(&price, theme.rgb(&price, onra_ui::palette::PAPER)),
            Cell::new(&size, theme.dim(&size)),
            Cell::new(&filled, theme.faint(&filled)),
            Cell::new(&age, theme.faint(&age)),
        ]);
    }
    let mut lines = table.render();
    if orders.len() > MAX_ORDERS_SHOWN {
        let more = (orders.len() - MAX_ORDERS_SHOWN).to_string();
        lines.push(format!("  {}", theme.faint(&fill(s.more_orders, &[("n", &more)]))));
    }
    lines
}

fn age(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_addresses() {
        assert!(valid_evm("0xc926ddba8b7617dbc65712f20cf8e1b58b8598d3"));
        assert!(!valid_evm("0xc926"));
        assert!(valid_solana("9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM"));
        assert!(!valid_solana("0OIl-not-base58-0OIl0OIl0OIl0OIl0OIl"));
        assert_eq!(account_venue("sol").unwrap(), VenueId::Jupiter);
        assert_eq!(account_venue("hl").unwrap(), VenueId::Hyperliquid);
    }

    #[test]
    fn shortens_and_ages() {
        assert_eq!(short("0xc926ddba8b7617dbc65712f20cf8e1b58b8598d3"), "0xc926…98d3");
        assert_eq!(age(90_000), "1m");
        assert_eq!(age(7_200_000), "2h");
    }
}
