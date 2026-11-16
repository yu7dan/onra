//! The wire between the command line and the signer daemon: one JSON request
//! and one JSON response per connection, each a single line.
//!
//! The endpoint is private to the user. On Windows it is a named pipe whose
//! name carries the user name (the default pipe ACL only lets the owner
//! write). Elsewhere it is a socket file in a directory only the user can
//! enter; Linux abstract sockets are avoided because anyone can connect to
//! them.

use std::io;

use interprocess::local_socket::tokio::{Listener, Stream, prelude::*};
use interprocess::local_socket::{ListenerOptions, Name};
use onra_core::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use zeroize::Zeroizing;

/// Requests and responses are small; anything bigger is not ours.
const MAX_LINE: u64 = 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Status,
    Unlock {
        passphrase: Zeroizing<String>,
        ttl_secs: u64,
    },
    Lock,
    /// Anyone may halt: stopping trading is always safe.
    Halt,
    /// Check the audit chain with the seal key the signer holds.
    Verify,
    /// Price and judge a trade; keeps the plan unless it is denied.
    Plan {
        request: crate::trade::TradeRequest,
        initiator: onra_policy::Initiator,
    },
    /// Carry out a plan.
    Exec {
        plan_id: String,
        initiator: onra_policy::Initiator,
    },
    /// A person approves an agent's plan; the passphrase proves it.
    Approve {
        plan_id: String,
        passphrase: Zeroizing<String>,
    },
    Show {
        plan_id: String,
    },
    Plans,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub pid: u32,
    pub version: String,
    pub unlocked: bool,
    /// Seconds until the daemon locks itself and exits.
    pub expires_in_secs: u64,
    pub keys: Vec<KeyInfo>,
    /// Whether the policy file's seal checks out (only known when unlocked).
    #[serde(default)]
    pub policy_ok: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyInfo {
    /// Vault entry: `bn`, `hl`, `sol`, optionally `.testnet`.
    pub name: String,
    /// Address, or for Binance the API key id. Never secret.
    pub public: String,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    Ok { ok: bool, data: serde_json::Value },
    Err { ok: bool, error: Error },
}

impl Response {
    pub fn ok(data: serde_json::Value) -> Self {
        Response::Ok { ok: true, data }
    }

    pub fn err(error: Error) -> Self {
        Response::Err { ok: false, error }
    }
}

fn ipc_error(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorCode::Internal, format!("signer connection: {e}"))
}

#[cfg(windows)]
fn name() -> io::Result<Name<'static>> {
    use interprocess::local_socket::GenericNamespaced;
    let user: String = std::env::var("USERNAME")
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    format!("onra-signer-{user}{}", crate::home::signer_suffix()).to_ns_name::<GenericNamespaced>()
}

#[cfg(unix)]
fn name() -> io::Result<Name<'static>> {
    use interprocess::local_socket::GenericFilePath;
    use std::os::unix::fs::PermissionsExt;
    let path = socket_path()?;
    let dir = path.parent().expect("socket path has a directory");
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    path.to_fs_name::<GenericFilePath>().map(|n| n.into_owned())
}

/// Binds the endpoint. Fails with `AddrInUse` if a daemon is already there.
pub fn listen() -> io::Result<Listener> {
    let name = name()?;
    match ListenerOptions::new().name(name.clone()).create_tokio() {
        // A socket file left behind by a daemon that died: nobody answers on
        // it, so it is safe to replace.
        #[cfg(unix)]
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            if std::os::unix::net::UnixStream::connect(socket_path()?).is_ok() {
                return Err(e);
            }
            ListenerOptions::new().name(name).try_overwrite(true).create_tokio()
        }
        other => other,
    }
}

/// XDG_RUNTIME_DIR is already private to the user; elsewhere, and for a
/// custom home, the socket goes in a directory we make private.
#[cfg(unix)]
fn socket_path() -> io::Result<std::path::PathBuf> {
    let runtime = dirs::runtime_dir().filter(|_| crate::home::signer_suffix().is_empty());
    let dir = match runtime {
        Some(dir) => dir.join("onra"),
        None => crate::home::data_dir().map_err(|e| io::Error::other(e.message))?.join("run"),
    };
    Ok(dir.join("signer.sock"))
}

/// Connects to a running daemon; `None` if there is none.
pub async fn connect() -> Option<Stream> {
    Stream::connect(name().ok()?).await.ok()
}

pub async fn read_line<T: for<'de> Deserialize<'de>>(stream: &Stream) -> Result<Option<T>> {
    let mut line = Zeroizing::new(String::new());
    let n = BufReader::new(stream.take(MAX_LINE)).read_line(&mut line).await.map_err(ipc_error)?;
    if n == 0 {
        return Ok(None);
    }
    serde_json::from_str(&line).map(Some).map_err(ipc_error)
}

pub async fn write_line<T: Serialize>(stream: &Stream, value: &T) -> Result<()> {
    let mut line = Zeroizing::new(serde_json::to_string(value).map_err(ipc_error)?);
    line.push('\n');
    let mut writer = stream;
    writer.write_all(line.as_bytes()).await.map_err(ipc_error)?;
    writer.flush().await.map_err(ipc_error)
}

/// Sends one request to a running daemon. `None` if no daemon is running.
pub async fn call(request: &Request) -> Result<Option<serde_json::Value>> {
    let Some(stream) = connect().await else { return Ok(None) };
    write_line(&stream, request).await?;
    match read_line::<Response>(&stream).await? {
        Some(Response::Ok { data, .. }) => Ok(Some(data)),
        Some(Response::Err { error, .. }) => Err(error),
        None => Err(ipc_error("the signer closed the connection")),
    }
}

/// [`call`] for the requests that answer with the signer's status.
pub async fn call_status(request: &Request) -> Result<Option<Status>> {
    match call(request).await? {
        Some(data) => serde_json::from_value(data).map(Some).map_err(ipc_error),
        None => Ok(None),
    }
}
