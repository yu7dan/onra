use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Stable error codes. These are part of the agent contract: never rename a
/// variant once released, only add new ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// Invalid arguments or syntax.
    Usage,
    /// The `@venue` suffix names a venue Onra doesn't know.
    UnknownVenue,
    /// No venue lists the requested market.
    MarketNotFound,
    /// Connection failed or timed out. Usually retryable.
    Network,
    /// The venue is throttling requests. Retry after a pause.
    RateLimited,
    /// The venue answered, but with an error.
    Venue,
    /// The config file is unreadable or invalid.
    Config,
    /// An account is not linked, or its credentials are missing or rejected.
    Credentials,
    /// A bug in Onra.
    Internal,
    /// The trading policy blocks this, or its file or the audit log was
    /// tampered with.
    Policy,
    /// The trade waits for a person to approve it (`onra approve <plan>`).
    ApprovalRequired,
    /// The plan is older than its validity window; make a new one.
    PlanExpired,
    /// The signer is locked; a person must run `onra unlock`.
    Locked,
}

impl ErrorCode {
    /// Process exit code for this error class. See the exit code table in the
    /// README; agents branch on these.
    pub fn exit_code(self) -> u8 {
        match self {
            ErrorCode::Usage | ErrorCode::UnknownVenue => 2,
            ErrorCode::MarketNotFound | ErrorCode::Venue => 5,
            ErrorCode::Network | ErrorCode::RateLimited => 6,
            ErrorCode::Config
            | ErrorCode::Credentials
            | ErrorCode::Internal
            | ErrorCode::Locked => 1,
            ErrorCode::Policy => 3,
            ErrorCode::ApprovalRequired => 4,
            ErrorCode::PlanExpired => 7,
        }
    }

    pub fn retryable(self) -> bool {
        matches!(self, ErrorCode::Network | ErrorCode::RateLimited)
    }
}

/// An error with everything an agent needs to recover: a stable code, a
/// human-readable message, and an optional machine-readable hint.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[error("{message}")]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<Value>,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), retryable: code.retryable(), hint: None }
    }

    pub fn with_hint(mut self, hint: Value) -> Self {
        self.hint = Some(hint);
        self
    }

    /// The `suggest` field of the hint, if any: a command the user or agent
    /// can run next.
    pub fn suggestion(&self) -> Option<&str> {
        self.hint.as_ref()?.get("suggest")?.as_str()
    }
}
