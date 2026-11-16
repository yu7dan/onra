//! Where Onra keeps its files.
//!
//! By default: the platform's config directory for `config.toml`, and its
//! data directory for everything else (vault, policy, audit log, history).
//! `ONRA_HOME` puts all of it in one directory instead, for portable
//! installs and for tests that must not touch a real setup. A different
//! home also gets its own signer, so two homes never share keys.

use std::path::PathBuf;

use onra_core::{Error, ErrorCode, Result};

fn custom() -> Option<PathBuf> {
    std::env::var_os("ONRA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub fn data_dir() -> Result<PathBuf> {
    custom()
        .or_else(|| dirs::data_dir().map(|d| d.join("onra")))
        .ok_or_else(|| Error::new(ErrorCode::Config, "cannot locate the data directory"))
}

pub fn config_dir() -> Result<PathBuf> {
    custom()
        .or_else(|| dirs::config_dir().map(|d| d.join("onra")))
        .ok_or_else(|| Error::new(ErrorCode::Config, "cannot locate the config directory"))
}

/// Tells signers for different homes apart: empty for the default home.
pub fn signer_suffix() -> String {
    match custom() {
        None => String::new(),
        Some(path) => {
            // FNV-1a: stable across runs and platforms, no dependency.
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in path.to_string_lossy().bytes() {
                h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
            format!("-{:08x}", h as u32)
        }
    }
}
