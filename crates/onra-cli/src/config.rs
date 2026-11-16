//! `config.toml`: user settings. Every key is optional; absent keys fall back
//! to defaults, so an empty or missing file is valid.

use std::path::PathBuf;
use std::time::Duration;
use std::{env, fs};

use onra_core::{Error, ErrorCode, Result};
use onra_ui::Convention;

use crate::i18n::Lang;
use onra_venues::NetConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const KEYS: [&str; 4] =
    ["network.proxy", "network.timeout_ms", "ui.color_convention", "ui.language"];
const DEFAULT_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    #[serde(skip_serializing_if = "Network::is_empty")]
    network: Network,
    #[serde(skip_serializing_if = "Ui::is_empty")]
    ui: Ui,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Network {
    #[serde(skip_serializing_if = "Option::is_none")]
    proxy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Ui {
    #[serde(skip_serializing_if = "Option::is_none")]
    color_convention: Option<ConventionSetting>,
    /// `en`, `zh`, or absent for the system language.
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
}

impl Network {
    fn is_empty(&self) -> bool {
        self.proxy.is_none() && self.timeout_ms.is_none()
    }
}

impl Ui {
    fn is_empty(&self) -> bool {
        self.color_convention.is_none() && self.language.is_none()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ConventionSetting {
    East,
    West,
}

fn config_error(message: String) -> Error {
    Error::new(ErrorCode::Config, message)
}

pub fn path() -> Result<PathBuf> {
    if let Some(p) = env::var_os("ONRA_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    dirs::config_dir().map(|d| d.join("onra").join("config.toml")).ok_or_else(|| {
        config_error("cannot determine the config directory; set ONRA_CONFIG".into())
    })
}

pub fn load() -> Result<Config> {
    let path = path()?;
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(|e| {
            config_error(format!("invalid config at {}: {}", path.display(), e.message()))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(config_error(format!("cannot read {}: {e}", path.display()))),
    }
}

impl Config {
    pub fn save(&self) -> Result<PathBuf> {
        let path = path()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .map_err(|e| config_error(format!("cannot create {}: {e}", dir.display())))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| {
            Error::new(ErrorCode::Internal, format!("cannot serialize config: {e}"))
        })?;
        fs::write(&path, text)
            .map_err(|e| config_error(format!("cannot write {}: {e}", path.display())))?;
        Ok(path)
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let invalid = |expected: &str| {
            Error::new(
                ErrorCode::Usage,
                format!("invalid value `{value}` for {key}: expected {expected}"),
            )
        };
        match key {
            "network.proxy" => self.network.proxy = Some(value.to_string()),
            "network.timeout_ms" => {
                let ms = value
                    .parse()
                    .ok()
                    .filter(|&ms| ms > 0)
                    .ok_or_else(|| invalid("milliseconds > 0"))?;
                self.network.timeout_ms = Some(ms);
            }
            "ui.color_convention" => {
                self.ui.color_convention = Some(match value {
                    "east" => ConventionSetting::East,
                    "west" => ConventionSetting::West,
                    _ => return Err(invalid("`east` (red up) or `west` (green up)")),
                });
            }
            "ui.language" => match value {
                "auto" => self.ui.language = None,
                v if Lang::from_tag(v).is_some() => self.ui.language = Some(v.to_ascii_lowercase()),
                _ => return Err(invalid("`en`, `zh` or `auto`")),
            },
            _ => return Err(unknown_key(key)),
        }
        Ok(())
    }

    pub fn unset(&mut self, key: &str) -> Result<()> {
        match key {
            "network.proxy" => self.network.proxy = None,
            "network.timeout_ms" => self.network.timeout_ms = None,
            "ui.color_convention" => self.ui.color_convention = None,
            "ui.language" => self.ui.language = None,
            _ => return Err(unknown_key(key)),
        }
        Ok(())
    }

    /// Up/down colors. Defaults from the system locale: Chinese, Japanese
    /// and Korean users expect red for gains.
    pub fn convention(&self) -> Convention {
        match self.ui.color_convention {
            Some(ConventionSetting::East) => Convention::East,
            Some(ConventionSetting::West) => Convention::West,
            None => {
                let locale = sys_locale::get_locale().unwrap_or_default();
                if ["zh", "ja", "ko"].iter().any(|l| locale.starts_with(l)) {
                    Convention::East
                } else {
                    Convention::West
                }
            }
        }
    }

    /// Interface language: `ONRA_LANG`, then `ui.language`, then the system.
    pub fn language(&self) -> Lang {
        env::var("ONRA_LANG")
            .ok()
            .and_then(|l| Lang::from_tag(&l))
            .or_else(|| self.ui.language.as_deref().and_then(Lang::from_tag))
            .unwrap_or_else(Lang::system)
    }

    /// Proxy precedence: `--proxy` flag, then `ONRA_PROXY`, then the config
    /// file. With none of those, the standard proxy env vars still apply.
    pub fn net(&self, flag: Option<String>) -> NetConfig {
        let proxy = flag
            .or_else(|| env::var("ONRA_PROXY").ok().filter(|p| !p.is_empty()))
            .or_else(|| self.network.proxy.clone());
        let ms = self.network.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        NetConfig { proxy, timeout: Duration::from_millis(ms) }
    }

    /// Every key with its effective value, for `config show`.
    pub fn effective(&self) -> Vec<(&'static str, Value, bool)> {
        let convention = match self.convention() {
            Convention::East => "east",
            Convention::West => "west",
        };
        vec![
            ("network.proxy", json!(self.network.proxy), self.network.proxy.is_none()),
            (
                "network.timeout_ms",
                json!(self.network.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
                self.network.timeout_ms.is_none(),
            ),
            ("ui.color_convention", json!(convention), self.ui.color_convention.is_none()),
            (
                "ui.language",
                json!(match self.language() {
                    Lang::En => "en",
                    Lang::Zh => "zh",
                }),
                self.ui.language.is_none(),
            ),
        ]
    }
}

fn unknown_key(key: &str) -> Error {
    Error::new(ErrorCode::Usage, format!("unknown config key `{key}`"))
        .with_hint(json!({ "keys": KEYS }))
}
