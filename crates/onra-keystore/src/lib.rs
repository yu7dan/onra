//! Secrets for Onra: exchange API secrets now, signing keys later.
//!
//! The system keychain is the default home: Windows Credential Manager,
//! macOS Keychain, the Secret Service on Linux. Where none is available
//! (headless Linux), secrets go to an encrypted file instead: a key derived
//! from a passphrase with Argon2id seals each entry with ChaCha20-Poly1305,
//! bound to the entry's name.
//!
//! Secrets are handed out as [`Secret`], which wipes its memory on drop.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use argon2::Argon2;
use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use onra_core::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const SERVICE: &str = "onra";

pub type Secret = Zeroizing<String>;

/// Asks the person at the keyboard for the file passphrase. Only called
/// when the keychain is unavailable and the file is actually needed.
pub type Passphrase<'a> = &'a dyn Fn() -> Result<Secret>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Keychain,
    EncryptedFile,
}

impl Backend {
    pub fn describe(self) -> &'static str {
        match self {
            Backend::Keychain => "system keychain",
            Backend::EncryptedFile => "encrypted file",
        }
    }
}

pub struct Keystore {
    file: PathBuf,
}

fn keychain_unavailable(e: &keyring::Error) -> bool {
    matches!(e, keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_))
}

fn keychain_error(e: keyring::Error) -> Error {
    Error::new(ErrorCode::Credentials, format!("system keychain: {e}"))
}

impl Keystore {
    /// `file` is where secrets go if no keychain is available.
    pub fn new(file: PathBuf) -> Self {
        Self { file }
    }

    pub fn set(&self, name: &str, secret: &str, passphrase: Passphrase) -> Result<Backend> {
        match keyring::Entry::new(SERVICE, name).and_then(|e| e.set_password(secret)) {
            Ok(()) => Ok(Backend::Keychain),
            Err(e) if keychain_unavailable(&e) => {
                self.file_set(name, secret, &passphrase()?)?;
                Ok(Backend::EncryptedFile)
            }
            Err(e) => Err(keychain_error(e)),
        }
    }

    pub fn get(&self, name: &str, passphrase: Passphrase) -> Result<Option<Secret>> {
        match keyring::Entry::new(SERVICE, name).and_then(|e| e.get_password()) {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring::Error::NoEntry) => self.file_get(name, passphrase),
            Err(e) if keychain_unavailable(&e) => self.file_get(name, passphrase),
            Err(e) => Err(keychain_error(e)),
        }
    }

    /// Remove `name` everywhere it may live. Removing from the file needs no
    /// passphrase: entries are sealed independently.
    pub fn delete(&self, name: &str) -> Result<()> {
        match keyring::Entry::new(SERVICE, name).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) if keychain_unavailable(&e) => {}
            Err(e) => return Err(keychain_error(e)),
        }
        if let Some(mut vault) = self.load()?
            && vault.entries.remove(name).is_some()
        {
            self.save(&vault)?;
        }
        Ok(())
    }

    // -- encrypted file -----------------------------------------------------

    fn load(&self) -> Result<Option<Vault>> {
        match fs::read_to_string(&self.file) {
            Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| {
                Error::new(
                    ErrorCode::Credentials,
                    format!("unreadable keystore {}: {e}", self.file.display()),
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(file_error(&self.file, e)),
        }
    }

    fn save(&self, vault: &Vault) -> Result<()> {
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir).map_err(|e| file_error(dir, e))?;
        }
        let text = serde_json::to_string_pretty(vault).expect("vault serializes");
        fs::write(&self.file, text).map_err(|e| file_error(&self.file, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.file, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    fn file_set(&self, name: &str, secret: &str, passphrase: &str) -> Result<()> {
        let mut vault = self.load()?.unwrap_or_else(Vault::new);
        let cipher = vault.cipher(passphrase)?;
        // A wrong passphrase must not silently start a second key.
        if let Some((existing, sealed)) = vault.entries.iter().next() {
            open(&cipher, existing, sealed)?;
        }
        vault.entries.insert(name.to_string(), seal(&cipher, name, secret));
        self.save(&vault)
    }

    fn file_get(&self, name: &str, passphrase: Passphrase) -> Result<Option<Secret>> {
        let Some(vault) = self.load()? else { return Ok(None) };
        let Some(sealed) = vault.entries.get(name) else { return Ok(None) };
        let cipher = vault.cipher(&passphrase()?)?;
        open(&cipher, name, sealed).map(Some)
    }
}

#[derive(Serialize, Deserialize)]
struct Vault {
    v: u32,
    /// Argon2id salt, hex.
    salt: String,
    /// Argon2id cost; absent means the library defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kdf: Option<Kdf>,
    entries: BTreeMap<String, Sealed>,
}

/// Argon2id parameters: memory in KiB, passes, lanes.
#[derive(Clone, Copy, Serialize, Deserialize)]
struct Kdf {
    m: u32,
    t: u32,
    p: u32,
}

#[derive(Serialize, Deserialize)]
struct Sealed {
    nonce: String,
    data: String,
}

impl Vault {
    fn new() -> Self {
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        Self { v: 1, salt: hex::encode(salt), kdf: None, entries: BTreeMap::new() }
    }

    fn cipher(&self, passphrase: &str) -> Result<ChaCha20Poly1305> {
        let salt = hex::decode(&self.salt).map_err(|_| corrupt())?;
        let mut key = Zeroizing::new([0u8; 32]);
        let argon = match self.kdf {
            None => Argon2::default(),
            Some(k) => Argon2::new(
                argon2::Algorithm::Argon2id,
                argon2::Version::V0x13,
                argon2::Params::new(k.m, k.t, k.p, Some(32)).map_err(|_| corrupt())?,
            ),
        };
        argon
            .hash_password_into(passphrase.as_bytes(), &salt, key.as_mut())
            .map_err(|e| Error::new(ErrorCode::Internal, format!("key derivation failed: {e}")))?;
        Ok(ChaCha20Poly1305::new(Key::from_slice(key.as_ref())))
    }
}

/// Keys that can move money: always sealed with a passphrase, never in the
/// system keychain, where any process running as the user could read them.
/// Only the signer daemon opens this vault.
pub struct TradingVault {
    store: Keystore,
}

/// Sealed at creation so a wrong passphrase is caught even before the vault
/// holds a key, and so one vault never mixes two passphrases.
const CHECK: &str = "_check";

/// Stronger than the default: unlocking happens rarely and guards funds.
const TRADING_KDF: Kdf = Kdf { m: 64 * 1024, t: 3, p: 1 };

impl TradingVault {
    pub fn new(file: PathBuf) -> Self {
        Self { store: Keystore::new(file) }
    }

    pub fn exists(&self) -> Result<bool> {
        Ok(self.store.load()?.is_some())
    }

    /// Entry names, readable without the passphrase.
    pub fn names(&self) -> Result<Vec<String>> {
        let Some(vault) = self.store.load()? else { return Ok(Vec::new()) };
        Ok(vault.entries.into_keys().filter(|n| !n.starts_with('_')).collect())
    }

    /// Adds or replaces an entry, creating the vault on first use.
    pub fn insert(&self, name: &str, secret: &str, passphrase: &str) -> Result<()> {
        let mut vault = match self.store.load()? {
            Some(vault) => vault,
            None => {
                let mut vault = Vault::new();
                vault.kdf = Some(TRADING_KDF);
                let cipher = vault.cipher(passphrase)?;
                vault.entries.insert(CHECK.into(), seal(&cipher, CHECK, CHECK));
                vault
            }
        };
        let cipher = vault.cipher(passphrase)?;
        open_check(&cipher, &vault)?;
        vault.entries.insert(name.to_string(), seal(&cipher, name, secret));
        ensure_seal_key(&cipher, &mut vault);
        self.store.save(&vault)
    }

    /// Opens every entry at once: the daemon holds them until it locks.
    /// Includes [`SEAL_KEY`].
    pub fn unlock(&self, passphrase: &str) -> Result<BTreeMap<String, Secret>> {
        let mut vault = self.store.load()?.ok_or_else(|| {
            Error::new(ErrorCode::Credentials, "no trading keys yet")
                .with_hint(serde_json::json!({ "cause": "no_trading_keys", "suggest": "onra account trade <venue>" }))
        })?;
        let cipher = vault.cipher(passphrase)?;
        open_check(&cipher, &vault)?;
        if ensure_seal_key(&cipher, &mut vault) {
            self.store.save(&vault)?;
        }
        vault
            .entries
            .iter()
            .filter(|(name, _)| *name != CHECK)
            .map(|(name, sealed)| Ok((name.clone(), open(&cipher, name, sealed)?)))
            .collect()
    }

    /// Just the seal key, for changing the policy without opening any
    /// trading key.
    pub fn seal_key(&self, passphrase: &str) -> Result<Secret> {
        let mut vault = self.store.load()?.ok_or_else(|| {
            Error::new(ErrorCode::Credentials, "no trading vault yet")
                .with_hint(serde_json::json!({ "cause": "no_trading_keys", "suggest": "onra account trade <venue>" }))
        })?;
        let cipher = vault.cipher(passphrase)?;
        open_check(&cipher, &vault)?;
        if ensure_seal_key(&cipher, &mut vault) {
            self.store.save(&vault)?;
        }
        open(&cipher, SEAL_KEY, &vault.entries[SEAL_KEY])
    }

    /// Removes an entry. Needs no passphrase: deleting a key never spends it.
    pub fn remove(&self, name: &str) -> Result<bool> {
        let Some(mut vault) = self.store.load()? else { return Ok(false) };
        let found = vault.entries.remove(name).is_some();
        if found {
            self.store.save(&vault)?;
        }
        Ok(found)
    }
}

/// The random key that seals the policy file and the audit log. Created
/// with the vault; only someone with the passphrase can use it.
pub const SEAL_KEY: &str = "_seal";

/// Adds the seal key if the vault lacks one. Returns whether it changed.
fn ensure_seal_key(cipher: &ChaCha20Poly1305, vault: &mut Vault) -> bool {
    if vault.entries.contains_key(SEAL_KEY) {
        return false;
    }
    let mut key = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(key.as_mut());
    let encoded = Zeroizing::new(hex::encode(key.as_ref()));
    vault.entries.insert(SEAL_KEY.into(), seal(cipher, SEAL_KEY, &encoded));
    true
}

fn open_check(cipher: &ChaCha20Poly1305, vault: &Vault) -> Result<()> {
    let sealed = vault.entries.get(CHECK).ok_or_else(corrupt)?;
    open(cipher, CHECK, sealed).map(drop)
}

fn seal(cipher: &ChaCha20Poly1305, name: &str, secret: &str) -> Sealed {
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let data = cipher
        .encrypt(&nonce, Payload { msg: secret.as_bytes(), aad: name.as_bytes() })
        .expect("encryption with a valid key cannot fail");
    Sealed { nonce: hex::encode(nonce), data: hex::encode(data) }
}

fn open(cipher: &ChaCha20Poly1305, name: &str, sealed: &Sealed) -> Result<Secret> {
    let nonce = hex::decode(&sealed.nonce).map_err(|_| corrupt())?;
    let data = hex::decode(&sealed.data).map_err(|_| corrupt())?;
    if nonce.len() != 12 {
        return Err(corrupt());
    }
    let plain = cipher
        .decrypt(Nonce::from_slice(&nonce), Payload { msg: &data, aad: name.as_bytes() })
        .map_err(|_| Error::new(ErrorCode::Credentials, "wrong keystore passphrase"))?;
    String::from_utf8(plain).map(Zeroizing::new).map_err(|_| corrupt())
}

fn corrupt() -> Error {
    Error::new(ErrorCode::Credentials, "keystore file is corrupted")
}

fn file_error(path: &std::path::Path, e: std::io::Error) -> Error {
    Error::new(ErrorCode::Credentials, format!("cannot access {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Keystore {
        let dir = std::env::temp_dir().join(format!("onra-keystore-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Keystore::new(dir.join("keystore.json"))
    }

    #[test]
    fn file_roundtrip_and_wrong_passphrase() {
        let store = temp_store();
        store.file_set("binance", "s3cret", "correct horse").unwrap();
        store.file_set("other", "x", "correct horse").unwrap();
        let ok: Passphrase = &|| Ok(Zeroizing::new("correct horse".into()));
        let bad: Passphrase = &|| Ok(Zeroizing::new("wrong".into()));
        assert_eq!(store.file_get("binance", ok).unwrap().unwrap().as_str(), "s3cret");
        assert_eq!(store.file_get("binance", bad).unwrap_err().code, ErrorCode::Credentials);
        assert!(store.file_set("third", "y", "wrong").is_err(), "must not mix passphrases");
        assert!(store.file_get("missing", ok).unwrap().is_none());

        // Entries are bound to their names: swapping ciphertexts fails.
        let mut vault = store.load().unwrap().unwrap();
        let moved = vault.entries.remove("other").unwrap();
        vault.entries.insert("binance".into(), moved);
        store.save(&vault).unwrap();
        assert!(store.file_get("binance", ok).is_err());

        let text = fs::read_to_string(&store.file).unwrap();
        assert!(!text.contains("s3cret"));
    }

    #[test]
    fn trading_vault() {
        let dir = std::env::temp_dir().join(format!("onra-vault-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let vault = TradingVault::new(dir.join("trading.json"));
        assert!(!vault.exists().unwrap());
        assert_eq!(vault.unlock("pw").unwrap_err().code, ErrorCode::Credentials);

        vault.insert("hl", "0xabc", "pw").unwrap();
        vault.insert("sol", "xyz", "pw").unwrap();
        assert!(vault.insert("bn", "k", "other").is_err(), "one passphrase per vault");
        assert_eq!(vault.names().unwrap(), ["hl", "sol"]);

        let keys = vault.unlock("pw").unwrap();
        assert_eq!(keys["hl"].as_str(), "0xabc");
        let seal = vault.seal_key("pw").unwrap();
        assert_eq!(seal.len(), 64);
        assert_eq!(keys[SEAL_KEY], seal, "one seal key, stable across opens");
        assert!(vault.seal_key("nope").is_err());
        assert!(vault.unlock("nope").is_err());

        assert!(vault.remove("hl").unwrap());
        assert!(!vault.remove("hl").unwrap());
        assert_eq!(vault.names().unwrap(), ["sol"]);
        let text = fs::read_to_string(dir.join("trading.json")).unwrap();
        assert!(text.contains("\"kdf\"") && !text.contains("xyz"));
    }
}

#[cfg(test)]
mod keychain_tests {
    use super::*;

    /// Touches the real system keychain; run with `--ignored`.
    #[test]
    #[ignore]
    fn keychain_roundtrip() {
        let store = Keystore::new(std::env::temp_dir().join("onra-unused.json"));
        let never: Passphrase = &|| panic!("keychain should be used");
        assert_eq!(store.set("selftest", "hello", never).unwrap(), Backend::Keychain);
        assert_eq!(store.get("selftest", never).unwrap().unwrap().as_str(), "hello");
        store.delete("selftest").unwrap();
        assert!(store.get("selftest", never).unwrap().is_none());
    }
}
