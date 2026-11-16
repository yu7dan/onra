use std::str::FromStr;
use std::time::Duration;

use onra_core::{Error, ErrorCode, Result, VenueId};
use reqwest::{Client, Proxy};
use rust_decimal::Decimal;
use serde_json::json;

#[derive(Debug, Clone)]
pub struct NetConfig {
    /// Explicit proxy URL (`http://`, `https://` or `socks5://`). When unset,
    /// the standard `HTTPS_PROXY` / `ALL_PROXY` environment variables apply.
    pub proxy: Option<String>,
    pub timeout: Duration,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self { proxy: None, timeout: Duration::from_secs(10) }
    }
}

pub(crate) fn client(net: &NetConfig) -> Result<Client> {
    let mut builder = Client::builder()
        .user_agent(concat!("onra/", env!("CARGO_PKG_VERSION")))
        .timeout(net.timeout)
        .connect_timeout(net.timeout.min(Duration::from_secs(5)));
    if let Some(url) = &net.proxy {
        let proxy = Proxy::all(url)
            .map_err(|e| Error::new(ErrorCode::Config, format!("invalid proxy `{url}`: {e}")))?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|e| Error::new(ErrorCode::Internal, format!("failed to build HTTP client: {e}")))
}

/// Map a transport failure to an Onra error. Unreachable venues are the most
/// common failure in practice (regional blocks), so the hint points at the
/// proxy setting.
pub(crate) fn transport_error(venue: VenueId, e: reqwest::Error) -> Error {
    if e.is_decode() {
        return unexpected(venue, e);
    }
    let reason = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    Error::new(ErrorCode::Network, format!("could not reach {venue} ({reason})")).with_hint(json!({
        "suggest": "onra config set network.proxy <url>",
        "reason": "if your network blocks this venue, route Onra through a proxy",
    }))
}

/// Map a non-success HTTP status. Throttling gets its own code so callers
/// can back off instead of treating it as a failure.
pub(crate) fn status_error(venue: VenueId, status: reqwest::StatusCode) -> Error {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Error::new(ErrorCode::RateLimited, format!("{venue} is rate limiting requests"));
    }
    unexpected(venue, format!("HTTP {status}"))
}

pub(crate) fn unexpected(venue: VenueId, detail: impl std::fmt::Display) -> Error {
    Error::new(ErrorCode::Venue, format!("{venue} returned an unexpected response: {detail}"))
}

pub(crate) fn decimal(venue: VenueId, s: &str) -> Result<Decimal> {
    Decimal::from_str(s)
        .map(|d| d.normalize())
        .map_err(|_| unexpected(venue, format!("bad number `{s}`")))
}

/// f64 → Decimal via the shortest round-trip representation, so 71.33 stays
/// 71.33 instead of 71.3299999999999982946974341757595539093017578125.
pub(crate) fn decimal_f64(x: f64) -> Option<Decimal> {
    Decimal::from_str(&x.to_string()).ok()
}

/// Percent change from `prev` to `now`, or `None` if `prev` is zero.
pub(crate) fn pct_change(prev: Decimal, now: Decimal) -> Option<Decimal> {
    if prev.is_zero() {
        return None;
    }
    Some(((now - prev) / prev * Decimal::ONE_HUNDRED).round_dp(4))
}
