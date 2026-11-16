//! Interface language for the interactive shell. Commands, flags and JSON are
//! always English; menus, prompts and hints follow the system locale.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    pub fn detect() -> Self {
        let locale = sys_locale::get_locale().unwrap_or_default();
        if locale.starts_with("zh") { Lang::Zh } else { Lang::En }
    }
}

pub struct Strings {
    pub ask: &'static str,
    pub menu: [(&'static str, &'static str); 7],
    pub soon: &'static str,
    pub placeholder: &'static str,
    pub hints_menu: &'static str,
    pub hints_input: &'static str,
    pub hints_pick: &'static str,
    pub hints_busy: &'static str,
    pub pick_asset: &'static str,
    pub pick_asset_hint: &'static str,
    pub pick_venue: &'static str,
    pub all_venues: &'static str,
    pub hints_watch: &'static str,
    pub nl_soon: &'static str,
    pub nl_try: &'static str,
    pub cancelled: &'static str,
    pub bye: &'static str,
    pub help_title: &'static str,
    pub help: [(&'static str, &'static str); 9],
    pub offline: &'static str,
}

pub const ZH: Strings = Strings {
    ask: "想做什么？",
    menu: [
        ("查行情", "看一个币在各家市场的价格"),
        ("盯盘", "实时刷新价格"),
        ("搜索", "找币种和市场，识别仿冒币"),
        ("我的资产", "余额 · 持仓 · 挂单"),
        ("交易", "现货 · 合约"),
        ("设置", "代理 · 涨跌颜色"),
        ("退出", ""),
    ],
    soon: "即将推出",
    placeholder: "输入命令，比如 price SOL",
    hints_menu: "↑↓ 选择 · enter 确认 · 直接打字输入命令 · ctrl+c 退出",
    hints_input: "enter 执行 · tab 补全 · ↑↓ 历史 · → 接受建议 · esc 返回菜单",
    hints_pick: "↑↓ 选择 · enter 确认 · esc 返回",
    hints_busy: "ctrl+c 取消",
    pick_asset: "哪个币？",
    pick_asset_hint: "输入代码，比如 SOL；或者从下面选",
    pick_venue: "在哪看？",
    all_venues: "全部市场",
    hints_watch: "esc / q 停止盯盘",
    nl_soon: "自然语言助手还在路上（M4）。",
    nl_try: "你可能想要",
    cancelled: "已取消",
    bye: "下次见。市场不睡觉，你要睡。",
    help_title: "命令",
    help: [
        ("price <币>", "跨市场价格，比如 price SOL ETH、price ETH-PERP@hl"),
        ("watch <币>", "实时盯盘，比如 watch SOL BTC"),
        ("search <关键词>", "在各家市场搜索，标出已验证和仿冒的代币"),
        ("config show", "查看设置"),
        ("config set <键> <值>", "修改设置，比如 config set network.proxy http://127.0.0.1:7890"),
        ("menu", "回到菜单"),
        ("clear", "清屏"),
        ("help", "显示这份帮助"),
        ("exit", "退出"),
    ],
    offline: "连不上",
};

pub const EN: Strings = Strings {
    ask: "What would you like to do?",
    menu: [
        ("Prices", "One asset across every venue"),
        ("Watch", "Live prices"),
        ("Search", "Find markets, spot impostor tokens"),
        ("Portfolio", "Balances · positions · orders"),
        ("Trade", "Spot · perps"),
        ("Settings", "Proxy · up/down colors"),
        ("Quit", ""),
    ],
    soon: "coming soon",
    placeholder: "Type a command, e.g. price SOL",
    hints_menu: "↑↓ select · enter confirm · type to enter a command · ctrl+c quit",
    hints_input: "enter run · tab complete · ↑↓ history · → accept suggestion · esc menu",
    hints_pick: "↑↓ select · enter confirm · esc back",
    hints_busy: "ctrl+c cancel",
    pick_asset: "Which asset?",
    pick_asset_hint: "Type a symbol like SOL, or pick one below",
    pick_venue: "Where?",
    all_venues: "All venues",
    hints_watch: "esc / q stop watching",
    nl_soon: "The natural-language assistant is on its way (M4).",
    nl_try: "You might want",
    cancelled: "Cancelled",
    bye: "See you. Markets never sleep; you should.",
    help_title: "Commands",
    help: [
        ("price <asset>", "Cross-venue prices, e.g. price SOL ETH, price ETH-PERP@hl"),
        ("watch <asset>", "Live prices, e.g. watch SOL BTC"),
        ("search <text>", "Search every venue; verified and impostor tokens marked"),
        ("config show", "Show settings"),
        (
            "config set <key> <value>",
            "Change a setting, e.g. config set network.proxy http://127.0.0.1:7890",
        ),
        ("menu", "Back to the menu"),
        ("clear", "Clear the screen"),
        ("help", "Show this help"),
        ("exit", "Quit"),
    ],
    offline: "offline",
};

pub fn strings(lang: Lang) -> &'static Strings {
    match lang {
        Lang::Zh => &ZH,
        Lang::En => &EN,
    }
}
