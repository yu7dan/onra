//! `onra policy`, `onra halt`, `onra resume`, `onra audit`: the rules the
//! signer enforces, the emergency brake, and the record of what happened.
//!
//! Reading is open to everyone, agents included. Changing a rule or
//! releasing the brake takes the vault passphrase; pulling the brake
//! doesn't, because stopping is always safe.

use onra_core::{ErrorCode, Result};
use onra_policy::Policy;
use onra_policy::audit::Entry;
use onra_ui::text::pad;
use serde_json::{Value, json};

use crate::cli::PolicyAction;
use crate::commands::Outcome;
use crate::i18n::{self, fill};
use crate::signer::{self, guard, ipc};

fn seal_key() -> Result<zeroize::Zeroizing<Vec<u8>>> {
    signer::require_human("changing the policy", "onra policy set <key> <value>")?;
    let passphrase = signer::hidden(i18n::s().prompt_vault)?;
    let hex_key = signer::vault()?.seal_key(&passphrase)?;
    guard::seal_bytes(&hex_key)
}

fn usd(v: rust_decimal::Decimal) -> String {
    format!("${}", v.round_dp(2).normalize())
}

// ---------------------------------------------------------------------------
// policy show | set | reset

pub fn policy(action: Option<PolicyAction>) -> Result<Outcome> {
    match action.unwrap_or(PolicyAction::Show) {
        PolicyAction::Show => show(),
        PolicyAction::Set { key, value } => {
            let seal = seal_key()?;
            let mut policy = guard::load_policy(&seal)?;
            policy.set(&key, &value)?;
            guard::save_policy(&policy, &seal)?;
            guard::audit_log()?.append(
                &seal,
                guard::now_ms(),
                "policy",
                None,
                json!({ "set": key, "value": value }),
            )?;
            show()
        }
        PolicyAction::Reset => {
            let seal = seal_key()?;
            guard::save_policy(&Policy::default(), &seal)?;
            guard::audit_log()?.append(
                &seal,
                guard::now_ms(),
                "policy",
                None,
                json!({ "reset": true }),
            )?;
            show()
        }
    }
}

fn show() -> Result<Outcome> {
    let custom = guard::peek_policy()?;
    let policy = custom.clone().unwrap_or_default();
    let halted = guard::halted()?;
    let today = guard::audit_log()?.usage(guard::now_ms())?.spent_today_usd;
    let data = json!({
        "policy": policy,
        "source": if custom.is_some() { "file" } else { "defaults" },
        "halted": halted,
        "spent_today_usd": today.to_string(),
    });
    Ok(Outcome::new(data, move |theme| {
        let s = i18n::s();
        let p = &policy;
        let source = if custom.is_some() { s.policy_sealed } else { s.policy_defaults };
        let rows: [(&str, String, String); 8] = [
            ("max_trade_usd", usd(p.max_trade_usd), s.pol_max_trade.into()),
            (
                "max_daily_usd",
                usd(p.max_daily_usd),
                fill(s.pol_max_daily, &[("used", &usd(today))]),
            ),
            ("max_leverage", format!("{}x", p.max_leverage.normalize()), s.pol_leverage.into()),
            (
                "max_slippage_pct",
                format!("{}%", p.max_slippage_pct.normalize()),
                s.pol_slippage.into(),
            ),
            (
                "verified_only",
                p.verified_only.to_string(),
                if p.verified_only { s.pol_verified_on } else { s.pol_verified_off }.into(),
            ),
            ("venues", p.venues.join(","), s.pol_venues.into()),
            (
                "agent_approval",
                p.agent_approval.to_string(),
                if p.agent_approval { s.pol_approval_on } else { s.pol_approval_off }.into(),
            ),
            ("max_trades_per_minute", p.max_trades_per_minute.to_string(), s.pol_rate.into()),
        ];
        let mut lines = vec![
            String::new(),
            format!(
                "  {} {}  {}",
                theme.brand("◆"),
                theme.strong(s.policy_title),
                theme.faint(source)
            ),
            String::new(),
        ];
        for (key, value, about) in rows {
            lines.push(format!(
                "    {}  {}  {}",
                theme.dim(&pad(key, 22)),
                theme.strong(&pad(&value, 10)),
                theme.faint(&about)
            ));
        }
        lines.push(String::new());
        lines.push(if halted {
            format!("    {} {}", theme.warn("■"), fill(s.pol_halted, &[("cmd", "onra resume")]))
        } else {
            format!("    {} {}", theme.brand("●"), s.pol_running)
        });
        lines.push(String::new());
        lines.push(format!("  {}", theme.faint(s.pol_change_hint)));
        lines.push(String::new());
        lines
    }))
}

// ---------------------------------------------------------------------------
// halt | resume

pub async fn halt() -> Result<Outcome> {
    guard::set_halt_file(true)?;
    // An unlocked signer also seals the halt into the audit log.
    let logged = match ipc::call(&ipc::Request::Halt).await {
        Ok(Some(data)) => data.get("logged").and_then(Value::as_bool).unwrap_or(false),
        _ => false,
    };
    Ok(Outcome::new(json!({ "halted": true, "logged": logged }), |theme| {
        let s = i18n::s();
        vec![format!("  {} {}", theme.warn("■"), fill(s.halted_ok, &[("cmd", "onra resume")]))]
    }))
}

pub fn resume() -> Result<Outcome> {
    signer::require_human("resuming trading", "onra resume")?;
    let passphrase = signer::hidden(i18n::s().prompt_vault)?;
    let seal = guard::seal_bytes(&signer::vault()?.seal_key(&passphrase)?)?;
    guard::audit_log()?.append(&seal, guard::now_ms(), guard::RESUME, None, json!({}))?;
    guard::set_halt_file(false)?;
    Ok(Outcome::new(json!({ "halted": false }), |theme| {
        vec![format!("  {} {}", theme.brand("●"), i18n::s().resumed_ok)]
    }))
}

// ---------------------------------------------------------------------------
// audit

pub async fn audit(limit: usize, verify: bool) -> Result<Outcome> {
    let log = guard::audit_log()?;
    let entries = log.read()?;
    let intact = if verify {
        // The signer holds the seal key while unlocked; otherwise ask.
        match ipc::call(&ipc::Request::Verify).await {
            Ok(Some(_)) => Some(true),
            Err(e) if e.code == ErrorCode::Policy => return Err(e),
            _ => {
                signer::require_human("verifying the audit log", "onra audit --verify")?;
                let passphrase = signer::hidden(i18n::s().prompt_vault)?;
                let seal = guard::seal_bytes(&signer::vault()?.seal_key(&passphrase)?)?;
                log.verify(&seal)?;
                Some(true)
            }
        }
    } else {
        None
    };
    let total = entries.len();
    let shown: Vec<Entry> = entries.into_iter().rev().take(limit).collect();
    let data = json!({ "total": total, "intact": intact, "entries": shown });
    Ok(Outcome::new(data, move |theme| {
        let s = i18n::s();
        let mut lines = vec![String::new()];
        if shown.is_empty() {
            lines.push(format!("  {}", theme.faint(s.audit_empty)));
        }
        for e in shown.iter().rev() {
            let who = match e.initiator {
                Some(onra_policy::Initiator::Agent) => "agent",
                Some(onra_policy::Initiator::Human) => "human",
                None => "",
            };
            lines.push(format!(
                "  {}  {}  {}  {}  {}",
                theme.faint(&format!("#{:<4}", e.seq)),
                theme.dim(&time(e.ts)),
                theme.strong(&pad(&e.event, 10)),
                theme.faint(&pad(who, 5)),
                summary(&e.data)
            ));
        }
        if let Some(true) = intact {
            lines.push(String::new());
            lines.push(format!(
                "  {} {}",
                theme.brand("✓"),
                fill(s.audit_intact, &[("n", &total.to_string())])
            ));
        }
        lines.push(String::new());
        lines
    }))
}

fn time(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (_, m, d) = civil(days as i64);
    format!("{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// One line about an entry: its own `summary` if it has one, else its fields.
fn summary(data: &Value) -> String {
    if let Some(s) = data.get("summary").and_then(Value::as_str) {
        return s.to_string();
    }
    match data.as_object() {
        Some(map) if !map.is_empty() => map
            .iter()
            .map(|(k, v)| {
                format!("{k}={}", v.as_str().map_or_else(|| v.to_string(), str::to_string))
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_times() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_773), (2026, 11, 16));
        assert_eq!(time(1_794_825_045_000), "11-16 10:30:45");
    }
}
