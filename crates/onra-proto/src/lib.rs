//! The JSON envelope every Onra command emits in machine mode.
//!
//! ```json
//! { "ok": true,  "data": { ... },  "meta": { "v": 1, "ts": "...", "latency_ms": 182 } }
//! { "ok": false, "error": { "code": "...", "message": "...", "retryable": false, "hint": { ... } }, "meta": { ... } }
//! ```
//!
//! Bump [`VERSION`] on any breaking change to an envelope or payload shape.

use std::time::{Duration, SystemTime};

use onra_core::Error;
use serde::Serialize;

pub const VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct Meta {
    pub v: u32,
    pub ts: String,
    pub latency_ms: u64,
}

impl Meta {
    pub fn new(elapsed: Duration) -> Self {
        Self {
            v: VERSION,
            ts: humantime::format_rfc3339_seconds(SystemTime::now()).to_string(),
            latency_ms: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Envelope<'a, T: Serialize> {
    Ok { ok: bool, data: &'a T, meta: Meta },
    Err { ok: bool, error: &'a Error, meta: Meta },
}

impl<'a, T: Serialize> Envelope<'a, T> {
    pub fn ok(data: &'a T, meta: Meta) -> Self {
        Envelope::Ok { ok: true, data, meta }
    }
}

impl<'a> Envelope<'a, ()> {
    pub fn err(error: &'a Error, meta: Meta) -> Self {
        Envelope::Err { ok: false, error, meta }
    }
}

/// A venue that failed while others succeeded. Partial results are still
/// useful, so commands return them alongside these instead of failing.
#[derive(Debug, Serialize)]
pub struct Warning {
    pub venue: onra_core::VenueId,
    #[serde(flatten)]
    pub error: Error,
}
