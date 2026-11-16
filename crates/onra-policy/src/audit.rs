//! The audit log: one JSON line per event, each sealed to the one before.
//!
//! Every line carries an HMAC over its content and the previous line's HMAC.
//! Removing, editing or reordering lines breaks the chain, and the key is in
//! the trading vault, so a process without the passphrase can't forge a new
//! one. The log also answers the policy's questions about the past: how
//! much was traded today, how many orders went out in the last minute.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use hmac::Mac;
use onra_core::{Error, ErrorCode, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{HmacSha256, Initiator, Usage};

/// An order went out to a venue. These count toward the daily limit and
/// the rate limit, whether or not they fill.
pub const SUBMITTED: &str = "submitted";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub ts: u64,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initiator: Option<Initiator>,
    pub data: Value,
    /// The previous entry's `mac`; empty for the first.
    pub prev: String,
    pub mac: String,
}

/// The sealed part of an entry: everything but the MAC itself.
#[derive(Serialize)]
struct Body<'a> {
    seq: u64,
    ts: u64,
    event: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    initiator: Option<Initiator>,
    data: &'a Value,
    prev: &'a str,
}

impl Entry {
    fn seal(&self, key: &[u8]) -> String {
        let body = Body {
            seq: self.seq,
            ts: self.ts,
            event: &self.event,
            initiator: self.initiator,
            data: &self.data,
            prev: &self.prev,
        };
        let mut m = HmacSha256::new_from_slice(key).expect("any key length works");
        m.update(serde_json::to_string(&body).expect("entry serializes").as_bytes());
        hex::encode(m.finalize().into_bytes())
    }
}

fn io_error(path: &std::path::Path, e: std::io::Error) -> Error {
    Error::new(ErrorCode::Internal, format!("audit log {}: {e}", path.display()))
}

pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn read(&self) -> Result<Vec<Entry>> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_error(&self.path, e)),
        };
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .enumerate()
            .map(|(i, line)| {
                serde_json::from_str(line).map_err(|_| {
                    Error::new(ErrorCode::Policy, format!("audit log line {} is unreadable", i + 1))
                        .with_hint(serde_json::json!({ "cause": "audit_broken" }))
                })
            })
            .collect()
    }

    pub fn append(
        &self,
        key: &[u8],
        now_ms: u64,
        event: &str,
        initiator: Option<Initiator>,
        data: Value,
    ) -> Result<Entry> {
        let last = self.read()?.pop();
        let mut entry = Entry {
            seq: last.as_ref().map_or(1, |e| e.seq + 1),
            ts: now_ms,
            event: event.to_string(),
            initiator,
            data,
            prev: last.map(|e| e.mac).unwrap_or_default(),
            mac: String::new(),
        };
        entry.mac = entry.seal(key);
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| io_error(dir, e))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| io_error(&self.path, e))?;
        let line = serde_json::to_string(&entry).expect("entry serializes");
        writeln!(file, "{line}").map_err(|e| io_error(&self.path, e))?;
        file.sync_data().map_err(|e| io_error(&self.path, e))?;
        Ok(entry)
    }

    /// Checks every link. Returns the number of entries, or the first
    /// sequence number where the chain breaks.
    pub fn verify(&self, key: &[u8]) -> Result<usize> {
        let entries = self.read()?;
        let mut prev = String::new();
        for (i, e) in entries.iter().enumerate() {
            if e.seq != i as u64 + 1 || e.prev != prev || e.mac != e.seal(key) {
                return Err(Error::new(
                    ErrorCode::Policy,
                    format!("the audit log was altered at entry {}", i + 1),
                )
                .with_hint(serde_json::json!({ "cause": "audit_broken", "seq": i + 1 })));
            }
            prev.clone_from(&e.mac);
        }
        Ok(entries.len())
    }

    /// Trading so far: notional submitted since UTC midnight, and orders in
    /// the last minute.
    pub fn usage(&self, now_ms: u64) -> Result<Usage> {
        let midnight = now_ms - now_ms % 86_400_000;
        let mut usage = Usage::default();
        for e in self.read()?.iter().filter(|e| e.event == SUBMITTED) {
            if e.ts >= midnight {
                let notional = e
                    .data
                    .get("notional_usd")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<Decimal>().ok())
                    .unwrap_or_default();
                usage.spent_today_usd += notional;
            }
            if e.ts + 60_000 > now_ms {
                usage.trades_last_minute += 1;
            }
        }
        Ok(usage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";
    const DAY: u64 = 86_400_000;

    fn temp_log(name: &str) -> AuditLog {
        let path = std::env::temp_dir()
            .join(format!("onra-audit-{name}-{}", std::process::id()))
            .join("audit.jsonl");
        let _ = fs::remove_file(&path);
        AuditLog::new(path)
    }

    #[test]
    fn chains_and_detects_tampering() {
        let log = temp_log("chain");
        let now = 100 * DAY + 5_000_000;
        log.append(KEY, now, "plan", Some(Initiator::Agent), json!({ "plan": "p1" })).unwrap();
        log.append(KEY, now, SUBMITTED, Some(Initiator::Human), json!({ "notional_usd": "80" }))
            .unwrap();
        log.append(KEY, now, SUBMITTED, None, json!({ "notional_usd": "20.5" })).unwrap();
        assert_eq!(log.verify(KEY).unwrap(), 3);
        assert!(log.verify(b"another key").is_err());

        let usage = log.usage(now + 1_000).unwrap();
        assert_eq!(usage.spent_today_usd, Decimal::new(1005, 1));
        assert_eq!(usage.trades_last_minute, 2);
        // Tomorrow the day starts fresh; a minute later the rate does too.
        assert_eq!(log.usage(now + DAY).unwrap().spent_today_usd, Decimal::ZERO);
        assert_eq!(log.usage(now + 61_000).unwrap().trades_last_minute, 0);

        // Shrinking a trade to hide it breaks the chain.
        let text = fs::read_to_string(&log.path).unwrap();
        fs::write(&log.path, text.replace("\"80\"", "\"8\"")).unwrap();
        let err = log.verify(KEY).unwrap_err();
        assert_eq!(err.hint.unwrap()["seq"], 2);

        // So does deleting a line.
        let lines: Vec<&str> = text.lines().collect();
        fs::write(&log.path, format!("{}\n{}\n", lines[0], lines[2])).unwrap();
        assert!(log.verify(KEY).is_err());
    }
}
