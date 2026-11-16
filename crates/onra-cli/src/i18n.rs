//! Interface language. Everything a person reads follows the language picked
//! at startup (`ONRA_LANG`, then `ui.language`, then the system locale).
//! Commands, flags, JSON fields and error codes stay English: they are the
//! contract agents rely on.
//!
//! Adding a language means adding a [`Strings`] table and a [`Lang`] variant.

use std::sync::atomic::{AtomicU8, Ordering};

use onra_core::{Error, ErrorCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Zh,
}

impl Lang {
    /// From a locale or language tag: `zh-CN`, `zh_TW.UTF-8`, `en-US`...
    pub fn from_tag(tag: &str) -> Option<Self> {
        let tag = tag.to_ascii_lowercase();
        if tag.starts_with("zh") {
            Some(Lang::Zh)
        } else if tag.starts_with("en") {
            Some(Lang::En)
        } else {
            None
        }
    }

    pub fn system() -> Self {
        sys_locale::get_locale().and_then(|l| Self::from_tag(&l)).unwrap_or(Lang::En)
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set(lang: Lang) {
    CURRENT.store(lang as u8 + 1, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        0 => {
            let lang = Lang::system();
            set(lang);
            lang
        }
        2 => Lang::Zh,
        _ => Lang::En,
    }
}

/// The phrase table for the current language.
pub fn s() -> &'static Strings {
    strings(lang())
}

pub fn strings(lang: Lang) -> &'static Strings {
    match lang {
        Lang::Zh => &ZH,
        Lang::En => &EN,
    }
}

/// Fill `{name}` placeholders.
pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// An error as a person should read it. Codes with structured details in
/// their hint get a translated sentence; anything else keeps its message.
pub fn error_text(error: &Error) -> String {
    let s = s();
    let hint = |key: &str| {
        error.hint.as_ref().and_then(|h| h.get(key)).and_then(|v| v.as_str()).map(str::to_string)
    };
    let text = match error.code {
        ErrorCode::MarketNotFound => hint("query").map(|q| fill(s.err_not_found, &[("query", &q)])),
        ErrorCode::UnknownVenue => {
            hint("venue").map(|v| fill(s.err_unknown_venue, &[("venue", &v)]))
        }
        ErrorCode::RateLimited => hint("venue").map(|v| fill(s.err_rate_limited, &[("venue", &v)])),
        ErrorCode::Venue if hint("cause").as_deref() == Some("no_book") => {
            hint("venue").map(|v| fill(s.err_no_book, &[("venue", &v)]))
        }
        ErrorCode::Credentials => match hint("cause").as_deref() {
            Some("no_accounts") => Some(s.err_no_accounts.to_string()),
            Some("missing_key") => Some(s.err_missing_key.to_string()),
            Some("key_rejected") => Some(s.err_key_rejected.to_string()),
            Some("withdrawals") => Some(s.err_withdrawals.to_string()),
            Some("clock") => Some(s.err_clock.to_string()),
            Some("needs_human") => {
                Some(fill(s.err_needs_human, &[("cmd", &hint("suggest").unwrap_or_default())]))
            }
            Some("no_trading_keys") => Some(s.err_no_trading_keys.to_string()),
            Some("main_wallet") => Some(s.err_main_wallet.to_string()),
            Some("cannot_trade") => Some(s.err_cannot_trade.to_string()),
            Some("locked") => Some(s.err_locked.to_string()),
            Some("no_trading_key") => {
                Some(fill(s.err_no_trading_key, &[("cmd", &hint("suggest").unwrap_or_default())]))
            }
            _ => None,
        },
        ErrorCode::Locked => Some(s.err_locked.to_string()),
        ErrorCode::ApprovalRequired => {
            hint("plan_id").map(|id| fill(s.err_approval, &[("id", &id)]))
        }
        ErrorCode::PlanExpired => Some(s.err_plan_expired.to_string()),
        ErrorCode::Policy if hint("cause").as_deref() == Some("denied") => {
            let what = error.message.trim_start_matches("blocked by policy: ").to_string();
            Some(fill(s.err_denied, &[("what", &what)]))
        }
        ErrorCode::Venue if hint("cause").as_deref() == Some("min_order") => Some(fill(
            s.err_min_order,
            &[
                ("min", &hint("min_notional").unwrap_or_default()),
                ("quote", &hint("quote").unwrap_or_default()),
            ],
        )),
        ErrorCode::Venue if hint("cause").as_deref() == Some("insufficient_balance") => Some(fill(
            s.err_insufficient,
            &[
                ("asset", &hint("asset").unwrap_or_default()),
                ("need", &hint("need").unwrap_or_default()),
                ("have", &hint("have").unwrap_or_default()),
            ],
        )),
        ErrorCode::Usage if hint("cause").as_deref() == Some("amount_unit") => {
            Some(s.err_amount_unit.to_string())
        }
        ErrorCode::Policy if hint("cause").as_deref() == Some("tampered") => {
            Some(s.err_tampered.to_string())
        }
        ErrorCode::Policy if hint("cause").as_deref() == Some("audit_broken") => {
            let seq = error.hint.as_ref().and_then(|h| h.get("seq")).map(|v| v.to_string());
            Some(fill(s.err_audit_broken, &[("seq", seq.as_deref().unwrap_or("?"))]))
        }
        ErrorCode::Usage if hint("cause").as_deref() == Some("address") => {
            let address = hint("address").unwrap_or_default();
            Some(fill(s.err_bad_address, &[("address", &address)]))
        }
        ErrorCode::Network => hint("venue").map(|v| {
            let cause = match hint("cause").as_deref() {
                Some("timeout") => s.cause_timeout,
                Some("connect") => s.cause_connect,
                _ => s.cause_request,
            };
            fill(s.err_network, &[("venue", &v), ("cause", cause)])
        }),
        _ => None,
    };
    text.unwrap_or_else(|| error.message.clone())
}

pub struct Strings {
    // Shell
    pub ask: &'static str,
    pub menu: &'static [(&'static str, &'static str)],
    pub soon: &'static str,
    pub placeholder: &'static str,
    pub hints_menu: &'static str,
    pub hints_input: &'static str,
    pub hints_pick: &'static str,
    pub hints_busy: &'static str,
    pub hints_watch: &'static str,
    pub pick_asset: &'static str,
    pub pick_asset_hint: &'static str,
    pub pick_venue: &'static str,
    pub all_venues: &'static str,
    pub nl_soon: &'static str,
    pub nl_try: &'static str,
    pub cancelled: &'static str,
    pub bye: &'static str,
    pub help_title: &'static str,
    pub help: &'static [(&'static str, &'static str)],
    pub offline: &'static str,

    // price
    pub reaching_markets: &'static str,
    pub footer: &'static str,
    pub venue_one: &'static str,
    pub venue_many: &'static str,
    pub range_24h: &'static str,
    pub spread: &'static str,
    pub low: &'static str,
    pub high: &'static str,
    pub unverified: &'static str,

    // watch
    pub live: &'static str,
    pub stopped: &'static str,
    pub watch_status: &'static str,
    pub watch_stop: &'static str,

    // search
    pub search_hint: &'static str,
    pub liquidity: &'static str,
    pub holders: &'static str,
    pub per_contract: &'static str,

    // config
    pub default_note: &'static str,
    pub reset_default: &'static str,

    pub total: &'static str,
    pub hidden_tokens: &'static str,
    pub wallet_spot: &'static str,
    pub wallet_perps: &'static str,
    pub wallet_earn: &'static str,
    pub wallet_solana: &'static str,
    pub section_positions: &'static str,
    pub section_orders: &'static str,
    pub no_balances: &'static str,
    pub no_positions: &'static str,
    pub no_orders: &'static str,
    pub long: &'static str,
    pub short: &'static str,
    pub entry: &'static str,
    pub mark: &'static str,
    pub liq: &'static str,
    pub buy: &'static str,
    pub sell: &'static str,
    pub filled: &'static str,
    pub account_not_linked: &'static str,
    pub account_read_only: &'static str,
    pub account_how_add: &'static str,
    pub prompt_key: &'static str,
    pub prompt_secret: &'static str,
    pub prompt_passphrase: &'static str,
    pub checking_key: &'static str,
    pub linked_ok: &'static str,
    pub address_ok: &'static str,
    pub removed: &'static str,
    pub backend_keychain: &'static str,
    pub backend_file: &'static str,
    pub prompt_vault: &'static str,
    pub prompt_vault_new: &'static str,
    pub prompt_vault_repeat: &'static str,
    pub vault_new: &'static str,
    pub hl_agent_note: &'static str,
    pub prompt_hl_agent: &'static str,
    pub prompt_sol_keypair: &'static str,
    pub trade_key_added: &'static str,
    pub trade_key_reload: &'static str,
    pub trade_key_removed: &'static str,
    pub trade_key_absent: &'static str,
    pub trade_keys: &'static str,
    pub trade_keys_none: &'static str,
    pub signer: &'static str,
    pub signer_locked: &'static str,
    pub signer_unlocked: &'static str,
    pub unlocked_ok: &'static str,
    pub locked_ok: &'static str,
    pub already_locked: &'static str,
    pub err_needs_human: &'static str,
    pub err_no_trading_keys: &'static str,
    pub err_main_wallet: &'static str,
    pub err_cannot_trade: &'static str,
    pub policy_title: &'static str,
    pub policy_sealed: &'static str,
    pub policy_defaults: &'static str,
    pub pol_max_trade: &'static str,
    pub pol_max_daily: &'static str,
    pub pol_leverage: &'static str,
    pub pol_slippage: &'static str,
    pub pol_verified_on: &'static str,
    pub pol_verified_off: &'static str,
    pub pol_venues: &'static str,
    pub pol_approval_on: &'static str,
    pub pol_approval_off: &'static str,
    pub pol_rate: &'static str,
    pub pol_running: &'static str,
    pub pol_halted: &'static str,
    pub pol_change_hint: &'static str,
    pub halted_ok: &'static str,
    pub resumed_ok: &'static str,
    pub audit_empty: &'static str,
    pub audit_intact: &'static str,
    pub err_tampered: &'static str,
    pub err_locked: &'static str,
    pub err_audit_broken: &'static str,
    pub card_buy: &'static str,
    pub card_sell: &'static str,
    pub testnet: &'static str,
    pub for_at_most: &'static str,
    pub for_at_least: &'static str,
    pub lbl_price: &'static str,
    pub lbl_order: &'static str,
    pub lbl_simulated: &'static str,
    pub lbl_policy: &'static str,
    pub best_ask: &'static str,
    pub best_bid: &'static str,
    pub order_ioc: &'static str,
    pub order_gtc: &'static str,
    pub simulated_ok: &'static str,
    pub confirm_hint: &'static str,
    pub trade_cancelled: &'static str,
    pub plan_waiting: &'static str,
    pub plan_ready: &'static str,
    pub plans_empty: &'static str,
    pub rule_trade: &'static str,
    pub rule_daily: &'static str,
    pub rule_slippage: &'static str,
    pub rule_leverage: &'static str,
    pub rule_verified: &'static str,
    pub rule_venue: &'static str,
    pub rule_halt: &'static str,
    pub rule_rate: &'static str,
    pub bought: &'static str,
    pub sold: &'static str,
    pub filled_ok: &'static str,
    pub fee_paid: &'static str,
    pub order_id: &'static str,
    pub nothing_filled: &'static str,
    pub order_resting: &'static str,
    pub prompt_approve: &'static str,
    pub err_approval: &'static str,
    pub err_plan_expired: &'static str,
    pub err_denied: &'static str,
    pub err_min_order: &'static str,
    pub err_insufficient: &'static str,
    pub err_amount_unit: &'static str,
    pub err_no_trading_key: &'static str,

    // errors
    pub err_not_found: &'static str,
    pub err_unknown_venue: &'static str,
    pub err_rate_limited: &'static str,
    pub err_network: &'static str,
    pub err_no_accounts: &'static str,
    pub err_missing_key: &'static str,
    pub err_key_rejected: &'static str,
    pub err_withdrawals: &'static str,
    pub err_clock: &'static str,
    pub err_bad_address: &'static str,
    pub more_orders: &'static str,
    pub book_mid: &'static str,
    pub funding_title: &'static str,
    pub funding_per: &'static str,
    pub funding_diff: &'static str,
    pub funding_note: &'static str,
    pub err_no_book: &'static str,
    pub show_total: &'static str,
    pub hide_total: &'static str,
    pub cause_timeout: &'static str,
    pub cause_connect: &'static str,
    pub cause_request: &'static str,
}

pub const EN: Strings = Strings {
    ask: "What would you like to do?",
    menu: &[
        ("Prices", "One asset across every venue"),
        ("Watch", "Live prices"),
        ("Search", "Find markets, spot impostor tokens"),
        ("Portfolio", "Balances · positions · orders"),
        ("Trade", "Spot · perps"),
        ("Settings", "Proxy · language · colors"),
        ("Quit", ""),
    ],
    soon: "coming soon",
    placeholder: "Type a command, e.g. price SOL",
    hints_menu: "↑↓ select · enter confirm · type to enter a command · ctrl+c quit",
    hints_input: "enter run · tab complete · ↑↓ history · → accept suggestion · esc menu",
    hints_pick: "↑↓ select · enter confirm · esc back",
    hints_busy: "ctrl+c cancel",
    hints_watch: "esc / q stop watching",
    pick_asset: "Which asset?",
    pick_asset_hint: "Type a symbol like SOL, or pick one below",
    pick_venue: "Where?",
    all_venues: "All venues",
    nl_soon: "The natural-language assistant is on its way (M4).",
    nl_try: "You might want",
    cancelled: "Cancelled",
    bye: "See you. Markets never sleep; you should.",
    help_title: "Commands",
    help: &[
        ("price <asset>", "Cross-venue prices, e.g. price SOL ETH, price ETH-PERP@hl"),
        ("watch <asset>", "Live prices, e.g. watch SOL BTC"),
        ("search <text>", "Search every venue; verified and impostor tokens marked"),
        ("book <market>", "Order book with depth bars, e.g. book ETH"),
        ("funding [assets]", "Perp funding, annualized across venues"),
        ("portfolio", "Balances, positions and open orders together"),
        ("account", "Linked accounts: list, add, login, remove"),
        ("config show", "Show settings"),
        ("config set <key> <value>", "Change a setting, e.g. config set ui.language zh"),
        ("menu", "Back to the menu"),
        ("clear", "Clear the screen"),
        ("help", "Show this help"),
        ("exit", "Quit"),
    ],
    offline: "offline",

    reaching_markets: "reaching markets",
    footer: "{venues} · {ms}ms · {time} UTC",
    venue_one: "1 venue",
    venue_many: "{n} venues",
    range_24h: "24h",
    spread: "spread",
    low: "low",
    high: "high",
    unverified: "unverified",

    live: "LIVE",
    stopped: "STOPPED",
    watch_status: "{markets} quotes · every {interval}s · updated {ago}s ago · {running}",
    watch_stop: "q / esc to stop",

    search_hint: "price a Solana token by its ✓ symbol, or by mint address with @jup",
    liquidity: "liq",
    holders: "{n} holders",
    per_contract: "{n} {base} per contract",

    default_note: "(default)",
    reset_default: "reset to default",

    total: "Total",
    hidden_tokens: "{n} small or unverified tokens hidden · --all to show",
    wallet_spot: "spot",
    wallet_perps: "perps",
    wallet_earn: "earn",
    wallet_solana: "wallet",
    section_positions: "Positions",
    section_orders: "Open orders",
    no_balances: "No balances",
    no_positions: "No open positions",
    no_orders: "No open orders",
    long: "LONG",
    short: "SHORT",
    entry: "entry",
    mark: "mark",
    liq: "liq",
    buy: "buy",
    sell: "sell",
    filled: "{pct} filled",
    account_not_linked: "not linked",
    account_read_only: "read-only",
    account_how_add: "link with: {cmd}",
    prompt_key: "Binance API key: ",
    prompt_secret: "Secret (hidden): ",
    prompt_passphrase: "Keystore passphrase: ",
    checking_key: "checking what this key can do…",
    linked_ok: "{venue} linked · {mode} · secret stored in the {backend}",
    address_ok: "{venue} linked · {address}",
    removed: "{venue} unlinked",
    backend_keychain: "system keychain",
    backend_file: "encrypted file",
    prompt_vault: "Trading vault passphrase: ",
    prompt_vault_new: "New vault passphrase (hidden): ",
    prompt_vault_repeat: "Repeat it: ",
    vault_new: "Creating the trading vault. Its passphrase unlocks every trading key; it cannot be recovered.",
    hl_agent_note: "Use an API wallet (Hyperliquid → More → API): it can trade but cannot withdraw.",
    prompt_hl_agent: "API wallet private key (hidden): ",
    prompt_sol_keypair: "Keypair, base58 or JSON array (hidden): ",
    trade_key_added: "trading key {key} sealed in the vault · {public}",
    trade_key_reload: "the signer is unlocked; run `onra unlock` again to load the new key",
    trade_key_removed: "trading key {key} deleted",
    trade_key_absent: "no trading key {key}",
    trade_keys: "trading keys",
    trade_keys_none: "none · add with: onra account trade <venue>",
    signer: "signer",
    signer_locked: "locked",
    signer_unlocked: "unlocked · locks in {ttl}",
    unlocked_ok: "signer unlocked · locks itself in {ttl}",
    locked_ok: "signer locked · keys wiped from memory",
    already_locked: "signer was already locked",
    err_needs_human: "this needs a person at the keyboard; ask them to run `{cmd}`",
    err_no_trading_keys: "no trading keys yet; add one with `onra account trade <venue>`",
    err_main_wallet: "that is your main wallet's key, which can withdraw. Create an API wallet on Hyperliquid and use its key",
    err_cannot_trade: "this key cannot trade; enable spot trading for it on Binance",
    policy_title: "Trading policy",
    policy_sealed: "custom · sealed",
    policy_defaults: "defaults",
    pol_max_trade: "per trade",
    pol_max_daily: "per UTC day · {used} used today",
    pol_leverage: "perps",
    pol_slippage: "swaps and market orders",
    pol_verified_on: "verified assets only",
    pol_verified_off: "any asset (lookalikes allowed)",
    pol_venues: "where trading is allowed",
    pol_approval_on: "agent trades wait for your approval",
    pol_approval_off: "agents trade within limits on their own",
    pol_rate: "orders per minute",
    pol_running: "trading is on",
    pol_halted: "halted: agents cannot trade · resume with {cmd}",
    pol_change_hint: "change with: onra policy set <key> <value>",
    halted_ok: "all agent trading halted · resume with {cmd}",
    resumed_ok: "trading resumed",
    audit_empty: "nothing recorded yet",
    audit_intact: "{n} entries · chain intact",
    err_tampered: "the policy file was edited by hand, so trading is stopped; run `onra policy reset`",
    err_locked: "the signer is locked; a person needs to run `onra unlock`",
    err_audit_broken: "the audit log was altered at entry {seq}; trading is stopped",
    card_buy: "BUY",
    card_sell: "SELL",
    testnet: "testnet",
    for_at_most: "for at most {amount}",
    for_at_least: "for at least {amount}",
    lbl_price: "Price",
    lbl_order: "Order",
    lbl_simulated: "Simulated",
    lbl_policy: "Policy",
    best_ask: "best ask {p} · {slip}% bound",
    best_bid: "best bid {p} · {slip}% bound",
    order_ioc: "limit IOC · fills now, the rest cancels",
    order_gtc: "limit GTC · rests on the book",
    simulated_ok: "binance accepted a dry run",
    confirm_hint: "enter send · esc cancel",
    trade_cancelled: "cancelled · nothing was sent",
    plan_waiting: "waits for your approval",
    plan_ready: "ready",
    plans_empty: "no plans waiting",
    rule_trade: "per trade",
    rule_daily: "today",
    rule_slippage: "slippage",
    rule_leverage: "leverage",
    rule_verified: "asset",
    rule_venue: "venue",
    rule_halt: "halt",
    rule_rate: "orders/min",
    bought: "bought",
    sold: "sold",
    filled_ok: "{side} {qty} · avg {avg} · {total}",
    fee_paid: "fee {fee}",
    order_id: "order {id}",
    nothing_filled: "nothing filled: the price moved past the bound, and no order is left resting",
    order_resting: "order placed; it rests on the book until filled or cancelled",
    prompt_approve: "Passphrase to approve: ",
    err_approval: "plan {id} waits for a person: run `onra approve {id}`",
    err_plan_expired: "that plan expired; make a new one",
    err_denied: "blocked by policy: {what}",
    err_min_order: "below the venue's minimum order ({min} {quote})",
    err_insufficient: "not enough {asset}: {need} needed, {have} available",
    err_amount_unit: "say what the amount is in, e.g. `$100` or `100 USDT`",
    err_no_trading_key: "no Binance trading key for this network; add one with `{cmd}`",

    err_not_found: "no venue lists `{query}`",
    err_unknown_venue: "unknown venue `{venue}`",
    err_rate_limited: "{venue} is rate limiting requests; try again shortly",
    err_network: "could not reach {venue} ({cause})",
    err_no_accounts: "no accounts are linked yet",
    err_missing_key: "the binance key is missing from the keystore; link it again",
    err_key_rejected: "binance rejected the API key (invalid, expired, or IP not allowed)",
    err_withdrawals: "this key can withdraw funds. Onra refuses such keys: create one with withdrawals disabled",
    err_clock: "binance rejected the request time; check the system clock",
    err_bad_address: "`{address}` is not a valid address for this venue",
    more_orders: "+{n} more · --json lists all",
    book_mid: "mid",
    funding_title: "Funding · annualized",
    funding_per: "per {h}h",
    funding_diff: "diff",
    funding_note: "positive: longs pay shorts · negative: shorts pay longs",
    err_no_book: "{venue} has no order book: it routes swaps through AMM pools",
    show_total: "ctrl+o show",
    hide_total: "ctrl+o hide",
    cause_timeout: "timed out",
    cause_connect: "connection failed",
    cause_request: "request failed",
};

pub const ZH: Strings = Strings {
    ask: "想做什么？",
    menu: &[
        ("查行情", "看一个币在各家市场的价格"),
        ("盯盘", "实时刷新价格"),
        ("搜索", "找币种和市场，识别仿冒币"),
        ("我的资产", "余额 · 持仓 · 挂单"),
        ("交易", "现货 · 合约"),
        ("设置", "代理 · 语言 · 涨跌颜色"),
        ("退出", ""),
    ],
    soon: "即将推出",
    placeholder: "输入命令，比如 price SOL",
    hints_menu: "↑↓ 选择 · enter 确认 · 直接打字输入命令 · ctrl+c 退出",
    hints_input: "enter 执行 · tab 补全 · ↑↓ 历史 · → 接受建议 · esc 返回菜单",
    hints_pick: "↑↓ 选择 · enter 确认 · esc 返回",
    hints_busy: "ctrl+c 取消",
    hints_watch: "esc / q 停止盯盘",
    pick_asset: "哪个币？",
    pick_asset_hint: "输入代码，比如 SOL；或者从下面选",
    pick_venue: "在哪看？",
    all_venues: "全部市场",
    nl_soon: "自然语言助手还在路上（M4）。",
    nl_try: "你可能想要",
    cancelled: "已取消",
    bye: "下次见。市场不睡觉，你要睡。",
    help_title: "命令",
    help: &[
        ("price <币>", "跨市场价格，比如 price SOL ETH、price ETH-PERP@hl"),
        ("watch <币>", "实时盯盘，比如 watch SOL BTC"),
        ("search <关键词>", "在各家市场搜索，标出已验证和仿冒的代币"),
        ("book <市场>", "订单簿和深度条，比如 book ETH"),
        ("funding [币]", "合约资金费率，跨市场年化对比"),
        ("portfolio", "余额、持仓、挂单一起看"),
        ("account", "管理绑定的账户：list、add、login、remove"),
        ("config show", "查看设置"),
        ("config set <键> <值>", "修改设置，比如 config set ui.language en"),
        ("menu", "回到菜单"),
        ("clear", "清屏"),
        ("help", "显示这份帮助"),
        ("exit", "退出"),
    ],
    offline: "连不上",

    reaching_markets: "正在连接市场",
    footer: "{venues} · {ms}ms · {time} UTC",
    venue_one: "1 个市场",
    venue_many: "{n} 个市场",
    range_24h: "24h",
    spread: "价差",
    low: "最低",
    high: "最高",
    unverified: "未验证",

    live: "实时",
    stopped: "已停止",
    watch_status: "{markets} 个报价 · 每 {interval} 秒刷新 · {ago} 秒前更新 · {running}",
    watch_stop: "q / esc 停止",

    search_hint: "查 Solana 代币的价格：用带 ✓ 的代码，或者合约地址加 @jup",
    liquidity: "流动性",
    holders: "{n} 持有人",
    per_contract: "每份合约 {n} 个 {base}",

    default_note: "（默认）",
    reset_default: "已恢复默认",

    total: "总资产",
    hidden_tokens: "已隐藏 {n} 个小额或未验证的代币 · 加 --all 显示",
    wallet_spot: "现货",
    wallet_perps: "合约",
    wallet_earn: "理财",
    wallet_solana: "钱包",
    section_positions: "持仓",
    section_orders: "挂单",
    no_balances: "没有余额",
    no_positions: "没有持仓",
    no_orders: "没有挂单",
    long: "多",
    short: "空",
    entry: "开仓",
    mark: "标记",
    liq: "强平",
    buy: "买",
    sell: "卖",
    filled: "已成交 {pct}",
    account_not_linked: "未绑定",
    account_read_only: "只读",
    account_how_add: "绑定：{cmd}",
    prompt_key: "币安 API key：",
    prompt_secret: "Secret（输入时不显示）：",
    prompt_passphrase: "密钥库口令：",
    checking_key: "正在检查这个 key 的权限…",
    linked_ok: "已绑定 {venue} · {mode} · secret 保存在{backend}",
    address_ok: "已绑定 {venue} · {address}",
    removed: "已解绑 {venue}",
    backend_keychain: "系统密钥链",
    backend_file: "加密文件",
    prompt_vault: "交易密钥库口令：",
    prompt_vault_new: "设置密钥库口令（输入时不显示）：",
    prompt_vault_repeat: "再输一次：",
    vault_new: "正在创建交易密钥库。这个口令能解锁所有交易密钥，忘记后无法找回。",
    hl_agent_note: "请使用 API 钱包（Hyperliquid → More → API）：它能交易，但不能提现。",
    prompt_hl_agent: "API 钱包私钥（输入时不显示）：",
    prompt_sol_keypair: "密钥对，base58 或 JSON 数组（输入时不显示）：",
    trade_key_added: "交易密钥 {key} 已加密存入密钥库 · {public}",
    trade_key_reload: "签名器正处于解锁状态，重新运行 `onra unlock` 以载入新密钥",
    trade_key_removed: "已删除交易密钥 {key}",
    trade_key_absent: "没有交易密钥 {key}",
    trade_keys: "交易密钥",
    trade_keys_none: "暂无 · 添加：onra account trade <venue>",
    signer: "签名器",
    signer_locked: "已锁定",
    signer_unlocked: "已解锁 · {ttl} 后上锁",
    unlocked_ok: "签名器已解锁 · {ttl} 后自动上锁",
    locked_ok: "签名器已上锁 · 密钥已从内存清除",
    already_locked: "签名器本来就是锁定状态",
    err_needs_human: "这一步需要本人在键盘前操作，请让用户自己运行 `{cmd}`",
    err_no_trading_keys: "还没有交易密钥，先用 `onra account trade <venue>` 添加",
    err_main_wallet: "这是主钱包的私钥，它可以提现。请在 Hyperliquid 创建 API 钱包，使用它的私钥",
    err_cannot_trade: "这个 key 没有交易权限，请在币安为它开启现货交易",
    policy_title: "交易风控",
    policy_sealed: "自定义 · 已封印",
    policy_defaults: "默认",
    pol_max_trade: "单笔",
    pol_max_daily: "每个 UTC 自然日 · 今天已用 {used}",
    pol_leverage: "合约",
    pol_slippage: "兑换和市价单",
    pol_verified_on: "只允许已验证资产",
    pol_verified_off: "任何资产（含仿冒币）",
    pol_venues: "允许交易的市场",
    pol_approval_on: "Agent 发起的交易需要你批准",
    pol_approval_off: "Agent 可在限额内自行交易",
    pol_rate: "每分钟下单数",
    pol_running: "交易已开启",
    pol_halted: "已急停：Agent 无法交易 · 恢复：{cmd}",
    pol_change_hint: "修改：onra policy set <key> <value>",
    halted_ok: "已急停所有 Agent 交易 · 恢复：{cmd}",
    resumed_ok: "交易已恢复",
    audit_empty: "还没有任何记录",
    audit_intact: "共 {n} 条 · 哈希链完整",
    err_tampered: "风控文件被手动改过，交易已停止；请运行 `onra policy reset`",
    err_locked: "签名器已锁定，需要本人运行 `onra unlock`",
    err_audit_broken: "审计日志第 {seq} 条被篡改，交易已停止",
    card_buy: "买入",
    card_sell: "卖出",
    testnet: "测试网",
    for_at_most: "最多花费 {amount}",
    for_at_least: "至少收到 {amount}",
    lbl_price: "价格",
    lbl_order: "订单",
    lbl_simulated: "模拟",
    lbl_policy: "风控",
    best_ask: "卖一 {p} · 滑点上限 {slip}%",
    best_bid: "买一 {p} · 滑点上限 {slip}%",
    order_ioc: "限价 IOC · 立即成交，剩余取消",
    order_gtc: "限价 GTC · 挂单等待成交",
    simulated_ok: "币安模拟下单通过",
    confirm_hint: "回车发送 · esc 取消",
    trade_cancelled: "已取消 · 没有发送任何订单",
    plan_waiting: "等待你批准",
    plan_ready: "可执行",
    plans_empty: "没有待处理的方案",
    rule_trade: "单笔",
    rule_daily: "今日累计",
    rule_slippage: "滑点",
    rule_leverage: "杠杆",
    rule_verified: "资产",
    rule_venue: "市场",
    rule_halt: "急停",
    rule_rate: "每分钟单数",
    bought: "买入",
    sold: "卖出",
    filled_ok: "已{side} {qty} · 均价 {avg} · 合计 {total}",
    fee_paid: "手续费 {fee}",
    order_id: "订单号 {id}",
    nothing_filled: "未成交：价格已越过滑点上限，没有留下挂单",
    order_resting: "订单已挂出，成交或撤单前会一直挂在盘口",
    prompt_approve: "输入口令以批准：",
    err_approval: "方案 {id} 需要本人批准：运行 `onra approve {id}`",
    err_plan_expired: "方案已过期，请重新生成",
    err_denied: "被风控拦截：{what}",
    err_min_order: "低于交易所最小下单额（{min} {quote}）",
    err_insufficient: "{asset} 余额不足：需要 {need}，可用 {have}",
    err_amount_unit: "请写明数量单位，例如 `$100` 或 `100 USDT`",
    err_no_trading_key: "这个网络还没有币安交易密钥，请用 `{cmd}` 添加",

    err_not_found: "没有市场上架 `{query}`",
    err_unknown_venue: "不认识的市场 `{venue}`",
    err_rate_limited: "{venue} 正在限流，稍后再试",
    err_network: "连不上 {venue}（{cause}）",
    err_no_accounts: "还没有绑定任何账户",
    err_missing_key: "密钥库里找不到币安的 key，请重新绑定",
    err_key_rejected: "币安拒绝了这个 API key（无效、已过期，或 IP 不在白名单）",
    err_withdrawals: "这个 key 开了提现权限。Onra 拒绝这类 key：请创建一个不带提现权限的 key",
    err_clock: "币安拒绝了请求时间，请检查系统时钟",
    err_bad_address: "`{address}` 不是这个市场有效的地址",
    more_orders: "还有 {n} 条 · 加 --json 查看全部",
    book_mid: "中间价",
    funding_title: "资金费率 · 年化",
    funding_per: "每 {h} 小时",
    funding_diff: "差值",
    funding_note: "正数：多头付给空头 · 负数：空头付给多头",
    err_no_book: "{venue} 没有订单簿：它通过 AMM 池子撮合兑换",
    show_total: "ctrl+o 显示",
    hide_total: "ctrl+o 隐藏",
    cause_timeout: "超时",
    cause_connect: "连接失败",
    cause_request: "请求失败",
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_language_tags() {
        assert_eq!(Lang::from_tag("zh-CN"), Some(Lang::Zh));
        assert_eq!(Lang::from_tag("zh_TW.UTF-8"), Some(Lang::Zh));
        assert_eq!(Lang::from_tag("en-US"), Some(Lang::En));
        assert_eq!(Lang::from_tag("fr-FR"), None);
    }

    #[test]
    fn tables_line_up() {
        assert_eq!(EN.menu.len(), ZH.menu.len());
        assert_eq!(EN.help.len(), ZH.help.len());
    }

    #[test]
    fn localizes_structured_errors() {
        set(Lang::Zh);
        let e = Error::new(ErrorCode::Network, "could not reach binance (timed out)")
            .with_hint(json!({ "venue": "binance", "cause": "timeout" }));
        assert_eq!(error_text(&e), "连不上 binance（超时）");
        let plain = Error::new(ErrorCode::Usage, "bad flag");
        assert_eq!(error_text(&plain), "bad flag");
        set(Lang::En);
    }
}
