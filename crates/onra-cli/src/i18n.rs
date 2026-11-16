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
        ErrorCode::Credentials => match hint("cause").as_deref() {
            Some("no_accounts") => Some(s.err_no_accounts.to_string()),
            Some("missing_key") => Some(s.err_missing_key.to_string()),
            Some("key_rejected") => Some(s.err_key_rejected.to_string()),
            Some("withdrawals") => Some(s.err_withdrawals.to_string()),
            Some("clock") => Some(s.err_clock.to_string()),
            _ => None,
        },
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
