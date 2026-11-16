//! `onra buy`, `onra sell`, `onra exec`, `onra approve`, `onra plans`.
//!
//! Every trade is two steps: the signer prices and judges a plan, then
//! carries it out. At a terminal the steps feel like one: the plan shows as
//! a card and Enter sends it. Anything else (a pipe, a script, an agent)
//! gets the plan as JSON, and plans an agent starts wait for a person's
//! `onra approve` unless the policy says otherwise.

mod types;

use std::io::IsTerminal;

use onra_core::{Error, ErrorCode, OrderSide, Result};
use onra_policy::{Check, Decision, Initiator};
use onra_ui::Theme;
use onra_ui::text::{pad, width};
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::json;

pub use types::{Amount, DEFAULT_SLIPPAGE_PCT, Execution, Plan, TradeRequest};

use crate::cli::TradeArgs;
use crate::commands::Outcome;
use crate::i18n::{self, fill};
use crate::signer::{self, ipc};

/// A person at a terminal confirms on screen; everyone else is an agent.
pub fn initiator() -> Initiator {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        Initiator::Human
    } else {
        Initiator::Agent
    }
}

async fn unlocked() -> Result<()> {
    match signer::status().await {
        Some(s) if s.unlocked => Ok(()),
        _ => Err(Error::new(ErrorCode::Locked, "the signer is locked")
            .with_hint(json!({ "cause": "locked", "suggest": "onra unlock" }))),
    }
}

async fn call<T: serde::de::DeserializeOwned>(request: &ipc::Request) -> Result<T> {
    let data = ipc::call(request).await?.ok_or_else(|| {
        Error::new(ErrorCode::Locked, "the signer is locked")
            .with_hint(json!({ "cause": "locked", "suggest": "onra unlock" }))
    })?;
    serde_json::from_value(data)
        .map_err(|e| Error::new(ErrorCode::Internal, format!("unexpected signer reply: {e}")))
}

fn denied(plan: &Plan) -> Error {
    let failed: Vec<&Check> = plan.verdict.failed().collect();
    let what = failed
        .iter()
        .map(|c| format!("{} ({} > {})", c.rule, c.value, c.limit))
        .collect::<Vec<_>>()
        .join(", ");
    Error::new(ErrorCode::Policy, format!("blocked by policy: {what}"))
        .with_hint(json!({ "cause": "denied", "plan": plan, "checks": failed }))
}

fn needs_approval(plan: &Plan) -> Error {
    Error::new(
        ErrorCode::ApprovalRequired,
        format!("plan {} waits for a person to approve it", plan.id),
    )
    .with_hint(json!({
        "cause": "approval",
        "plan_id": plan.id,
        "summary": plan.summary(),
        "expires_ms": plan.expires_ms,
        "suggest": format!("onra approve {}", plan.id),
    }))
}

fn parse_pct(text: &str) -> Result<Decimal> {
    text.trim_end_matches('%')
        .parse()
        .ok()
        .filter(|v: &Decimal| *v > Decimal::ZERO && *v <= Decimal::from(50))
        .ok_or_else(|| {
            Error::new(ErrorCode::Usage, format!("`{text}` is not a slippage between 0 and 50%"))
        })
}

// ---------------------------------------------------------------------------
// buy | sell

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Buy,
    Sell,
    Long,
    Short,
    Close,
}

pub async fn trade(kind: Kind, args: TradeArgs, theme: Option<Theme>) -> Result<Outcome> {
    let perp = matches!(kind, Kind::Long | Kind::Short | Kind::Close);
    if args.lev.is_some() && !matches!(kind, Kind::Long | Kind::Short) {
        return Err(Error::new(ErrorCode::Usage, "--lev is for long and short"));
    }
    let request = TradeRequest {
        market: args.market,
        side: if matches!(kind, Kind::Buy | Kind::Long) { OrderSide::Buy } else { OrderSide::Sell },
        amount: Amount::parse(&args.amount)?,
        limit: args.limit,
        slippage_pct: args.slippage.as_deref().map(parse_pct).transpose()?,
        testnet: args.testnet,
        client_id: args.client_id,
        perp,
        leverage: args.lev,
        close: kind == Kind::Close,
    };
    let initiator = initiator();
    unlocked().await?;
    let plan: Plan = call(&ipc::Request::Plan { request, initiator }).await?;
    let human = initiator == Initiator::Human && theme.is_some();

    if args.plan {
        if plan.verdict.decision == Decision::Deny {
            return Err(denied(&plan));
        }
        let data = serde_json::to_value(&plan).expect("plan serializes");
        return Ok(Outcome::new(data, move |theme| card(&plan, theme, false)));
    }
    match plan.verdict.decision {
        Decision::Deny => {
            if let Some(theme) = &theme
                && human
            {
                for line in card(&plan, theme, false) {
                    println!("{line}");
                }
            }
            Err(denied(&plan))
        }
        Decision::NeedsApproval => Err(needs_approval(&plan)),
        Decision::Allow => {
            if let Some(theme) = theme.filter(|_| human) {
                for line in card(&plan, &theme, !args.yes) {
                    println!("{line}");
                }
                if !args.yes && !confirm()? {
                    let _ = ipc::call(&ipc::Request::Discard { plan_id: plan.id.clone() }).await;
                    return Ok(Outcome::new(
                        json!({ "plan_id": plan.id, "cancelled": true }),
                        |theme| {
                            vec![
                                format!("  {} {}", theme.faint("·"), i18n::s().trade_cancelled),
                                String::new(),
                            ]
                        },
                    ));
                }
            }
            execute(&plan.id, initiator).await
        }
    }
}

async fn execute(plan_id: &str, initiator: Initiator) -> Result<Outcome> {
    let exec: Execution =
        call(&ipc::Request::Exec { plan_id: plan_id.to_string(), initiator }).await?;
    let data = serde_json::to_value(&exec).expect("execution serializes");
    Ok(Outcome::new(data, move |theme| result_lines(&exec, theme)))
}

/// Enter or y sends; Esc, n, q or Ctrl+C cancels.
fn confirm() -> Result<bool> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use crossterm::terminal;
    let io = |e: std::io::Error| Error::new(ErrorCode::Internal, format!("terminal: {e}"));
    terminal::enable_raw_mode().map_err(io)?;
    let answer = loop {
        match event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Enter | KeyCode::Char('y' | 'Y') => break Ok(true),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Ok(false);
                }
                KeyCode::Esc | KeyCode::Char('n' | 'N' | 'q') => break Ok(false),
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(io(e)),
        }
    };
    let _ = terminal::disable_raw_mode();
    answer
}

// ---------------------------------------------------------------------------
// exec | approve | plans

pub async fn exec(plan_id: &str) -> Result<Outcome> {
    unlocked().await?;
    execute(plan_id, initiator()).await
}

pub async fn approve(plan_id: &str, theme: Option<Theme>) -> Result<Outcome> {
    signer::require_human("approving a plan", &format!("onra approve {plan_id}"))?;
    unlocked().await?;
    let plan: Plan = call(&ipc::Request::Show { plan_id: plan_id.to_string() }).await?;
    if let Some(theme) = &theme {
        for line in card(&plan, theme, false) {
            println!("{line}");
        }
    }
    let passphrase = signer::hidden(i18n::s().prompt_approve)?;
    let exec: Execution =
        call(&ipc::Request::Approve { plan_id: plan_id.to_string(), passphrase }).await?;
    let data = serde_json::to_value(&exec).expect("execution serializes");
    Ok(Outcome::new(data, move |theme| result_lines(&exec, theme)))
}

pub async fn plans() -> Result<Outcome> {
    let plans: Vec<Plan> = match ipc::call(&ipc::Request::Plans).await? {
        Some(data) => serde_json::from_value(data).unwrap_or_default(),
        None => Vec::new(),
    };
    let data = json!({ "plans": plans });
    Ok(Outcome::new(data, move |theme| {
        let s = i18n::s();
        let mut lines = vec![String::new()];
        if plans.is_empty() {
            lines.push(format!("  {}", theme.faint(s.plans_empty)));
        }
        let now = crate::signer::guard::now_ms();
        for p in &plans {
            let state = match p.verdict.decision {
                Decision::NeedsApproval => theme.warn(s.plan_waiting),
                _ => theme.faint(s.plan_ready),
            };
            let left = p.expires_ms.saturating_sub(now) / 1000;
            lines.push(format!(
                "  {}  {}  {}  {}",
                theme.strong(&p.id),
                p.summary(),
                state,
                theme.faint(&signer::minutes(left))
            ));
        }
        lines.push(String::new());
        lines
    }))
}

// ---------------------------------------------------------------------------
// rendering

const INNER: usize = 62;

fn money(v: Decimal) -> String {
    onra_ui::format::quantity(v)
}

fn qty(v: Decimal) -> String {
    v.normalize().to_string()
}

/// A rounded box around plan lines; `confirm` puts the key hints in the
/// bottom border.
pub fn card(plan: &Plan, theme: &Theme, confirm: bool) -> Vec<String> {
    let s = i18n::s();
    let buy = plan.side == OrderSide::Buy;
    let title = match plan.action() {
        "LONG" => s.card_long,
        "SHORT" => s.card_short,
        "CLOSE" => s.card_close,
        "BUY" => s.card_buy,
        _ => s.card_sell,
    };
    let net = if plan.testnet { format!(" · {}", s.testnet) } else { String::new() };
    let right =
        if plan.perp { format!("hyperliquid perp{net}") } else { format!("binance spot{net}") };
    // Stablecoin amounts to the cent, rounded against the trader: a buy
    // spends at most this, a sell receives at least this.
    let cost = (plan.qty * plan.price).round_dp_with_strategy(
        2,
        if buy { RoundingStrategy::AwayFromZero } else { RoundingStrategy::ToZero },
    );

    let mut body: Vec<(String, String)> = Vec::new();
    let line = |plain: String, styled: String| (plain, styled);
    body.push(line(String::new(), String::new()));
    let headline_plain = format!("{} {}", qty(plan.qty), plan.base);
    // Perps don't spend their notional, only margin: say what the size is worth.
    let bound = if plan.perp {
        fill(s.notional_about, &[("amount", &format!("${}", onra_ui::format::price(cost, 2)))])
    } else {
        fill(
            if buy { s.for_at_most } else { s.for_at_least },
            &[("amount", &format!("{} {}", onra_ui::format::price(cost, 2), plan.quote))],
        )
    };
    body.push(line(
        format!("  {headline_plain}   {bound}"),
        format!("  {}   {}", theme.strong(&headline_plain), theme.dim(&bound)),
    ));
    body.push(line(String::new(), String::new()));

    let label = |t: &str| pad(t, 11);
    let price_text = match plan.slippage_pct {
        Some(slip) => {
            let best = fill(
                if buy { s.best_ask } else { s.best_bid },
                &[("p", &money(plan.reference_price)), ("slip", &slip.normalize().to_string())],
            );
            format!("{} {}  ·  {best}", if buy { "≤" } else { "≥" }, money(plan.price))
        }
        None => money(plan.price),
    };
    body.push(line(
        format!("  {}{price_text}", label(s.lbl_price)),
        format!("  {}{price_text}", theme.dim(&label(s.lbl_price))),
    ));
    let order = if plan.time_in_force == "gtc" { s.order_gtc } else { s.order_ioc };
    body.push(line(
        format!("  {}{order}", label(s.lbl_order)),
        format!("  {}{order}", theme.dim(&label(s.lbl_order))),
    ));
    if let Some(lev) = plan.leverage {
        let margin = onra_ui::format::price(plan.qty * plan.price / lev, 2);
        let text = fill(
            s.leverage_line,
            &[("lev", &lev.normalize().to_string()), ("margin", &format!("${margin}"))],
        );
        body.push(line(
            format!("  {}{text}", label(s.lbl_leverage)),
            format!("  {}{text}", theme.dim(&label(s.lbl_leverage))),
        ));
    }
    if plan.reduce_only {
        body.push(line(
            format!("  {}{}", label(""), s.reduce_only_line),
            format!("  {}{}", label(""), theme.faint(s.reduce_only_line)),
        ));
    }
    if !plan.simulated && plan.perp {
        body.push(line(
            format!("  {}✓ {}", label(s.lbl_checked), s.checked_hl),
            format!("  {}{} {}", theme.dim(&label(s.lbl_checked)), theme.brand("✓"), s.checked_hl),
        ));
    }
    if plan.simulated {
        body.push(line(
            format!("  {}✓ {}", label(s.lbl_simulated), s.simulated_ok),
            format!(
                "  {}{} {}",
                theme.dim(&label(s.lbl_simulated)),
                theme.brand("✓"),
                s.simulated_ok
            ),
        ));
    }
    body.push(line(String::new(), String::new()));
    // Conditions that pass (venue allowed, asset verified, not halted) are
    // noise on a card; limits always show, with how close the trade comes.
    let shown = plan
        .verdict
        .checks
        .iter()
        .filter(|c| !c.passed || !matches!(c.rule.as_str(), "venue" | "verified" | "halt"));
    for (i, c) in shown.enumerate() {
        let name = pad(i18n::rule_name(&c.rule), 12);
        let mark_plain = if c.passed { "✓" } else { "✗" };
        let mark = if c.passed { theme.brand("✓") } else { theme.error("✗") };
        let lbl = if i == 0 { s.lbl_policy } else { "" };
        let detail = match c.rule.as_str() {
            "venue" | "verified" | "halt" => format!("{name}{}", c.value),
            _ => format!("{name}{} {} {}", c.value, if c.passed { "≤" } else { ">" }, c.limit),
        };
        body.push(line(
            format!("  {}{mark_plain} {detail}", label(lbl)),
            format!(
                "  {}{mark} {}",
                theme.dim(&label(lbl)),
                if c.passed { theme.faint(&detail) } else { theme.error(&detail) }
            ),
        ));
    }
    if plan.verdict.decision == Decision::NeedsApproval {
        body.push(line(
            format!("  {}! {}", label(""), s.plan_waiting),
            format!("  {}{} {}", label(""), theme.warn("!"), theme.warn(s.plan_waiting)),
        ));
    }
    body.push(line(String::new(), String::new()));

    // Borders: ╭─ TITLE ──── right ─╮ and ╰──── hints ─╯
    let top_fill = INNER.saturating_sub(width(title) + width(&right) + 6);
    let mut out = vec![
        String::new(),
        format!(
            "  {}{}{}{}{}",
            theme.faint("╭─ "),
            theme.brand(title),
            theme.faint(&format!(" {} ", "─".repeat(top_fill))),
            theme.dim(&right),
            theme.faint(" ─╮")
        ),
    ];
    for (plain, styled) in body {
        let gap = INNER.saturating_sub(width(&plain));
        out.push(format!("  {}{styled}{}{}", theme.faint("│"), " ".repeat(gap), theme.faint("│")));
    }
    let hint = if confirm { format!(" {} ", s.confirm_hint) } else { format!(" {} ", plan.id) };
    let bottom_fill = INNER.saturating_sub(width(&hint) + 1);
    out.push(format!(
        "  {}{}{}",
        theme.faint(&format!("╰{}", "─".repeat(bottom_fill))),
        theme.dim(&hint),
        theme.faint("─╯")
    ));
    out
}

fn result_lines(exec: &Execution, theme: &Theme) -> Vec<String> {
    let s = i18n::s();
    let net = if exec.testnet { format!(" · {}", s.testnet) } else { String::new() };
    let id = format!("{}{net}", fill(s.order_id, &[("id", &exec.order_id)]));
    let mut lines = Vec::new();
    if exec.executed_qty.is_zero() {
        let text = if exec.status == "new" { s.order_resting } else { s.nothing_filled };
        lines.push(format!("  {} {text}", theme.warn("!")));
    } else {
        let fee = exec
            .fee
            .iter()
            .filter(|(a, _)| !a.is_zero())
            .map(|(a, asset)| format!("{} {asset}", a.normalize()))
            .collect::<Vec<_>>()
            .join(" + ");
        let side = match exec.action.as_str() {
            "LONG" => s.opened_long,
            "SHORT" => s.opened_short,
            "CLOSE" => s.closed,
            _ if exec.side == OrderSide::Buy => s.bought,
            _ => s.sold,
        };
        let text = fill(
            s.filled_ok,
            &[
                ("side", side),
                ("qty", &format!("{} {}", qty(exec.executed_qty), exec.base)),
                ("avg", &exec.avg_price.map(money).unwrap_or_default()),
                ("total", &format!("{} {}", onra_ui::format::price(exec.quote_qty, 2), exec.quote)),
            ],
        );
        lines.push(format!("  {} {text}", theme.brand("✓")));
        if !fee.is_empty() {
            lines.push(format!("    {}", theme.faint(&fill(s.fee_paid, &[("fee", &fee)]))));
        }
    }
    lines.push(format!("    {}", theme.faint(&id)));
    lines.push(String::new());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use onra_core::VenueId;
    use onra_policy::Verdict;

    pub(crate) fn sample(decision: Decision) -> Plan {
        let check = |rule: &str, passed: bool, value: &str, limit: &str| Check {
            rule: rule.into(),
            passed,
            value: value.into(),
            limit: limit.into(),
        };
        Plan {
            id: "p3fa9c2e1".into(),
            venue: VenueId::Binance,
            testnet: true,
            symbol: "BTCUSDT".into(),
            base: "BTC".into(),
            quote: "USDT".into(),
            side: OrderSide::Buy,
            time_in_force: "ioc".into(),
            qty: "0.00076".parse().unwrap(),
            price: "65069.62".parse().unwrap(),
            reference_price: "64745.87".parse().unwrap(),
            notional_usd: "49.45".parse().unwrap(),
            slippage_pct: Some(DEFAULT_SLIPPAGE_PCT),
            simulated: true,
            verdict: Verdict {
                decision,
                checks: vec![
                    check("venue", true, "bn", "bn,hl,jup"),
                    check("verified", true, "verified", "verified"),
                    check("max_trade_usd", decision != Decision::Deny, "$49.45", "$40"),
                    check("max_daily_usd", true, "$49.45", "$500"),
                    check("max_slippage_pct", true, "0.5%", "1%"),
                    check("max_trades_per_minute", true, "1", "10"),
                ],
            },
            initiator: Initiator::Human,
            created_ms: 0,
            expires_ms: 60_000,
            client_id: "onra-p3fa9c2e1".into(),
            perp: false,
            leverage: None,
            reduce_only: false,
            asset_index: None,
        }
    }

    fn perp(close: bool) -> Plan {
        let mut p = sample(Decision::Allow);
        p.venue = VenueId::Hyperliquid;
        p.symbol = "ETH".into();
        p.base = "ETH".into();
        p.quote = "USDC".into();
        p.qty = "0.006".parse().unwrap();
        p.price = "1807.1".parse().unwrap();
        p.perp = true;
        p.reduce_only = close;
        p.leverage = (!close).then(|| Decimal::from(3));
        p.simulated = false;
        p
    }

    #[test]
    fn cards_are_rectangular() {
        let theme = Theme::plain(onra_ui::Convention::West);
        for lang in [i18n::Lang::En, i18n::Lang::Zh] {
            i18n::set(lang);
            let plans = [
                sample(Decision::Allow),
                sample(Decision::Deny),
                sample(Decision::NeedsApproval),
                perp(false),
                perp(true),
            ];
            for plan in plans {
                let decision = plan.verdict.decision;
                let lines = card(&plan, &theme, decision == Decision::Allow);
                let widths: Vec<usize> = lines.iter().skip(1).map(|l| width(l)).collect();
                assert!(
                    widths.iter().all(|w| *w == widths[0]),
                    "{lang:?} {decision:?}: {widths:?}"
                );
                if std::env::var_os("ONRA_SHOW_CARDS").is_some() {
                    println!("{}", lines.join("\n"));
                }
            }
        }
        i18n::set(i18n::Lang::En);
    }
}
