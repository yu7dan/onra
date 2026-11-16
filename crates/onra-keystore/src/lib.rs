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
    entries: BTreeMap<String, Sealed>,
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
        Self { v: 1, salt: hex::encode(salt), entries: BTreeMap::new() }
    }

    fn cipher(&self, passphrase: &str) -> Result<ChaCha20Poly1305> {
        let salt = hex::decode(&self.salt).map_err(|_| corrupt())?;
        let mut key = Zeroizing::new([0u8; 32]);
        Argon2::default()
            .hash_password_into(passphrase.as_bytes(), &salt, key.as_mut())
            .map_err(|e| Error::new(ErrorCode::Internal, format!("key derivation failed: {e}")))?;
        Ok(ChaCha20Poly1305::new(Key::from_slice(key.as_ref())))
    }
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
