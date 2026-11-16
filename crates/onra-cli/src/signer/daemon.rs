//! `onra daemon`: the only process that ever holds a trading key.
//!
//! It lives exactly as long as it is unlocked. Locking, or the unlock period
//! running out, ends the process, and the keys go with its memory. A daemon
//! that is started but never unlocked gives up after a short wait.

use std::collections::{BTreeMap, HashMap};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::traits::tokio::Listener as _;
use onra_core::{Error, ErrorCode, Result, VenueId};
use onra_keystore::SEAL_KEY;
use onra_keystore::TradingVault;
use onra_policy::{Decision, Initiator};
use onra_signer::TradingKey;
use onra_venues::{BinanceKey, Venues};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::ipc::{self, KeyInfo, Request, Response, Status};
use super::{exec, guard, vault};
use crate::trade::Plan;

/// How long a fresh daemon waits for its first unlock.
const FIRST_UNLOCK: Duration = Duration::from_secs(30);
/// The longest anyone can keep the signer unlocked in one go.
pub const MAX_TTL: Duration = Duration::from_secs(12 * 3600);

struct Unlocked {
    keys: BTreeMap<String, TradingKey>,
    /// Seals the policy file and the audit log.
    seal: Zeroizing<Vec<u8>>,
    until: Instant,
}

/// A plan the signer made and still honors.
struct Pending {
    plan: Plan,
    approved: bool,
}

#[derive(Default)]
struct State {
    unlocked: Option<Unlocked>,
    plans: HashMap<String, Pending>,
    stop: bool,
}

impl State {
    fn status(&self) -> Status {
        let policy_ok = self.unlocked.as_ref().map(|u| guard::load_policy(&u.seal).is_ok());
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
            policy_ok,
        }
    }
}

pub fn run() -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::new(ErrorCode::Internal, format!("cannot start runtime: {e}")))?;
    // The signer prices trades itself, through the same network settings.
    let net = crate::config::load().map(|c| c.net(None)).unwrap_or_default();
    let venues = Venues::new(&net)?;
    runtime.block_on(serve(vault()?, venues))?;
    Ok(ExitCode::SUCCESS)
}

async fn serve(vault: TradingVault, venues: Venues) -> Result<()> {
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
                if let Ok(conn) = conn {
                    handle(conn, &state, &vault, &venues).await;
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
async fn handle(conn: Stream, state: &Mutex<State>, vault: &TradingVault, venues: &Venues) {
    // A client that connects and goes quiet must not stall the signer.
    let request =
        match tokio::time::timeout(Duration::from_secs(10), ipc::read_line::<Request>(&conn)).await
        {
            Ok(Ok(Some(request))) => request,
            Ok(Err(e)) => {
                let _ = ipc::write_line(&conn, &Response::err(e)).await;
                return;
            }
            _ => return,
        };
    let result = match request {
        Request::Plan { .. }
        | Request::Exec { .. }
        | Request::Approve { .. }
        | Request::Show { .. }
        | Request::Plans => trade(request, state, vault, venues).await,
        other => apply(other, state, vault),
    };
    let response = match result {
        Ok(data) => Response::ok(data),
        Err(e) => Response::err(e),
    };
    let _ = ipc::write_line(&conn, &response).await;
}

fn status(s: &State) -> Value {
    serde_json::to_value(s.status()).expect("status serializes")
}

fn locked() -> Error {
    Error::new(ErrorCode::Locked, "the signer is locked")
        .with_hint(json!({ "cause": "locked", "suggest": "onra unlock" }))
}

/// The Binance key for the plan's network and the seal key, copied out so
/// no lock is held while talking to the venue.
/// The key for a venue and network, and the seal key, copied out so no lock
/// is held while talking to the venue.
fn credentials(
    state: &Mutex<State>,
    venue: VenueId,
    testnet: bool,
) -> Result<(exec::Signer, Zeroizing<Vec<u8>>)> {
    let s = state.lock().expect("state lock");
    let u = s.unlocked.as_ref().ok_or_else(locked)?;
    let base = if venue == VenueId::Hyperliquid { "hl" } else { "bn" };
    let name = if testnet { format!("{base}.testnet") } else { base.to_string() };
    let missing = || {
        let flag = if testnet { " --testnet" } else { "" };
        Error::new(ErrorCode::Credentials, format!("no {name} trading key in the vault")).with_hint(
            json!({ "cause": "no_trading_key", "venue": venue, "suggest": format!("onra account trade {base}{flag}") }),
        )
    };
    let signer = match (venue, u.keys.get(&name)) {
        (VenueId::Binance, Some(TradingKey::Binance(k))) => {
            exec::Signer::Binance(BinanceKey { key: k.key.clone(), secret: k.secret.clone() })
        }
        (VenueId::Hyperliquid, Some(TradingKey::Hyperliquid(k))) => {
            // The API wallet trades for the linked account; read it fresh.
            let user = crate::config::load()?.accounts().hyperliquid.clone().ok_or_else(|| {
                Error::new(ErrorCode::Credentials, "no Hyperliquid account is linked").with_hint(
                    json!({ "venue": "hyperliquid", "cause": "no_accounts", "suggest": "onra account add hl <address>" }),
                )
            })?;
            exec::Signer::Hyperliquid { key: k.clone(), user }
        }
        _ => return Err(missing()),
    };
    Ok((signer, u.seal.clone()))
}

fn denied(verdict: &onra_policy::Verdict) -> Error {
    let failed: Vec<_> = verdict.failed().collect();
    let what = failed
        .iter()
        .map(|c| format!("{} ({} > {})", c.rule, c.value, c.limit))
        .collect::<Vec<_>>()
        .join(", ");
    Error::new(ErrorCode::Policy, format!("blocked by policy: {what}"))
        .with_hint(json!({ "cause": "denied", "checks": failed }))
}

fn to_value<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("serializes")
}

/// Takes a live plan out of the book; expired ones are dropped on the way.
fn take(state: &Mutex<State>, id: &str) -> Result<Pending> {
    let mut s = state.lock().expect("state lock");
    let now = guard::now_ms();
    s.plans.retain(|_, p| p.plan.expires_ms > now);
    s.plans.remove(id).ok_or_else(|| {
        Error::new(
            ErrorCode::PlanExpired,
            format!("plan {id} is unknown or expired; make a new one"),
        )
        .with_hint(json!({ "cause": "plan_expired", "plan_id": id }))
    })
}

async fn trade(
    request: Request,
    state: &Mutex<State>,
    vault: &TradingVault,
    venues: &Venues,
) -> Result<Value> {
    match request {
        Request::Plan { request, initiator } => {
            let (signer, seal) = credentials(state, exec::venue_of(&request), request.testnet)?;
            let plan = exec::plan(&request, initiator, venues, &signer, &seal).await?;
            if plan.verdict.decision != Decision::Deny {
                let pending = Pending { plan: plan.clone(), approved: false };
                state.lock().expect("state lock").plans.insert(plan.id.clone(), pending);
            }
            Ok(to_value(&plan))
        }
        Request::Exec { plan_id, initiator } => {
            let pending = take(state, &plan_id)?;
            if pending.plan.verdict.decision == Decision::NeedsApproval && !pending.approved {
                let plan = pending.plan.clone();
                state.lock().expect("state lock").plans.insert(plan_id.clone(), pending);
                return Err(Error::new(
                    ErrorCode::ApprovalRequired,
                    format!("plan {plan_id} waits for a person to approve it"),
                )
                .with_hint(json!({ "cause": "approval", "plan_id": plan_id, "summary": plan.summary(), "suggest": format!("onra approve {plan_id}") })));
            }
            carry_out(pending.plan, initiator, false, state, venues).await
        }
        Request::Approve { plan_id, passphrase } => {
            // Only someone with the passphrase can approve.
            vault.seal_key(&passphrase)?;
            let pending = take(state, &plan_id)?;
            let (_, seal) = credentials(state, pending.plan.venue, pending.plan.testnet)?;
            guard::audit_log()?.append(
                &seal,
                guard::now_ms(),
                "approved",
                Some(Initiator::Human),
                json!({ "plan_id": plan_id, "summary": pending.plan.summary() }),
            )?;
            carry_out(pending.plan, Initiator::Human, true, state, venues).await
        }
        Request::Show { plan_id } => {
            let mut s = state.lock().expect("state lock");
            let now = guard::now_ms();
            s.plans.retain(|_, p| p.plan.expires_ms > now);
            let p = s.plans.get(&plan_id).ok_or_else(|| {
                Error::new(ErrorCode::PlanExpired, format!("plan {plan_id} is unknown or expired"))
                    .with_hint(json!({ "cause": "plan_expired", "plan_id": plan_id }))
            })?;
            Ok(to_value(&p.plan))
        }
        Request::Plans => {
            let mut s = state.lock().expect("state lock");
            let now = guard::now_ms();
            s.plans.retain(|_, p| p.plan.expires_ms > now);
            let mut plans: Vec<&Plan> = s.plans.values().map(|p| &p.plan).collect();
            plans.sort_by_key(|p| p.created_ms);
            Ok(to_value(&plans))
        }
        _ => unreachable!("routed by handle"),
    }
}

/// Re-judges a plan against the policy and the day's trading as they are
/// now, then places the order.
async fn carry_out(
    plan: Plan,
    initiator: Initiator,
    approved: bool,
    state: &Mutex<State>,
    venues: &Venues,
) -> Result<Value> {
    let (signer, seal) = credentials(state, plan.venue, plan.testnet)?;
    // An approval is a person's decision; otherwise the plan's own origin counts.
    let judged_as = if approved { Initiator::Human } else { plan.initiator };
    let verdict = exec::judge(&seal, &plan, judged_as)?;
    if verdict.decision == Decision::Deny {
        return Err(denied(&verdict));
    }
    let execution = exec::execute(&plan, initiator, venues, &signer, &seal).await?;
    Ok(to_value(&execution))
}

fn apply(request: Request, state: &Mutex<State>, vault: &TradingVault) -> Result<Value> {
    match request {
        Request::Status => Ok(status(&state.lock().expect("state lock"))),
        Request::Unlock { passphrase, ttl_secs } => {
            // Always re-read the vault: this also picks up keys added since.
            let secrets = vault.unlock(&passphrase)?;
            let mut keys = BTreeMap::new();
            for (name, secret) in secrets.iter().filter(|(n, _)| !n.starts_with('_')) {
                keys.insert(name.clone(), TradingKey::decode(name, secret)?);
            }
            let seal = guard::seal_bytes(&secrets[SEAL_KEY])?;
            let ttl = Duration::from_secs(ttl_secs).min(MAX_TTL);
            let mut s = state.lock().expect("state lock");
            s.unlocked = Some(Unlocked { keys, seal, until: Instant::now() + ttl });
            Ok(status(&s))
        }
        Request::Lock => {
            let mut s = state.lock().expect("state lock");
            s.unlocked = None;
            s.stop = true;
            Ok(status(&s))
        }
        Request::Halt => {
            guard::set_halt_file(true)?;
            let s = state.lock().expect("state lock");
            // Sealing the halt into the log makes it outlive the file.
            let logged = match &s.unlocked {
                Some(u) => {
                    guard::audit_log()?.append(
                        &u.seal,
                        guard::now_ms(),
                        guard::HALT,
                        None,
                        json!({}),
                    )?;
                    true
                }
                None => false,
            };
            Ok(json!({ "halted": true, "logged": logged }))
        }
        Request::Verify => {
            let s = state.lock().expect("state lock");
            let u = s.unlocked.as_ref().ok_or_else(locked)?;
            let entries = guard::audit_log()?.verify(&u.seal)?;
            Ok(json!({ "entries": entries, "intact": true }))
        }
        _ => unreachable!("routed by handle"),
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

        let st = |r| serde_json::from_value::<Status>(apply(r, &state, &vault).unwrap()).unwrap();
        let locked = st(Request::Status);
        assert!(!locked.unlocked && locked.keys.is_empty());
        assert!(apply(unlock("wrong", 60), &state, &vault).is_err());

        let open = st(unlock("passphrase", 900));
        assert!(open.unlocked && open.expires_in_secs == 900);
        assert_eq!(open.keys.len(), 1);
        assert_eq!(open.keys[0].name, "hl.testnet");
        assert_eq!(open.keys[0].public, "0x2c7536E3605D9C16a7a3D7b1898e529396a65c23");

        // Nobody can hold it open longer than the cap.
        let long = st(unlock("passphrase", 999_999));
        assert_eq!(long.expires_in_secs, MAX_TTL.as_secs());

        let after = st(Request::Lock);
        assert!(!after.unlocked && after.keys.is_empty());
        assert!(state.lock().unwrap().stop);
    }
}
