//! Inside the signer: turning a trade request into a plan, and a plan into
//! an order. Everything that decides how much money moves happens here, from
//! prices the signer fetches itself: a caller can ask, never dictate.

use std::sync::atomic::{AtomicU64, Ordering};

use onra_core::{Error, ErrorCode, MarketKind, MarketQuery, OrderSide, Result, VenueId};
use onra_policy::{Decision, Initiator, Intent, Verdict, evaluate};
use onra_signer::hyperliquid::{sign_l1_action, wire_decimal};
use onra_signer::{EvmKey, SolanaKey};
use onra_venues::{BinanceKey, SpotOrder, TimeInForce, Venues, parse_order_status};
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{Value, json};

use super::guard;
use crate::trade::{Amount, DEFAULT_SLIPPAGE_PCT, Execution, Plan, SwapPlan, TradeRequest, units};

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
    /// The wallet that swaps on Jupiter.
    Solana(SolanaKey),
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
    if request.swap_to.is_some() {
        VenueId::Jupiter
    } else if request.perp {
        VenueId::Hyperliquid
    } else {
        VenueId::Binance
    }
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
        // Exchange listings are vetted; on Solana both tokens must be verified.
        verified: plan.swap.as_ref().is_none_or(|s| s.from.verified && s.to.verified),
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
        Signer::Solana(key) => plan_jupiter(req, venues, key).await?,
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
        swap: None,
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
        Signer::Solana(key) => place_jupiter(plan, venues, key).await,
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
                    "summary": format!(
                        "{} {} {} {}{} · order {}",
                        exec.status.to_ascii_uppercase(),
                        exec.executed_qty.normalize(),
                        exec.base,
                        exec.avg_price.map(|p| format!("@ {} ", p.normalize())).unwrap_or_default(),
                        exec.quote,
                        exec.order_id
                    ),
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

// ---------------------------------------------------------------------------
// Jupiter swaps

/// SOL a swap may spend beyond its input: network and priority fees, and
/// rent for a new token account. 0.01 SOL.
const SOL_ALLOWANCE: u64 = 10_000_000;

/// What the wallet holds of `mint`; SOL keeps the allowance back for fees.
fn holding(accounts: &[onra_venues::WalletAccount], mint: &str) -> u64 {
    let total: u64 = accounts.iter().filter(|a| a.mint == mint).map(|a| a.raw).sum();
    if mint == onra_venues::SOL_MINT { total.saturating_sub(2 * SOL_ALLOWANCE) } else { total }
}

async fn plan_jupiter(req: &TradeRequest, venues: &Venues, key: &SolanaKey) -> Result<Plan> {
    let to = req.swap_to.as_deref().ok_or_else(|| usage("a swap needs a token to swap into"))?;
    if to.eq_ignore_ascii_case(&req.market) {
        return Err(usage("swapping a token for itself does nothing")
            .with_hint(json!({ "cause": "same_token" })));
    }
    let jup = venues.jupiter_swap();
    let (from, to) = tokio::try_join!(jup.resolve(&req.market), jup.resolve(to))?;
    if from.mint == to.mint {
        return Err(usage("swapping a token for itself does nothing")
            .with_hint(json!({ "cause": "same_token" })));
    }
    let address = key.address();
    let accounts = venues.solana().accounts(&address).await?;
    let have = holding(&accounts, &from.mint);
    let scale = Decimal::from(10u64.pow(from.decimals));
    let price = from.usd_price.filter(|p| !p.is_zero());
    let wanted = match &req.amount {
        Amount::Units { qty, asset } if asset.eq_ignore_ascii_case(&from.symbol) => *qty,
        Amount::Units { asset, .. } => {
            return Err(usage(format!("swap amounts are in {}, not {asset}", from.symbol)));
        }
        Amount::Usd { usd } => {
            let p = price.ok_or_else(|| usage(format!("no dollar price for {}", from.symbol)))?;
            *usd / p
        }
        Amount::Percent { pct } => units(have, from.decimals) * *pct / Decimal::ONE_HUNDRED,
        Amount::Max => units(have, from.decimals),
    };
    let in_raw: u64 =
        (wanted * scale).trunc().try_into().map_err(|_| usage("amount out of range"))?;
    if in_raw == 0 {
        return Err(usage("the amount works out to zero"));
    }
    if in_raw > have {
        return Err(insufficient(
            &from.symbol,
            units(in_raw, from.decimals),
            units(have, from.decimals),
            "jupiter",
        ));
    }

    let slip = req.slippage_pct.unwrap_or(DEFAULT_SLIPPAGE_PCT);
    let bps: u32 = (slip * Decimal::ONE_HUNDRED).trunc().try_into().unwrap_or(50);
    let quote = jup.quote(&from.mint, &to.mint, in_raw, bps).await?;
    let tx = jup.transaction(&quote, &address).await?;
    let swap = SwapPlan {
        in_raw,
        out_raw: quote.out_amount,
        min_out_raw: quote.min_out,
        slippage_bps: bps,
        price_impact_pct: quote.price_impact_pct,
        route: quote.route.clone(),
        from: from.clone(),
        to: to.clone(),
    };
    check_effects(&swap, &accounts, venues, &tx).await?;

    let in_ui = units(in_raw, from.decimals);
    let notional = match (price, to.usd_price) {
        (Some(p), _) => in_ui * p,
        (None, Some(p)) => units(quote.out_amount, to.decimals) * p,
        (None, None) => {
            return Err(Error::new(
                ErrorCode::Policy,
                "neither token has a dollar price, so the limits cannot be applied",
            )
            .with_hint(json!({ "cause": "no_price" })));
        }
    };
    let id = new_id();
    let mut plan = draft(id.clone(), req.client_id.clone().unwrap_or_else(|| format!("onra-{id}")));
    plan.venue = VenueId::Jupiter;
    plan.symbol = format!("{}>{}", from.symbol, to.symbol);
    plan.base = to.symbol.clone();
    plan.quote = from.symbol.clone();
    plan.side = OrderSide::Buy;
    plan.qty = in_ui;
    plan.price = units(quote.min_out, to.decimals);
    plan.reference_price = units(quote.out_amount, to.decimals);
    plan.notional_usd = up2(notional);
    plan.slippage_pct = Some(slip);
    plan.simulated = true;
    plan.swap = Some(swap);
    Ok(plan)
}

/// Simulates the transaction and compares the wallet before and after. The
/// signer only signs if the input goes down by at most the planned amount,
/// nothing else goes down, and SOL pays no more than fees. A transaction
/// built by a third party is checked, never trusted.
async fn check_effects(
    swap: &SwapPlan,
    before: &[onra_venues::WalletAccount],
    venues: &Venues,
    tx: &[u8],
) -> Result<()> {
    let sim = venues.solana().simulate(tx, before).await?;
    judge_effects(swap, before, &sim)
}

/// The decision half of [`check_effects`], given the simulation.
fn judge_effects(
    swap: &SwapPlan,
    before: &[onra_venues::WalletAccount],
    sim: &onra_venues::Simulation,
) -> Result<()> {
    if let Some(err) = &sim.err {
        let hint =
            sim.logs.iter().rev().find(|l| l.contains("Error") || l.contains("insufficient"));
        return Err(Error::new(
            ErrorCode::Venue,
            format!(
                "the swap would fail: {err}{}",
                hint.map(|h| format!(" ({h})")).unwrap_or_default()
            ),
        )
        .with_hint(json!({ "venue": "jupiter", "cause": "simulation_failed" })));
    }
    let unexpected = |what: String| {
        Error::new(
            ErrorCode::Policy,
            format!("the transaction would do more than the plan says ({what}); not signed"),
        )
        .with_hint(json!({ "cause": "unexpected_effects" }))
    };
    for (acc, after) in before.iter().zip(&sim.after) {
        let after = after.unwrap_or(0);
        let spent = acc.raw.saturating_sub(after);
        let mut allowed = if acc.mint == swap.from.mint { swap.in_raw } else { 0 };
        if acc.native {
            allowed += SOL_ALLOWANCE;
        }
        if spent > allowed {
            return Err(unexpected(format!("{} would lose {spent}", acc.address)));
        }
    }
    // Where the output lands in an account we already have, it must arrive.
    let received: u64 = before
        .iter()
        .zip(&sim.after)
        .filter(|(a, _)| a.mint == swap.to.mint && !a.native)
        .map(|(a, after)| after.unwrap_or(0).saturating_sub(a.raw))
        .sum();
    let has_out_account = before.iter().any(|a| a.mint == swap.to.mint && !a.native);
    if has_out_account && swap.to.mint != onra_venues::SOL_MINT && received < swap.min_out_raw {
        return Err(unexpected(format!(
            "only {received} of at least {} would arrive",
            swap.min_out_raw
        )));
    }
    Ok(())
}

async fn place_jupiter(plan: &Plan, venues: &Venues, key: &SolanaKey) -> Result<Execution> {
    let planned = plan
        .swap
        .as_ref()
        .ok_or_else(|| Error::new(ErrorCode::Internal, "swap plan without a route"))?;
    let jup = venues.jupiter_swap();
    let address = key.address();
    let before = venues.solana().accounts(&address).await?;

    // Quotes move; re-quote, and tighten the slippage if needed so the
    // plan's minimum still holds. If it can't, the plan is void.
    let mut quote = jup
        .quote(&planned.from.mint, &planned.to.mint, planned.in_raw, planned.slippage_bps)
        .await?;
    if quote.min_out < planned.min_out_raw {
        if quote.out_amount <= planned.min_out_raw {
            return Err(Error::new(
                ErrorCode::PlanExpired,
                "the price moved past the plan's minimum; make a new plan",
            )
            .with_hint(json!({ "cause": "price_moved", "plan_id": plan.id })));
        }
        let room = (quote.out_amount - planned.min_out_raw) * 10_000 / quote.out_amount;
        quote =
            jup.quote(&planned.from.mint, &planned.to.mint, planned.in_raw, room as u32).await?;
        if quote.min_out < planned.min_out_raw {
            return Err(Error::new(
                ErrorCode::PlanExpired,
                "the price moved past the plan's minimum; make a new plan",
            )
            .with_hint(json!({ "cause": "price_moved", "plan_id": plan.id })));
        }
    }
    let tx = jup.transaction(&quote, &address).await?;
    let mut checked = planned.clone();
    checked.min_out_raw = quote.min_out;
    check_effects(&checked, &before, venues, &tx).await?;

    let signed = key.sign_transaction(&tx)?;
    let signature = venues.solana().send(&signed).await?;
    let confirmed = venues.solana().confirm(&signature, std::time::Duration::from_secs(45)).await?;

    let mut exec = execution(plan);
    exec.order_id = signature;
    exec.quote_qty = units(planned.in_raw, planned.from.decimals);
    if confirmed {
        exec.status = "confirmed".into();
        let after = venues.solana().accounts(&address).await.unwrap_or_default();
        let sum = |accs: &[onra_venues::WalletAccount]| -> u64 {
            accs.iter().filter(|a| a.mint == planned.to.mint).map(|a| a.raw).sum()
        };
        let got = sum(&after).saturating_sub(sum(&before));
        exec.executed_qty =
            units(if got > 0 { got } else { quote.out_amount }, planned.to.decimals);
    } else {
        // Sent but not yet seen: report what was sent, not a guess.
        exec.status = "sent".into();
    }
    if !exec.executed_qty.is_zero() {
        exec.avg_price = Some((exec.quote_qty / exec.executed_qty).round_dp(8).normalize());
    }
    Ok(exec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use onra_venues::{SOL_MINT, Simulation, SwapToken, WalletAccount};

    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    const JUP: &str = "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN";

    fn token(mint: &str, decimals: u32) -> SwapToken {
        SwapToken {
            mint: mint.into(),
            symbol: "X".into(),
            decimals,
            verified: true,
            usd_price: None,
        }
    }

    fn acc(address: &str, mint: &str, raw: u64, native: bool) -> WalletAccount {
        WalletAccount { address: address.into(), mint: mint.into(), raw, native }
    }

    /// 10 USDC for at least 0.06 SOL, from a wallet with SOL, USDC and JUP.
    fn setup() -> (SwapPlan, Vec<WalletAccount>) {
        let swap = SwapPlan {
            from: token(USDC, 6),
            to: token(SOL_MINT, 9),
            in_raw: 10_000_000,
            out_raw: 61_000_000,
            min_out_raw: 60_000_000,
            slippage_bps: 50,
            price_impact_pct: Decimal::ZERO,
            route: vec![],
        };
        let wallet = vec![
            acc("me", SOL_MINT, 1_000_000_000, true),
            acc("usdc", USDC, 50_000_000, false),
            acc("jup", JUP, 7_000_000, false),
        ];
        (swap, wallet)
    }

    fn sim(after: [u64; 3]) -> Simulation {
        Simulation { err: None, logs: vec![], after: after.into_iter().map(Some).collect() }
    }

    #[test]
    fn accepts_the_planned_swap() {
        let (swap, wallet) = setup();
        // USDC down by 10, SOL up by the proceeds minus fees, JUP untouched.
        let ok = sim([1_000_000_000 + 60_500_000 - 9_000, 40_000_000, 7_000_000]);
        assert!(judge_effects(&swap, &wallet, &ok).is_ok());
    }

    #[test]
    fn refuses_anything_more() {
        let (swap, wallet) = setup();
        let code = |after: [u64; 3]| judge_effects(&swap, &wallet, &sim(after)).unwrap_err().code;
        // Takes 20 USDC instead of 10.
        assert_eq!(code([1_060_000_000, 30_000_000, 7_000_000]), ErrorCode::Policy);
        // Quietly drains the JUP account.
        assert_eq!(code([1_060_000_000, 40_000_000, 0]), ErrorCode::Policy);
        // Spends 0.5 SOL "in fees".
        assert_eq!(code([500_000_000, 40_000_000, 7_000_000]), ErrorCode::Policy);
        // A failing transaction is never signed either.
        let failing =
            Simulation { err: Some(json!("InstructionError")), logs: vec![], after: vec![] };
        assert_eq!(judge_effects(&swap, &wallet, &failing).unwrap_err().code, ErrorCode::Venue);
    }

    #[test]
    fn insists_the_output_arrives() {
        let (mut swap, mut wallet) = setup();
        // Swap SOL for USDC instead, into the existing USDC account.
        swap.from = token(SOL_MINT, 9);
        swap.to = token(USDC, 6);
        swap.in_raw = 100_000_000;
        swap.min_out_raw = 15_000_000;
        wallet[0].raw = 1_000_000_000;
        let short = sim([900_000_000 - 5_000, 50_000_000 + 1_000_000, 7_000_000]);
        assert_eq!(judge_effects(&swap, &wallet, &short).unwrap_err().code, ErrorCode::Policy);
        let fine = sim([900_000_000 - 5_000, 50_000_000 + 16_000_000, 7_000_000]);
        assert!(judge_effects(&swap, &wallet, &fine).is_ok());
    }

    #[test]
    fn plan_ids_are_short_and_distinct() {
        let a = new_id();
        let b = new_id();
        assert_eq!(a.len(), 9);
        assert!(a.starts_with('p'));
        assert_ne!(a, b);
    }
}
