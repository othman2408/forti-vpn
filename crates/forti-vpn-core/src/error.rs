//! Error type shared by the CLI driver, the workflow, and the UI.
use std::fmt;

/// A failed VPN operation.
///
/// Messages are fixed, user-facing descriptions. They never contain raw
/// FortiClient output, because a service response can include a password,
/// token, gateway, or account information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VpnError {
    /// An operation failed.
    Failed(String),
    /// The FortiClient executable is missing (setup is required), rather than
    /// an operation failing.
    NotFound(String),
    /// The user cancelled the operation, or a prompt was dismissed.
    Cancelled(String),
}

impl VpnError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::Cancelled(message.into())
    }

    /// The user-facing message.
    pub fn message(&self) -> &str {
        match self {
            Self::Failed(message) | Self::NotFound(message) | Self::Cancelled(message) => message,
        }
    }

    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound(_))
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled(_))
    }
}

impl fmt::Display for VpnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for VpnError {}

/// The message used whenever the FortiClient executable cannot be found.
pub const CLI_NOT_FOUND: &str =
    "FortiClient CLI was not found. Use Set Up FortiClient to install it, then check again.";

/// The message used when a user dismisses a prompt or cancels an operation.
pub const CANCELLED: &str = "Operation cancelled. Checking VPN status…";
