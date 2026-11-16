//! Where the policy, the halt switch and the audit log live, and how the
//! signer and the command line read them.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use onra_core::{Error, ErrorCode, Result};
use onra_policy::Policy;
use onra_policy::audit::AuditLog;
use zeroize::Zeroizing;

pub const HALT: &str = "halt";
pub const RESUME: &str = "resume";

pub fn data_dir() -> Result<PathBuf> {
    crate::home::data_dir()
}

pub fn policy_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("policy.toml"))
}

fn halt_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("HALT"))
}

pub fn audit_log() -> Result<AuditLog> {
    Ok(AuditLog::new(data_dir()?.join("audit.jsonl")))
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The vault stores the seal key as hex; HMAC wants the bytes.
pub fn seal_bytes(hex_key: &str) -> Result<Zeroizing<Vec<u8>>> {
    hex::decode(hex_key)
        .map(Zeroizing::new)
        .map_err(|_| Error::new(ErrorCode::Credentials, "the vault's seal key is corrupted"))
}

/// The policy in force, with its seal checked. No file means the defaults.
pub fn load_policy(seal: &[u8]) -> Result<Policy> {
    match std::fs::read_to_string(policy_path()?) {
        Ok(text) => Policy::from_sealed(&text, seal),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Policy::default()),
        Err(e) => Err(Error::new(ErrorCode::Config, format!("cannot read the policy: {e}"))),
    }
}

/// The policy for display, without the key. `None` means the defaults.
pub fn peek_policy() -> Result<Option<Policy>> {
    match std::fs::read_to_string(policy_path()?) {
        Ok(text) => Policy::from_file_unchecked(&text).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::new(ErrorCode::Config, format!("cannot read the policy: {e}"))),
    }
}

pub fn save_policy(policy: &Policy, seal: &[u8]) -> Result<()> {
    let path = policy_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::new(ErrorCode::Config, format!("cannot write the policy: {e}")))?;
    }
    std::fs::write(&path, policy.to_sealed(seal))
        .map_err(|e| Error::new(ErrorCode::Config, format!("cannot write the policy: {e}")))
}

/// Halted if the switch file is there, or if the sealed log says the last
/// word was a halt: deleting the file alone does not resume trading.
pub fn halted() -> Result<bool> {
    if halt_path()?.exists() {
        return Ok(true);
    }
    let last =
        audit_log()?.read()?.into_iter().rev().find(|e| e.event == HALT || e.event == RESUME);
    Ok(last.is_some_and(|e| e.event == HALT))
}

pub fn set_halt_file(on: bool) -> Result<()> {
    let path = halt_path()?;
    let result = if on {
        std::fs::create_dir_all(data_dir()?).and_then(|_| std::fs::write(&path, b"halted\n"))
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    };
    result.map_err(|e| Error::new(ErrorCode::Internal, format!("cannot set the halt switch: {e}")))
}
