//! The rules every trade must pass, and the record of every trade tried.
//!
//! A policy is a handful of hard limits. The signer evaluates it before it
//! signs anything; nothing on the command line can skip that step. Limits
//! bind everyone. On top of them, trades started by an agent wait for a
//! person's approval unless the policy says otherwise.
//!
//! The policy file is sealed: it carries an HMAC made with a key from the
//! trading vault, so editing it by hand (or by an agent with file access)
//! makes the signer refuse to trade instead of trading under new rules.

pub mod audit;

use hmac::{Hmac, Mac};
use onra_core::{Error, ErrorCode, Result, VenueId};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    /// Largest single trade, in USD notional.
    pub max_trade_usd: Decimal,
    /// Total notional per UTC day.
    pub max_daily_usd: Decimal,
    /// Highest leverage a perp order may use.
    pub max_leverage: Decimal,
    /// Widest slippage a swap or market order may accept, in percent.
    pub max_slippage_pct: Decimal,
    /// Only assets a venue verifies (no lookalike tokens).
    pub verified_only: bool,
    /// Venues trading is allowed on: `bn`, `hl`, `jup`.
    pub venues: Vec<String>,
    /// Every trade an agent starts waits for a person to approve it.
    pub agent_approval: bool,
    /// A brake on runaway loops.
    pub max_trades_per_minute: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_trade_usd: Decimal::from(100),
            max_daily_usd: Decimal::from(500),
            max_leverage: Decimal::from(3),
            max_slippage_pct: Decimal::from(1),
            verified_only: true,
            venues: vec!["bn".into(), "hl".into(), "jup".into()],
            agent_approval: true,
            max_trades_per_minute: 10,
        }
    }
}

/// Keys accepted by `onra policy set`, in display order.
pub const KEYS: [&str; 8] = [
    "max_trade_usd",
    "max_daily_usd",
    "max_leverage",
    "max_slippage_pct",
    "verified_only",
    "venues",
    "agent_approval",
    "max_trades_per_minute",
];

const HEADER: &str = "# Onra trading policy. Change it with `onra policy set`: the seal below\n# is checked before every trade, and a hand-edited file stops all trading.\n";
const SEAL: &str = "# seal: ";

fn usage(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::Usage, message)
}

impl Policy {
    /// Applies `onra policy set <key> <value>`.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let decimal = || -> Result<Decimal> {
            let v: Decimal = value
                .trim_start_matches('$')
                .trim_end_matches('%')
                .trim_end_matches('x')
                .parse()
                .map_err(|_| usage(format!("`{value}` is not a number")))?;
            if v.is_sign_negative() {
                return Err(usage("limits cannot be negative"));
            }
            Ok(v)
        };
        let boolean = || match value {
            "true" | "on" | "yes" => Ok(true),
            "false" | "off" | "no" => Ok(false),
            _ => Err(usage(format!("`{value}` is not true or false"))),
        };
        match key {
            "max_trade_usd" => self.max_trade_usd = decimal()?,
            "max_daily_usd" => self.max_daily_usd = decimal()?,
            "max_leverage" => self.max_leverage = decimal()?,
            "max_slippage_pct" => self.max_slippage_pct = decimal()?,
            "verified_only" => self.verified_only = boolean()?,
            "agent_approval" => self.agent_approval = boolean()?,
            "max_trades_per_minute" => {
                self.max_trades_per_minute =
                    value.parse().map_err(|_| usage(format!("`{value}` is not a whole number")))?
            }
            "venues" => {
                let mut venues = Vec::new();
                for v in value.split(',').map(str::trim).filter(|v| !v.is_empty()) {
                    let id: VenueId = v.parse()?;
                    venues.push(venue_code(id).to_string());
                }
                self.venues = venues;
            }
            _ => {
                return Err(usage(format!("unknown policy key `{key}`"))
                    .with_hint(serde_json::json!({ "keys": KEYS })));
            }
        }
        Ok(())
    }

    /// The file contents: header, seal, then the TOML body the seal covers.
    pub fn to_sealed(&self, key: &[u8]) -> String {
        let body = toml::to_string(self).expect("policy serializes");
        format!("{HEADER}{SEAL}{}\n\n{body}", hex::encode(mac(key, &body)))
    }

    /// Parses a sealed file, refusing it if the seal doesn't match.
    pub fn from_sealed(text: &str, key: &[u8]) -> Result<Self> {
        let tampered = || {
            Error::new(
                ErrorCode::Policy,
                "the policy file was changed outside `onra policy set`; trading is stopped",
            )
            .with_hint(serde_json::json!({ "cause": "tampered", "suggest": "onra policy reset" }))
        };
        let (seal, body) = parse_sealed(text).ok_or_else(tampered)?;
        let expected = hex::decode(seal).map_err(|_| tampered())?;
        let mut m = HmacSha256::new_from_slice(key).expect("any key length works");
        m.update(body.as_bytes());
        m.verify_slice(&expected).map_err(|_| tampered())?;
        toml::from_str(body).map_err(|_| tampered())
    }

    /// Reads the body without checking the seal: for display only.
    pub fn from_file_unchecked(text: &str) -> Result<Self> {
        let body = parse_sealed(text).map(|(_, b)| b).unwrap_or(text);
        toml::from_str(body)
            .map_err(|e| Error::new(ErrorCode::Config, format!("unreadable policy file: {e}")))
    }
}

fn parse_sealed(text: &str) -> Option<(&str, &str)> {
    let start = text.find(SEAL)? + SEAL.len();
    let end = start + text[start..].find('\n')?;
    let body = text[end + 1..].strip_prefix('\n')?;
    Some((text[start..end].trim(), body))
}

fn mac(key: &[u8], body: &str) -> Vec<u8> {
    let mut m = HmacSha256::new_from_slice(key).expect("any key length works");
    m.update(body.as_bytes());
    m.finalize().into_bytes().to_vec()
}

pub fn venue_code(id: VenueId) -> &'static str {
    match id {
        VenueId::Binance => "bn",
        VenueId::Hyperliquid => "hl",
        VenueId::Jupiter => "jup",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Initiator {
    /// Someone at a terminal, confirming on screen.
    Human,
    /// Anything else: a script, a pipe, an AI agent.
    Agent,
}

/// What a trade would do, as far as the rules are concerned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intent {
    pub venue: VenueId,
    pub notional_usd: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leverage: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slippage_pct: Option<Decimal>,
    pub verified: bool,
    /// Only shrinks an existing position. Always allowed past size and
    /// daily limits: getting out must never be blocked.
    pub reduce_only: bool,
    pub initiator: Initiator,
}

/// What has already happened, from the audit log.
#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub spent_today_usd: Decimal,
    pub trades_last_minute: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    NeedsApproval,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub rule: String,
    pub passed: bool,
    /// The value being judged and the limit, as display strings.
    pub value: String,
    pub limit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub decision: Decision,
    pub checks: Vec<Check>,
}

impl Verdict {
    pub fn failed(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| !c.passed)
    }
}

fn usd(v: Decimal) -> String {
    format!("${}", v.round_dp(2).normalize())
}

fn check(rule: &str, passed: bool, value: String, limit: String) -> Check {
    Check { rule: rule.into(), passed, value, limit }
}

/// Judges one trade. `halted` is `onra halt`: it stops every agent trade.
pub fn evaluate(policy: &Policy, intent: &Intent, usage: &Usage, halted: bool) -> Verdict {
    let mut checks = Vec::new();
    let agent = intent.initiator == Initiator::Agent;
    if agent {
        checks.push(check(
            "halt",
            !halted,
            if halted { "halted" } else { "running" }.into(),
            "running".into(),
        ));
    }
    let code = venue_code(intent.venue);
    checks.push(check(
        "venue",
        policy.venues.iter().any(|v| v == code),
        code.into(),
        policy.venues.join(","),
    ));
    if policy.verified_only {
        checks.push(check(
            "verified",
            intent.verified,
            if intent.verified { "verified" } else { "unverified" }.into(),
            "verified".into(),
        ));
    }
    if !intent.reduce_only {
        checks.push(check(
            "max_trade_usd",
            intent.notional_usd <= policy.max_trade_usd,
            usd(intent.notional_usd),
            usd(policy.max_trade_usd),
        ));
        let after = usage.spent_today_usd + intent.notional_usd;
        checks.push(check(
            "max_daily_usd",
            after <= policy.max_daily_usd,
            usd(after),
            usd(policy.max_daily_usd),
        ));
        if let Some(lev) = intent.leverage {
            checks.push(check(
                "max_leverage",
                lev <= policy.max_leverage,
                format!("{}x", lev.normalize()),
                format!("{}x", policy.max_leverage.normalize()),
            ));
        }
    }
    if let Some(slip) = intent.slippage_pct {
        checks.push(check(
            "max_slippage_pct",
            slip <= policy.max_slippage_pct,
            format!("{}%", slip.normalize()),
            format!("{}%", policy.max_slippage_pct.normalize()),
        ));
    }
    let rate = usage.trades_last_minute + 1;
    checks.push(check(
        "max_trades_per_minute",
        rate <= policy.max_trades_per_minute,
        rate.to_string(),
        policy.max_trades_per_minute.to_string(),
    ));

    let decision = if checks.iter().any(|c| !c.passed) {
        Decision::Deny
    } else if agent && policy.agent_approval {
        Decision::NeedsApproval
    } else {
        Decision::Allow
    };
    Verdict { decision, checks }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(notional: i64, initiator: Initiator) -> Intent {
        Intent {
            venue: VenueId::Binance,
            notional_usd: Decimal::from(notional),
            leverage: None,
            slippage_pct: Some(Decimal::new(5, 1)),
            verified: true,
            reduce_only: false,
            initiator,
        }
    }

    #[test]
    fn defaults_allow_small_human_trades() {
        let v =
            evaluate(&Policy::default(), &intent(80, Initiator::Human), &Usage::default(), false);
        assert_eq!(v.decision, Decision::Allow);
    }

    #[test]
    fn agents_wait_for_approval_and_stop_on_halt() {
        let p = Policy::default();
        let i = intent(80, Initiator::Agent);
        assert_eq!(evaluate(&p, &i, &Usage::default(), false).decision, Decision::NeedsApproval);
        let halted = evaluate(&p, &i, &Usage::default(), true);
        assert_eq!(halted.decision, Decision::Deny);
        assert_eq!(halted.failed().next().unwrap().rule, "halt");
        // Halt is about agents; a person can still trade (and get out).
        assert_eq!(
            evaluate(&p, &intent(80, Initiator::Human), &Usage::default(), true).decision,
            Decision::Allow
        );
    }

    #[test]
    fn limits_bind_everyone_but_exits_stay_open() {
        let p = Policy::default();
        let big = evaluate(&p, &intent(150, Initiator::Human), &Usage::default(), false);
        assert_eq!(big.decision, Decision::Deny);
        assert_eq!(big.failed().map(|c| c.rule.as_str()).collect::<Vec<_>>(), ["max_trade_usd"]);

        let spent = Usage { spent_today_usd: Decimal::from(450), trades_last_minute: 0 };
        let daily = evaluate(&p, &intent(80, Initiator::Human), &spent, false);
        assert_eq!(daily.failed().next().unwrap().value, "$530");

        let mut exit = intent(5000, Initiator::Human);
        exit.reduce_only = true;
        assert_eq!(evaluate(&p, &exit, &spent, false).decision, Decision::Allow);

        let mut lev = intent(50, Initiator::Human);
        lev.leverage = Some(Decimal::from(5));
        assert_eq!(evaluate(&p, &lev, &Usage::default(), false).decision, Decision::Deny);

        let mut fake = intent(50, Initiator::Human);
        fake.verified = false;
        assert_eq!(evaluate(&p, &fake, &Usage::default(), false).decision, Decision::Deny);

        let busy = Usage { spent_today_usd: Decimal::ZERO, trades_last_minute: 10 };
        assert_eq!(
            evaluate(&p, &intent(5, Initiator::Human), &busy, false).decision,
            Decision::Deny
        );
    }

    #[test]
    fn sealed_files_detect_edits() {
        let key = [7u8; 32];
        let mut p = Policy::default();
        p.set("max_trade_usd", "$250").unwrap();
        p.set("venues", "hl, bn").unwrap();
        assert!(p.set("max_leverage", "-1").is_err());
        assert!(p.set("nope", "1").is_err());

        let text = p.to_sealed(&key);
        assert_eq!(Policy::from_sealed(&text, &key).unwrap(), p);
        assert_eq!(Policy::from_file_unchecked(&text).unwrap(), p);

        let edited = text.replace("\"250\"", "\"99999\"");
        assert_eq!(Policy::from_sealed(&edited, &key).unwrap_err().code, ErrorCode::Policy);
        assert!(Policy::from_sealed(&text, &[8u8; 32]).is_err(), "another vault's key");
    }
}
