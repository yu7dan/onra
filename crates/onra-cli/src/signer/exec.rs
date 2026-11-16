//! Inside the signer: turning a trade request into a plan, and a plan into
//! an order. Everything that decides how much money moves happens here, from
//! prices the signer fetches itself: a caller can ask, never dictate.

use std::sync::atomic::{AtomicU64, Ordering};

use onra_core::{Error, ErrorCode, MarketKind, MarketQuery, OrderSide, Result, VenueId};
use onra_policy::{Initiator, Intent, Verdict, evaluate};
use onra_venues::{BinanceKey, SpotOrder, TimeInForce, Venues};
use rust_decimal::Decimal;
use serde_json::json;

use super::guard;
use crate::trade::{Amount, DEFAULT_SLIPPAGE_PCT, Execution, Plan, TradeRequest};

/// Quotes treated as one US dollar for limits.
const STABLE_QUOTES: [&str; 3] = ["USDT", "USDC", "FDUSD"];
/// A plan a person confirms on screen is good for a minute.
const PLAN_TTL_MS: u64 = 60_000;
/// An agent's plan waits this long for approval (it is re-checked then).
const APPROVAL_TTL_MS: u64 = 10 * 60_000;

fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let seed =
        format!("{nanos}:{}:{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
    format!("p{}", &hex::encode(onra_signer::keccak(seed.as_bytes()))[..8])
}

fn usage(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::Usage, message)
}

/// Checks the guard rails and judges a trade. A tampered policy file or a
/// broken audit chain stops everything: the signer cannot trust its own
/// limits or its own record any more.
pub fn judge(
    seal: &[u8],
    notional_usd: Decimal,
    slippage_pct: Option<Decimal>,
    initiator: Initiator,
) -> Result<Verdict> {
    let log = guard::audit_log()?;
    log.verify(seal)?;
    let policy = guard::load_policy(seal)?;
    let intent = Intent {
        venue: VenueId::Binance,
        notional_usd,
        leverage: None,
        slippage_pct,
        verified: true,
        reduce_only: false,
        initiator,
    };
    Ok(evaluate(&policy, &intent, &log.usage(guard::now_ms())?, guard::halted()?))
}

pub async fn plan(
    req: &TradeRequest,
    initiator: Initiator,
    venues: &Venues,
    key: &BinanceKey,
    seal: &[u8],
) -> Result<Plan> {
    let query: MarketQuery = req.market.parse()?;
    match query.venue {
        None | Some(VenueId::Binance) => {}
        Some(_) => {
            return Err(usage(
                "buy and sell trade Binance spot for now; Hyperliquid and Jupiter come next",
            )
            .with_hint(json!({ "cause": "venue_not_tradable" })));
        }
    }
    if query.kind == Some(MarketKind::Perp) {
        return Err(usage("perps are traded with long and short (coming soon)"));
    }
    let base = query.base.to_ascii_uppercase();
    let quote = query.quote.clone().unwrap_or_else(|| "USDT".into()).to_ascii_uppercase();
    if !STABLE_QUOTES.contains(&quote.as_str()) {
        return Err(usage(format!(
            "only stablecoin pairs can be traded for now, not {base}/{quote}"
        )));
    }
    let buy = req.side == OrderSide::Buy;
    let spot = venues.binance_spot(req.testnet);
    let symbol = format!("{base}{quote}");
    let (rules, top) = tokio::try_join!(spot.rules(&symbol), spot.top(&symbol))?;
    if !rules.trading {
        return Err(Error::new(ErrorCode::Venue, format!("{symbol} is not trading right now")));
    }

    let reference = if buy { top.ask } else { top.bid };
    let (price, tif, slippage_pct) = match req.limit {
        Some(limit) => (rules.price(limit, buy), TimeInForce::Gtc, None),
        None => {
            let slip = req.slippage_pct.unwrap_or(DEFAULT_SLIPPAGE_PCT);
            let factor = Decimal::ONE
                + slip / Decimal::ONE_HUNDRED * if buy { Decimal::ONE } else { -Decimal::ONE };
            (rules.price(reference * factor, buy), TimeInForce::Ioc, Some(slip))
        }
    };
    if price <= Decimal::ZERO {
        return Err(usage("the price works out to zero"));
    }

    // Buys spend the quote asset, sells spend the base.
    let spend_asset = if buy { &quote } else { &base };
    let free = spot.free(key, spend_asset).await?;
    let raw = match &req.amount {
        Amount::Units { qty, asset } if *asset == base => *qty,
        Amount::Units { qty, asset } if *asset == quote => *qty / price,
        Amount::Units { asset, .. } => {
            return Err(usage(format!("{asset} is not part of {base}/{quote}")));
        }
        Amount::Usd { usd } => *usd / price,
        Amount::Percent { pct } => {
            let share = free * *pct / Decimal::ONE_HUNDRED;
            if buy { share / price } else { share }
        }
        Amount::Max => {
            if buy {
                free / price
            } else {
                free
            }
        }
    };
    let qty = rules.qty(raw);
    let cost = qty * price;
    if qty < rules.min_qty || qty.is_zero() || cost < rules.min_notional {
        return Err(Error::new(
            ErrorCode::Venue,
            format!(
                "below Binance's minimum order for {symbol} (at least {} {quote})",
                rules.min_notional.normalize()
            ),
        )
        .with_hint(json!({ "venue": "binance", "cause": "min_order", "min_notional": rules.min_notional.normalize().to_string(), "quote": quote })));
    }
    let need = if buy { cost } else { qty };
    if need > free {
        return Err(Error::new(
            ErrorCode::Venue,
            format!("not enough {spend_asset}: {} needed, {} available", need.normalize(), free.normalize()),
        )
        .with_hint(json!({ "venue": "binance", "cause": "insufficient_balance", "asset": spend_asset, "need": need.normalize().to_string(), "have": free.normalize().to_string() })));
    }

    let id = new_id();
    let client_id = req.client_id.clone().unwrap_or_else(|| format!("onra-{id}"));
    let order =
        SpotOrder { symbol: symbol.clone(), buy, qty, price, tif, client_id: client_id.clone() };
    spot.test(key, &order).await?;

    let notional_usd = (qty * price.max(reference))
        .round_dp_with_strategy(2, rust_decimal::RoundingStrategy::AwayFromZero);
    let verdict = judge(seal, notional_usd, slippage_pct, initiator)?;
    let now = guard::now_ms();
    let ttl = if verdict.decision == onra_policy::Decision::NeedsApproval {
        APPROVAL_TTL_MS
    } else {
        PLAN_TTL_MS
    };
    let plan = Plan {
        id,
        venue: VenueId::Binance,
        testnet: req.testnet,
        symbol,
        base,
        quote,
        side: req.side,
        time_in_force: match tif {
            TimeInForce::Ioc => "ioc",
            TimeInForce::Gtc => "gtc",
        }
        .into(),
        qty,
        price,
        reference_price: reference,
        notional_usd,
        slippage_pct,
        simulated: true,
        verdict,
        initiator,
        created_ms: now,
        expires_ms: now + ttl,
        client_id,
    };
    guard::audit_log()?.append(
        seal,
        now,
        "plan",
        Some(initiator),
        json!({
            "plan_id": plan.id,
            "summary": plan.summary(),
            "notional_usd": plan.notional_usd.to_string(),
            "decision": plan.verdict.decision,
        }),
    )?;
    Ok(plan)
}

pub async fn execute(
    plan: &Plan,
    initiator: Initiator,
    venues: &Venues,
    key: &BinanceKey,
    seal: &[u8],
) -> Result<Execution> {
    let log = guard::audit_log()?;
    log.append(
        seal,
        guard::now_ms(),
        onra_policy::audit::SUBMITTED,
        Some(initiator),
        json!({
            "plan_id": plan.id,
            "summary": plan.summary(),
            "notional_usd": plan.notional_usd.to_string(),
            "testnet": plan.testnet,
        }),
    )?;
    let order = SpotOrder {
        symbol: plan.symbol.clone(),
        buy: plan.side == OrderSide::Buy,
        qty: plan.qty,
        price: plan.price,
        tif: if plan.time_in_force == "gtc" { TimeInForce::Gtc } else { TimeInForce::Ioc },
        client_id: plan.client_id.clone(),
    };
    match venues.binance_spot(plan.testnet).place(key, &order).await {
        Ok(fill) => {
            let exec = Execution {
                plan_id: plan.id.clone(),
                side: plan.side,
                base: plan.base.clone(),
                quote: plan.quote.clone(),
                order_id: fill.order_id,
                status: fill.status.to_ascii_lowercase(),
                executed_qty: fill.executed_qty,
                quote_qty: fill.quote_qty,
                avg_price: fill.avg_price,
                fee: fill.fee,
                summary: plan.summary(),
                testnet: plan.testnet,
            };
            log.append(
                seal,
                guard::now_ms(),
                "result",
                Some(initiator),
                json!({
                    "plan_id": exec.plan_id,
                    "order_id": exec.order_id,
                    "status": exec.status,
                    "executed_qty": exec.executed_qty.normalize().to_string(),
                    "quote_qty": exec.quote_qty.normalize().to_string(),
                }),
            )?;
            Ok(exec)
        }
        Err(e) => {
            log.append(
                seal,
                guard::now_ms(),
                "failed",
                Some(initiator),
                json!({ "plan_id": plan.id, "error": e.message }),
            )?;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_ids_are_short_and_distinct() {
        let a = new_id();
        let b = new_id();
        assert_eq!(a.len(), 9);
        assert!(a.starts_with('p'));
        assert_ne!(a, b);
    }
}
