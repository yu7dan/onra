//! The signer: trading keys live in a daemon that a person unlocks.
//!
//! Keys that can move money are typed into hidden prompts and sealed in the
//! trading vault with a passphrase. Only `onra daemon` ever opens the vault;
//! `onra unlock` asks for the passphrase and hands it over, and the daemon
//! keeps the keys in memory until it locks. Agents can ask the daemon to act
//! within policy, but nothing they can call returns a key, and nothing they
//! can call unlocks it: both need a person at the keyboard.

pub mod daemon;
mod ipc;

use std::io::IsTerminal;
use std::time::Duration;

use onra_core::{Error, ErrorCode, Result, VenueId};
use onra_keystore::TradingVault;
use onra_signer::{BinanceKey, EvmKey, SolanaKey};
use onra_venues::Venues;
use serde_json::json;
use zeroize::Zeroizing;

pub use ipc::Status;

use crate::account::{account_venue, read_line, short};
use crate::commands::Outcome;
use crate::config::Config;
use crate::i18n::{self, fill};

const MIN_PASSPHRASE: usize = 8;

pub fn vault() -> Result<TradingVault> {
    let dir = dirs::data_dir()
        .ok_or_else(|| Error::new(ErrorCode::Credentials, "cannot locate the data directory"))?;
    Ok(TradingVault::new(dir.join("onra").join("trading.json")))
}

/// Unlocking and adding keys need a person: an agent driving Onra through a
/// pipe gets a clear error to pass on instead of a prompt nobody answers.
fn require_human(what: &str) -> Result<()> {
    if std::io::stdin().is_terminal() {
        return Ok(());
    }
    Err(Error::new(ErrorCode::Credentials, format!("{what} needs a person at the keyboard"))
        .with_hint(json!({ "cause": "needs_human", "suggest": "onra unlock" })))
}

fn hidden(prompt: &str) -> Result<Zeroizing<String>> {
    rpassword::prompt_password(prompt)
        .map(Zeroizing::new)
        .map_err(|e| Error::new(ErrorCode::Usage, format!("cannot read input: {e}")))
}

/// The vault passphrase: asked once for an existing vault, twice for a new one.
fn vault_passphrase(vault: &TradingVault) -> Result<Zeroizing<String>> {
    let s = i18n::s();
    if vault.exists()? {
        return hidden(s.prompt_vault);
    }
    println!("  {}", s.vault_new);
    let first = hidden(s.prompt_vault_new)?;
    if first.chars().count() < MIN_PASSPHRASE {
        return Err(Error::new(
            ErrorCode::Usage,
            format!("the passphrase needs at least {MIN_PASSPHRASE} characters"),
        ));
    }
    if *hidden(s.prompt_vault_repeat)? != *first {
        return Err(Error::new(ErrorCode::Usage, "the passphrases do not match"));
    }
    Ok(first)
}

/// The daemon's status, or `None` when it isn't running (locked).
pub async fn status() -> Option<Status> {
    ipc::call(&ipc::Request::Status).await.ok().flatten()
}

pub fn minutes(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs.div_ceil(60)),
        _ => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
    }
}

// ---------------------------------------------------------------------------
// unlock | lock

pub async fn unlock(ttl: Duration) -> Result<Outcome> {
    require_human("unlocking the signer")?;
    let vault = vault()?;
    if vault.names()?.is_empty() {
        return Err(Error::new(ErrorCode::Credentials, "no trading keys yet").with_hint(
            json!({ "cause": "no_trading_keys", "suggest": "onra account trade <venue>" }),
        ));
    }
    let passphrase = hidden(i18n::s().prompt_vault)?;
    ensure_running().await?;
    let request = ipc::Request::Unlock { passphrase, ttl_secs: ttl.as_secs() };
    let status = ipc::call(&request)
        .await?
        .ok_or_else(|| Error::new(ErrorCode::Internal, "the signer stopped unexpectedly"))?;
    let data = serde_json::to_value(&status).expect("status serializes");
    Ok(Outcome::new(data, move |theme| {
        let s = i18n::s();
        let mut lines = vec![
            String::new(),
            format!(
                "  {} {}",
                theme.brand("✓"),
                fill(s.unlocked_ok, &[("ttl", &minutes(status.expires_in_secs))])
            ),
        ];
        for key in &status.keys {
            lines.push(format!(
                "    {}  {}",
                theme.dim(&format!("{:<12}", key.name)),
                short(&key.public)
            ));
        }
        lines.push(String::new());
        lines
    }))
}

pub async fn lock() -> Result<Outcome> {
    let was_running = ipc::call(&ipc::Request::Lock).await?.is_some();
    Ok(Outcome::new(json!({ "unlocked": false, "was_running": was_running }), move |theme| {
        let s = i18n::s();
        let text = if was_running { s.locked_ok } else { s.already_locked };
        vec![format!("  {} {text}", theme.brand("✓"))]
    }))
}

/// Starts the daemon in the background if it isn't running, and waits for it.
async fn ensure_running() -> Result<()> {
    if ipc::connect().await.is_some() {
        return Ok(());
    }
    let exe = std::env::current_exe()
        .map_err(|e| Error::new(ErrorCode::Internal, format!("cannot locate onra: {e}")))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: Ctrl+C in this terminal must not kill it.
        cmd.process_group(0);
    }
    cmd.spawn()
        .map_err(|e| Error::new(ErrorCode::Internal, format!("cannot start the signer: {e}")))?;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if ipc::connect().await.is_some() {
            return Ok(());
        }
    }
    Err(Error::new(ErrorCode::Internal, "the signer did not start"))
}

// ---------------------------------------------------------------------------
// account trade <venue>

fn entry(venue: VenueId, testnet: bool) -> String {
    let base = match venue {
        VenueId::Binance => "bn",
        VenueId::Hyperliquid => "hl",
        VenueId::Jupiter => "sol",
    };
    if testnet { format!("{base}.testnet") } else { base.to_string() }
}

pub async fn add_key(
    venue: &str,
    testnet: bool,
    config: &mut Config,
    venues: &Venues,
) -> Result<Outcome> {
    let venue = account_venue(venue)?;
    if testnet && venue == VenueId::Jupiter {
        return Err(Error::new(ErrorCode::Usage, "Jupiter has no testnet"));
    }
    require_human("adding a trading key")?;
    let s = i18n::s();
    let name = entry(venue, testnet);

    let (secret, public): (Zeroizing<String>, String) = match venue {
        VenueId::Binance => {
            let key = read_line(s.prompt_key)?;
            let secret = hidden(s.prompt_secret)?;
            if key.is_empty() || secret.is_empty() {
                return Err(Error::new(
                    ErrorCode::Usage,
                    "both the API key and its secret are required",
                ));
            }
            if !testnet {
                println!("  {}", s.checking_key);
                let probe = onra_venues::BinanceKey { key: key.clone(), secret: secret.clone() };
                let can = venues.binance_restrictions(&probe).await?;
                if can.enable_withdrawals {
                    return Err(Error::new(
                        ErrorCode::Credentials,
                        "this key can withdraw funds; Onra refuses keys with withdrawal permission. Create a key without it",
                    )
                    .with_hint(json!({ "venue": "binance", "cause": "withdrawals" })));
                }
                if !can.enable_spot_and_margin_trading {
                    return Err(Error::new(
                        ErrorCode::Credentials,
                        "this key cannot trade; enable spot trading for it",
                    )
                    .with_hint(json!({ "venue": "binance", "cause": "cannot_trade" })));
                }
            }
            let stored = BinanceKey { key: key.clone(), secret };
            (Zeroizing::new(serde_json::to_string(&stored).expect("key serializes")), key)
        }
        VenueId::Hyperliquid => {
            let Some(main) = config.accounts().hyperliquid.clone() else {
                return Err(Error::new(
                    ErrorCode::Credentials,
                    "link your Hyperliquid account first; the API wallet trades on its behalf",
                )
                .with_hint(json!({ "venue": "hyperliquid", "cause": "no_accounts", "suggest": "onra account add hl <address>" })));
            };
            println!("  {}", s.hl_agent_note);
            let secret = hidden(s.prompt_hl_agent)?;
            let address = EvmKey::parse(&secret)?.address();
            // The main wallet's key can withdraw. Only an API wallet is accepted.
            if address.eq_ignore_ascii_case(&main) {
                return Err(Error::new(
                    ErrorCode::Credentials,
                    "this is your main wallet's key, which can withdraw. Create an API wallet on Hyperliquid and use its key",
                )
                .with_hint(json!({ "venue": "hyperliquid", "cause": "main_wallet" })));
            }
            (secret, address)
        }
        VenueId::Jupiter => {
            let secret = hidden(s.prompt_sol_keypair)?;
            let address = SolanaKey::parse(&secret)?.address();
            match config.accounts().solana.clone() {
                Some(linked) if linked != address => {
                    return Err(Error::new(
                        ErrorCode::Credentials,
                        format!("this keypair is for {address}, but the linked Solana wallet is {linked}"),
                    )
                    .with_hint(json!({ "venue": "jupiter", "cause": "wallet_mismatch", "suggest": format!("onra account add sol {address}") })));
                }
                Some(_) => {}
                None => config.accounts_mut().solana = Some(address.clone()),
            }
            (secret, address)
        }
    };

    let vault = vault()?;
    let passphrase = vault_passphrase(&vault)?;
    vault.insert(&name, &secret, &passphrase)?;
    config.save()?;
    let reload = status().await.is_some_and(|st| st.unlocked);

    let data = json!({ "venue": venue, "key": name, "public": public, "testnet": testnet });
    Ok(Outcome::new(data, move |theme| {
        let s = i18n::s();
        let mut lines = vec![format!(
            "  {} {}",
            theme.brand("✓"),
            fill(s.trade_key_added, &[("key", &name), ("public", &short(&public))])
        )];
        if reload {
            lines.push(format!("    {}", theme.faint(s.trade_key_reload)));
        }
        lines
    }))
}

pub fn remove_key(venue: &str, testnet: bool) -> Result<Outcome> {
    let name = entry(account_venue(venue)?, testnet);
    let removed = vault()?.remove(&name)?;
    Ok(Outcome::new(json!({ "key": name, "removed": removed }), move |theme| {
        let s = i18n::s();
        let text = if removed { s.trade_key_removed } else { s.trade_key_absent };
        vec![format!("  {} {}", theme.brand("✓"), fill(text, &[("key", &name)]))]
    }))
}
