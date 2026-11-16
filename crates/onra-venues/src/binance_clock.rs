//! Binance's clock, as seen from here.
//!
//! Signed requests carry a timestamp, and Binance refuses any that is more
//! than a second ahead of its own clock. Plenty of machines drift that far,
//! so timestamps are taken from the server's clock: one request to
//! `/api/v3/time` measures the offset, which is reused for ten minutes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use onra_core::{Result, VenueId};
use reqwest::Client;
use serde::Deserialize;

use crate::http::transport_error;

const FRESH: Duration = Duration::from_secs(600);

/// Offset (server minus local, ms) per API host, and when it was measured.
static OFFSETS: Mutex<Option<HashMap<&'static str, (i64, Instant)>>> = Mutex::new(None);

fn local_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// A timestamp for a signed request to `host`, on the server's clock.
pub(crate) async fn timestamp(http: &Client, host: &'static str) -> Result<i64> {
    let cached = OFFSETS
        .lock()
        .expect("clock lock")
        .as_ref()
        .and_then(|m| m.get(host).copied())
        .filter(|(_, at)| at.elapsed() < FRESH);
    let offset = match cached {
        Some((offset, _)) => offset,
        None => {
            let offset = measure(http, host).await?;
            OFFSETS
                .lock()
                .expect("clock lock")
                .get_or_insert_with(HashMap::new)
                .insert(host, (offset, Instant::now()));
            offset
        }
    };
    Ok(local_ms() + offset)
}

async fn measure(http: &Client, host: &'static str) -> Result<i64> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Time {
        server_time: i64,
    }
    let before = local_ms();
    let t: Time = http
        .get(format!("{host}/api/v3/time"))
        .send()
        .await
        .map_err(|e| transport_error(VenueId::Binance, e))?
        .json()
        .await
        .map_err(|e| transport_error(VenueId::Binance, e))?;
    let after = local_ms();
    // The server read its clock somewhere in the round trip; assume halfway.
    Ok(t.server_time - (before + after) / 2)
}
