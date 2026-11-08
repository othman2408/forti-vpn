//! The operations the workflow needs from FortiClient.
//!
//! [`crate::cli::Cli`] implements this trait by driving the real
//! `forticlient` executable; tests and demo mode use in-memory fakes.
use std::time::{Duration, Instant};

use crate::cancel::CancelToken;
use crate::error::VpnError;
use crate::model::{ClientInfo, Profile, ProfileSettings, PromptKind, Secret, Status};

/// Asks the user for a prompt answer. `Ok(None)` means the prompt was
/// dismissed. Called on the worker thread; it blocks until the user answers,
/// the operation is cancelled, or the deadline passes.
pub type Ask<'a> = dyn Fn(PromptKind) -> Result<Option<Secret>, VpnError> + Sync + 'a;

/// Records a sanitized diagnostics line. A line starting with `Warning: `
/// is also shown to the user as a non-fatal warning.
pub type Log<'a> = dyn Fn(&str) + Sync + 'a;

/// Everything an interactive operation needs besides its own arguments.
pub struct OperationContext<'a> {
    pub cancel: &'a CancelToken,
    /// The operation fails with a timeout after this instant.
    pub deadline: Instant,
    pub ask: &'a Ask<'a>,
    pub log: &'a Log<'a>,
}

pub trait Backend: Send + Sync + 'static {
    /// How long an interactive operation (connect, edit) may take overall.
    fn interactive_timeout(&self) -> Duration;

    fn status(&self) -> Result<Status, VpnError>;

    fn list_profiles(&self) -> Result<Vec<Profile>, VpnError>;

    /// Read a profile's details. The result keeps `profile.managed`.
    fn view(&self, profile: &Profile) -> Result<Profile, VpnError>;

    /// Connect and confirm the `connected` state.
    fn connect(
        &self,
        profile: &Profile,
        username: &str,
        remember: bool,
        context: &OperationContext<'_>,
    ) -> Result<Status, VpnError>;

    /// Disconnect and confirm the `disconnected` state.
    fn disconnect(&self) -> Result<Status, VpnError>;

    /// Create (`existing` is `None`) or edit a profile, then verify what
    /// FortiClient saved.
    fn save_profile(
        &self,
        settings: &ProfileSettings,
        existing: Option<&Profile>,
        context: &OperationContext<'_>,
    ) -> Result<Profile, VpnError>;

    fn remove(&self, profile: &Profile) -> Result<(), VpnError>;

    /// Public version information. Never fails; missing data is reported as
    /// [`ClientInfo::unavailable`] or `Could not determine`.
    fn information(&self) -> ClientInfo;
}
