//! Tab completion for the shell's input line.

use onra_core::VenueId;

use crate::config::KEYS;

pub const COMMANDS: [&str; 13] = [
    "price",
    "watch",
    "search",
    "portfolio",
    "balance",
    "positions",
    "orders",
    "account",
    "config",
    "menu",
    "help",
    "clear",
    "exit",
];
const ACCOUNT_ACTIONS: [&str; 4] = ["list", "add", "login", "remove"];
const CONFIG_ACTIONS: [&str; 4] = ["show", "set", "unset", "path"];

/// Symbols offered before the user has typed anything distinctive. The
/// venues list far more; any symbol can still be typed in full.
pub const POPULAR: [&str; 16] = [
    "BTC", "ETH", "SOL", "HYPE", "BNB", "XRP", "DOGE", "SUI", "JUP", "BONK", "WIF", "PEPE", "AVAX",
    "LINK", "TON", "ARB",
];

/// Completed lines for `line`, replacing its last word.
pub fn complete(line: &str) -> Vec<String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let fresh_word = line.is_empty() || line.ends_with(' ');
    let (done, partial) = if fresh_word {
        (&words[..], "")
    } else {
        (&words[..words.len() - 1], words[words.len() - 1])
    };
    let head = if done.is_empty() { String::new() } else { format!("{} ", done.join(" ")) };

    let options: Vec<String> = match done {
        [] => COMMANDS.iter().map(|s| s.to_string()).collect(),
        ["config"] => CONFIG_ACTIONS.iter().map(|s| s.to_string()).collect(),
        ["account"] => ACCOUNT_ACTIONS.iter().map(|s| s.to_string()).collect(),
        ["account", "add" | "remove"] => vec!["hl".into(), "sol".into(), "bn".into()],
        ["account", "login"] => vec!["bn".into()],
        ["config", "set" | "unset"] => KEYS.iter().map(|s| s.to_string()).collect(),
        ["config", "set", "ui.color_convention"] => vec!["east".into(), "west".into()],
        ["config", "set", "ui.language"] => vec!["auto".into(), "en".into(), "zh".into()],
        [cmd, ..] if *cmd == "price" || *cmd == "watch" => market_options(partial),
        _ => Vec::new(),
    };

    let lower = partial.to_ascii_lowercase();
    options
        .into_iter()
        .filter(|o| o.to_ascii_lowercase().starts_with(&lower) && o.len() > partial.len())
        .map(|o| format!("{head}{o}"))
        .collect()
}

fn market_options(partial: &str) -> Vec<String> {
    match partial.split_once('@') {
        Some((base, _)) => VenueId::ALL
            .iter()
            .map(|v| format!("{}@{}", base.to_ascii_uppercase(), v.alias()))
            .collect(),
        None => POPULAR.iter().map(|s| s.to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_commands_and_arguments() {
        assert_eq!(complete("pr"), vec!["price"]);
        assert_eq!(complete("config s"), vec!["config show", "config set"]);
        assert_eq!(complete("price so"), vec!["price SOL"]);
        assert_eq!(complete("price ETH sol@h"), vec!["price ETH SOL@hl"]);
        assert!(complete("price SOL ").contains(&"price SOL BTC".to_string()));
        assert!(complete("nonsense x").is_empty());
    }
}
