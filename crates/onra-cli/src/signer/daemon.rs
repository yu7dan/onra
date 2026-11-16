//! `onra daemon`: the only process that ever holds a trading key.
//!
//! It lives exactly as long as it is unlocked. Locking, or the unlock period
//! running out, ends the process, and the keys go with its memory. A daemon
//! that is started but never unlocked gives up after a short wait.

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::traits::tokio::Listener as _;
use onra_core::{Error, ErrorCode, Result};
use onra_keystore::TradingVault;
use onra_signer::TradingKey;

use super::ipc::{self, KeyInfo, Request, Response, Status};
use super::vault;

/// How long a fresh daemon waits for its first unlock.
const FIRST_UNLOCK: Duration = Duration::from_secs(30);
/// The longest anyone can keep the signer unlocked in one go.
pub const MAX_TTL: Duration = Duration::from_secs(12 * 3600);

struct Unlocked {
    keys: BTreeMap<String, TradingKey>,
    until: Instant,
}

#[derive(Default)]
struct State {
    unlocked: Option<Unlocked>,
    stop: bool,
}

impl State {
    fn status(&self) -> Status {
        let (unlocked, expires_in_secs, keys) = match &self.unlocked {
            None => (false, 0, Vec::new()),
            Some(u) => (
                true,
                u.until.saturating_duration_since(Instant::now()).as_millis().div_ceil(1000) as u64,
                u.keys
                    .iter()
                    .map(|(name, key)| KeyInfo { name: name.clone(), public: key.public() })
                    .collect(),
            ),
        };
        Status {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            unlocked,
            expires_in_secs,
            keys,
        }
    }
}

pub fn run() -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::new(ErrorCode::Internal, format!("cannot start runtime: {e}")))?;
    runtime.block_on(serve(vault()?))?;
    Ok(ExitCode::SUCCESS)
}

async fn serve(vault: TradingVault) -> Result<()> {
    // Windows reports a taken pipe name as "access denied"; asking is clearer.
    if ipc::connect().await.is_some() {
        return Err(Error::new(ErrorCode::Usage, "a signer is already running"));
    }
    let listener = ipc::listen().map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            Error::new(ErrorCode::Usage, "a signer is already running")
        } else {
            Error::new(ErrorCode::Internal, format!("cannot open the signer endpoint: {e}"))
        }
    })?;
    let state = Arc::new(Mutex::new(State::default()));
    let born = Instant::now();

    loop {
        tokio::select! {
            conn = listener.accept() => {
                // A client that connects and goes quiet must not stall the signer.
                if let Ok(conn) = conn {
                    let _ = tokio::time::timeout(Duration::from_secs(10), handle(conn, &state, &vault)).await;
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
        let s = state.lock().expect("state lock");
        let expired = match &s.unlocked {
            Some(u) => Instant::now() >= u.until,
            None => born.elapsed() >= FIRST_UNLOCK,
        };
        if s.stop || expired {
            break;
        }
    }
    // Dropping the state zeroizes every key.
    drop(state);
    Ok(())
}

/// One request per connection, served to completion before the next: the
/// signer does little, and serial handling keeps its state simple.
async fn handle(conn: Stream, state: &Mutex<State>, vault: &TradingVault) {
    let response = match ipc::read_line::<Request>(&conn).await {
        Ok(Some(request)) => match apply(request, state, vault) {
            Ok(status) => Response::ok(status),
            Err(e) => Response::err(e),
        },
        Ok(None) => return,
        Err(e) => Response::err(e),
    };
    let _ = ipc::write_line(&conn, &response).await;
}

fn apply(request: Request, state: &Mutex<State>, vault: &TradingVault) -> Result<Status> {
    match request {
        Request::Status => Ok(state.lock().expect("state lock").status()),
        Request::Unlock { passphrase, ttl_secs } => {
            // Always re-read the vault: this also picks up keys added since.
            let secrets = vault.unlock(&passphrase)?;
            let mut keys = BTreeMap::new();
            for (name, secret) in secrets.iter() {
                keys.insert(name.clone(), TradingKey::decode(name, secret)?);
            }
            let ttl = Duration::from_secs(ttl_secs).min(MAX_TTL);
            let mut s = state.lock().expect("state lock");
            s.unlocked = Some(Unlocked { keys, until: Instant::now() + ttl });
            Ok(s.status())
        }
        Request::Lock => {
            let mut s = state.lock().expect("state lock");
            s.unlocked = None;
            s.stop = true;
            Ok(s.status())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroize::Zeroizing;

    const HL: &str = "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";

    fn unlock(pw: &str, ttl_secs: u64) -> Request {
        Request::Unlock { passphrase: Zeroizing::new(pw.into()), ttl_secs }
    }

    #[test]
    fn unlocks_reports_and_locks() {
        let dir = std::env::temp_dir().join(format!("onra-daemon-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let vault = TradingVault::new(dir.join("trading.json"));
        vault.insert("hl.testnet", HL, "passphrase").unwrap();
        let state = Mutex::new(State::default());

        let locked = apply(Request::Status, &state, &vault).unwrap();
        assert!(!locked.unlocked && locked.keys.is_empty());
        assert!(apply(unlock("wrong", 60), &state, &vault).is_err());

        let open = apply(unlock("passphrase", 900), &state, &vault).unwrap();
        assert!(open.unlocked && open.expires_in_secs == 900);
        assert_eq!(open.keys.len(), 1);
        assert_eq!(open.keys[0].name, "hl.testnet");
        assert_eq!(open.keys[0].public, "0x2c7536E3605D9C16a7a3D7b1898e529396a65c23");

        // Nobody can hold it open longer than the cap.
        let long = apply(unlock("passphrase", 999_999), &state, &vault).unwrap();
        assert_eq!(long.expires_in_secs, MAX_TTL.as_secs());

        let after = apply(Request::Lock, &state, &vault).unwrap();
        assert!(!after.unlocked && after.keys.is_empty());
        assert!(state.lock().unwrap().stop);
    }
}
