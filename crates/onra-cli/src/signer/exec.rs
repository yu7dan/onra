//! Inside the signer: turning a trade request into a plan, and a plan into
//! an order. Everything that decides how much money moves happens here, from
//! prices the signer fetches itself: a caller can ask, never dictate.

use std::sync::atomic::{AtomicU64, Ordering};

use onra_core::{Error, ErrorCode, MarketKind, MarketQuery, OrderSide, Result, VenueId};
use onra_policy::{Decision, Initiator, Intent, Verdict, evaluate};
use onra_signer::EvmKey;
use onra_signer::hyperliquid::{sign_l1_action, wire_decimal};
use onra_venues::{BinanceKey, SpotOrder, TimeInForce, Venues, parse_order_status};
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{Value, json};

use super::guard;
use crate::trade::{Amount, DEFAULT_SLIPPAGE_PCT, Execution, Plan, TradeRequest};

/// Quotes treated as one US dollar for limits.
const STABLE_QUOTES: [&str; 3] = ["USDT", "USDC", "FDUSD"];
/// A plan a person confirms on screen is good for a minute.
const PLAN_TTL_MS: u64 = 60_000;
/// An agent's plan waits this long for approval (it is re-checked then).
const APPROVAL_TTL_MS: u64 = 10 * 60_000;
/// Hyperliquid refuses orders worth less than this.
const HL_MIN_NOTIONAL: Decimal = Decimal::TEN;

/// The key a plan is carried out with.
pub enum Signer {
    Binance(BinanceKey),
    /// The API wallet, and the account it trades for.
    Hyperliquid {
        key: EvmKey,
        user: String,
    },
}

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

fn up2(v: Decimal) -> Decimal {
    v.round_dp_with_strategy(2, RoundingStrategy::AwayFromZero)
}

/// The venue a request goes to: perps on Hyperliquid, spot on Binance.
pub fn venue_of(request: &TradeRequest) -> VenueId {
    if request.perp { VenueId::Hyperliquid } else { VenueId::Binance }
}

/// Checks the guard rails and judges a plan. A tampered policy file or a
/// broken audit chain stops everything: the signer cannot trust its own
/// limits or its own record any more.
pub fn judge(seal: &[u8], plan: &Plan, initiator: Initiator) -> Result<Verdict> {
    let log = guard::audit_log()?;
    log.verify(seal)?;
    let policy = guard::load_policy(seal)?;
    let intent = Intent {
        venue: plan.venue,
        notional_usd: plan.notional_usd,
        leverage: plan.leverage,
        slippage_pct: plan.slippage_pct,
        verified: true,
        reduce_only: plan.reduce_only,
        initiator,
    };
    Ok(evaluate(&policy, &intent, &log.usage(guard::now_ms())?, guard::halted()?))
}

/// Price bound and time in force: a limit rests (GTC); otherwise the best
/// price moved by the slippage, filling now or never (IOC).
fn bound(
    req: &TradeRequest,
    buy: bool,
    reference: Decimal,
    round: impl Fn(Decimal, bool) -> Decimal,
) -> (Decimal, TimeInForce, Option<Decimal>) {
    match req.limit {
        Some(limit) => (round(limit, buy), TimeInForce::Gtc, None),
        None => {
            let slip = req.slippage_pct.unwrap_or(DEFAULT_SLIPPAGE_PCT);
            let sign = if buy { Decimal::ONE } else { -Decimal::ONE };
            let factor = Decimal::ONE + slip / Decimal::ONE_HUNDRED * sign;
            (round(reference * factor, buy), TimeInForce::Ioc, Some(slip))
        }
    }
}

fn insufficient(asset: &str, need: Decimal, have: Decimal, venue: &str) -> Error {
    Error::new(
        ErrorCode::Venue,
        format!("not enough {asset}: {} needed, {} available", need.normalize(), have.normalize()),
    )
    .with_hint(json!({ "venue": venue, "cause": "insufficient_balance", "asset": asset, "need": need.normalize().to_string(), "have": have.normalize().to_string() }))
}

fn min_order(venue: &str, min: Decimal, quote: &str) -> Error {
    Error::new(
        ErrorCode::Venue,
        format!("below {venue}'s minimum order (at least {} {quote})", min.normalize()),
    )
    .with_hint(json!({ "venue": venue, "cause": "min_order", "min_notional": min.normalize().to_string(), "quote": quote }))
}

/// Prices and judges a request, and records the plan.
pub async fn plan(
    req: &TradeRequest,
    initiator: Initiator,
    venues: &Venues,
    signer: &Signer,
    seal: &[u8],
) -> Result<Plan> {
    let mut plan = match signer {
        Signer::Binance(key) => plan_binance(req, venues, key).await?,
        Signer::Hyperliquid { user, .. } => plan_hyperliquid(req, venues, user).await?,
    };
    plan.initiator = initiator;
    plan.verdict = judge(seal, &plan, initiator)?;
    let now = guard::now_ms();
    let ttl = if plan.verdict.decision == Decision::NeedsApproval {
        APPROVAL_TTL_MS
    } else {
        PLAN_TTL_MS
    };
    plan.created_ms = now;
    plan.expires_ms = now + ttl;
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

/// A plan with everything but the verdict and the clock filled in.
fn draft(id: String, client_id: String) -> Plan {
    Plan {
        id,
        venue: VenueId::Binance,
        testnet: false,
        symbol: String::new(),
        base: String::new(),
        quote: String::new(),
        side: OrderSide::Buy,
        time_in_force: "ioc".into(),
        qty: Decimal::ZERO,
        price: Decimal::ZERO,
        reference_price: Decimal::ZERO,
        notional_usd: Decimal::ZERO,
        slippage_pct: None,
        simulated: false,
        verdict: Verdict { decision: Decision::Deny, checks: Vec::new() },
        initiator: Initiator::Agent,
        created_ms: 0,
        expires_ms: 0,
        client_id,
        perp: false,
        leverage: None,
        reduce_only: false,
        asset_index: None,
    }
}

fn tif_name(tif: TimeInForce) -> String {
    match tif {
        TimeInForce::Ioc => "ioc",
        TimeInForce::Gtc => "gtc",
    }
    .into()
}

// ---------------------------------------------------------------------------
// Binance spot

async fn plan_binance(req: &TradeRequest, venues: &Venues, key: &BinanceKey) -> Result<Plan> {
    let query: MarketQuery = req.market.parse()?;
    if !matches!(query.venue, None | Some(VenueId::Binance)) {
        return Err(usage("buy and sell trade Binance spot; perps use long, short and close")
            .with_hint(json!({ "cause": "venue_not_tradable" })));
    }
    if query.kind == Some(MarketKind::Perp) {
        return Err(usage("perps are traded with long, short and close"));
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
    let (price, tif, slippage_pct) = bound(req, buy, reference, |p, b| rules.price(p, b));
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
        return Err(min_order("binance", rules.min_notional, &quote));
    }
    let need = if buy { cost } else { qty };
    if need > free {
        return Err(insufficient(spend_asset, need, free, "binance"));
    }

    let id = new_id();
    let client_id = req.client_id.clone().unwrap_or_else(|| format!("onra-{id}"));
    let order =
        SpotOrder { symbol: symbol.clone(), buy, qty, price, tif, client_id: client_id.clone() };
    spot.test(key, &order).await?;

    let mut plan = draft(id, client_id);
    plan.testnet = req.testnet;
    plan.symbol = symbol;
    plan.base = base;
    plan.quote = quote;
    plan.side = req.side;
    plan.time_in_force = tif_name(tif);
    plan.qty = qty;
    plan.price = price;
    plan.reference_price = reference;
    plan.notional_usd = up2(qty * price.max(reference));
    plan.slippage_pct = slippage_pct;
    plan.simulated = true;
    Ok(plan)
}

// ---------------------------------------------------------------------------
// Hyperliquid perps

async fn plan_hyperliquid(req: &TradeRequest, venues: &Venues, user: &str) -> Result<Plan> {
    let query: MarketQuery = req.market.parse()?;
    if !matches!(query.venue, None | Some(VenueId::Hyperliquid)) {
        return Err(usage("long, short and close trade Hyperliquid perps")
            .with_hint(json!({ "cause": "venue_not_tradable" })));
    }
    let hl = venues.hyperliquid_trade(req.testnet);
    let (rules, top, account) = tokio::try_join!(
        hl.rules(&query.base),
        hl.top(&query.base),
        hl.account(user, &query.base)
    )?;
    let coin = rules.coin.clone();

    // Closing follows the position: sell a long, buy back a short.
    let side = if req.close {
        if account.position.is_zero() {
            return Err(Error::new(
                ErrorCode::Usage,
                format!("there is no {coin} position to close"),
            )
            .with_hint(json!({ "cause": "no_position", "coin": coin })));
        }
        if account.position > Decimal::ZERO { OrderSide::Sell } else { OrderSide::Buy }
    } else {
        req.side
    };
    let buy = side == OrderSide::Buy;
    let reference = if buy { top.ask } else { top.bid };
    let (price, tif, slippage_pct) = bound(req, buy, reference, |p, b| rules.price(p, b));
    if price <= Decimal::ZERO {
        return Err(usage("the price works out to zero"));
    }

    let held = account.position.abs();
    let raw = match (&req.amount, req.close) {
        (Amount::Units { qty, asset }, _) if asset.eq_ignore_ascii_case(&coin) => *qty,
        (Amount::Units { qty, asset }, _) if asset == "USDC" || asset == "USD" => *qty / price,
        (Amount::Units { asset, .. }, _) => {
            return Err(usage(format!("{asset} is not {coin}: give a size in {coin} or dollars")));
        }
        (Amount::Usd { usd }, _) => *usd / price,
        (Amount::Percent { pct }, true) => held * *pct / Decimal::ONE_HUNDRED,
        (Amount::Max, true) => held,
        (Amount::Percent { .. } | Amount::Max, false) => {
            return Err(usage("open a perp with a size (0.1 ETH) or dollars ($100)"));
        }
    };
    let size = rules.size(if req.close { raw.min(held) } else { raw });
    if size.is_zero() || size * price < HL_MIN_NOTIONAL {
        return Err(min_order("hyperliquid", HL_MIN_NOTIONAL, "USDC"));
    }

    let leverage = if req.close {
        None
    } else {
        let lev = req.leverage.unwrap_or(Decimal::ONE);
        if lev < Decimal::ONE || lev.fract() != Decimal::ZERO {
            return Err(usage("leverage is a whole number from 1"));
        }
        if lev > Decimal::from(rules.max_leverage) {
            return Err(usage(format!("{coin} allows at most {}x", rules.max_leverage)));
        }
        let margin = size * price / lev;
        if margin > account.withdrawable {
            return Err(insufficient("USDC", up2(margin), account.withdrawable, "hyperliquid"));
        }
        Some(lev)
    };

    let id = new_id();
    let mut plan = draft(id.clone(), req.client_id.clone().unwrap_or_else(|| format!("onra-{id}")));
    plan.venue = VenueId::Hyperliquid;
    plan.testnet = req.testnet;
    plan.symbol = coin.clone();
    plan.base = coin;
    plan.quote = "USDC".into();
    plan.side = side;
    plan.time_in_force = tif_name(tif);
    plan.qty = size;
    plan.price = price;
    plan.reference_price = reference;
    plan.notional_usd = up2(size * price.max(reference));
    plan.slippage_pct = slippage_pct;
    plan.perp = true;
    plan.leverage = leverage;
    plan.reduce_only = req.close;
    plan.asset_index = Some(rules.asset);
    Ok(plan)
}

// ---------------------------------------------------------------------------
// carrying out

pub async fn execute(
    plan: &Plan,
    initiator: Initiator,
    venues: &Venues,
    signer: &Signer,
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
    let placed = match signer {
        Signer::Binance(key) => place_binance(plan, venues, key).await,
        Signer::Hyperliquid { key, .. } => place_hyperliquid(plan, venues, key).await,
    };
    match placed {
        Ok(exec) => {
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
                onra_policy::audit::FAILED,
                Some(initiator),
                json!({ "plan_id": plan.id, "error": e.message }),
            )?;
            Err(e)
        }
    }
}

fn execution(plan: &Plan) -> Execution {
    Execution {
        plan_id: plan.id.clone(),
        action: plan.action().into(),
        side: plan.side,
        base: plan.base.clone(),
        quote: plan.quote.clone(),
        order_id: String::new(),
        status: String::new(),
        executed_qty: Decimal::ZERO,
        quote_qty: Decimal::ZERO,
        avg_price: None,
        fee: Vec::new(),
        summary: plan.summary(),
        testnet: plan.testnet,
    }
}

async fn place_binance(plan: &Plan, venues: &Venues, key: &BinanceKey) -> Result<Execution> {
    let order = SpotOrder {
        symbol: plan.symbol.clone(),
        buy: plan.side == OrderSide::Buy,
        qty: plan.qty,
        price: plan.price,
        tif: if plan.time_in_force == "gtc" { TimeInForce::Gtc } else { TimeInForce::Ioc },
        client_id: plan.client_id.clone(),
    };
    let fill = venues.binance_spot(plan.testnet).place(key, &order).await?;
    let mut exec = execution(plan);
    exec.order_id = fill.order_id;
    exec.status = fill.status.to_ascii_lowercase();
    exec.executed_qty = fill.executed_qty;
    exec.quote_qty = fill.quote_qty;
    exec.avg_price = fill.avg_price;
    exec.fee = fill.fee;
    Ok(exec)
}

fn signature_json(sig: &onra_signer::EvmSignature) -> Value {
    json!({ "r": format!("0x{}", hex::encode(sig.r)), "s": format!("0x{}", hex::encode(sig.s)), "v": sig.v })
}

async fn place_hyperliquid(plan: &Plan, venues: &Venues, key: &EvmKey) -> Result<Execution> {
    let hl = venues.hyperliquid_trade(plan.testnet);
    let asset = plan
        .asset_index
        .ok_or_else(|| Error::new(ErrorCode::Internal, "perp plan without an asset index"))?;
    let mainnet = !plan.testnet;
    // Nonces must be unique per signer and close to now.
    let mut nonce = guard::now_ms();

    if let Some(lev) = plan.leverage {
        let lev: u32 = lev.try_into().unwrap_or(1);
        let action =
            json!({ "type": "updateLeverage", "asset": asset, "isCross": true, "leverage": lev });
        let sig = sign_l1_action(key, &action, nonce, None, mainnet)?;
        hl.exchange(&action, nonce, signature_json(&sig)).await?;
        nonce += 1;
    }

    let tif = if plan.time_in_force == "gtc" { "Gtc" } else { "Ioc" };
    let action = json!({
        "type": "order",
        "orders": [{
            "a": asset,
            "b": plan.side == OrderSide::Buy,
            "p": wire_decimal(plan.price),
            "s": wire_decimal(plan.qty),
            "r": plan.reduce_only,
            "t": { "limit": { "tif": tif } },
        }],
        "grouping": "na",
    });
    let sig = sign_l1_action(key, &action, nonce, None, mainnet)?;
    let response = hl.exchange(&action, nonce, signature_json(&sig)).await?;
    let fill = parse_order_status(&response)?;
    let mut exec = execution(plan);
    exec.order_id = fill.order_id;
    exec.status = fill.status;
    exec.executed_qty = fill.executed_qty;
    exec.avg_price = fill.avg_price;
    exec.quote_qty = fill.avg_price.map(|p| p * fill.executed_qty).unwrap_or_default();
    Ok(exec)
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
