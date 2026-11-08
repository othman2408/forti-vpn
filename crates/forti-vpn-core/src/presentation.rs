//! What the main window shows, computed from [`State`] without a toolkit.
//!
//! [`main_view`] started as a port of `UtilityWindow.render()` in the Python
//! 0.1.0 `forti_vpn/ui.py`, and [`retry_choice`] / [`exit_step`] of
//! `retry_choice()` and `advance_exit()`; differences are noted where they
//! are made. The GTK window only copies these values into widgets, so all
//! of this behaviour is unit-tested here.
//!
//! Python set the error, warning, and retry widgets independently and let
//! the hidden notice card hide them; [`MainView`] reports them hidden
//! whenever the card is, so the window cannot show one outside the card.
//!
//! # What the window still does itself
//!
//! A few facts live only in the window, which keeps them as `render()`
//! did in Python:
//!
//! * `cli_seen`: set on the first render whose status is anything but
//!   [`ConnectionState::Unavailable`], and never cleared. Only a window
//!   that has never seen FortiClient may quit without disconnecting; it
//!   is passed to [`exit_step`].
//! * Whether a tray shows the app's icon ([`MainInputs::tray_missing`]) and
//!   any message about the window itself ([`MainInputs::window_notice`]).
//! * [`MainInputs::first_check`]: true until the first refresh after launch
//!   has finished, whatever it found.
//! * An open authentication prompt is closed on the first render where
//!   [`State::busy`] is no longer [`Busy::AwaitingAuthentication`]: the
//!   operation was answered, cancelled, or timed out without it.
//! * While "Disconnect and Quit" runs, every render that finds no operation
//!   running ([`State::busy`] is `None`) schedules [`exit_step`] again, from
//!   an idle callback so that a step never runs inside a render. See
//!   [`exit_step`] for the rest of the exit sequence.
use crate::model::{ConnectionState, Profile};
use crate::parse::{LOGIN_BLOCKED_MESSAGE, PROFILE_LOAD_FAILED, SERVICE_STOPPED};
use crate::platform::Platform;
use crate::preferences::CloseBehavior;
use crate::workflow::{Busy, Operation, State};

/// Inputs that do not live in [`State`].
#[derive(Debug, Clone, Copy)]
pub struct MainInputs<'a> {
    pub state: &'a State,
    /// The profile chosen in the selector.
    pub selected: Option<&'a Profile>,
    /// "Disconnect and Quit" is in progress.
    pub exit_requested: bool,
    /// The profile editor is open (the notice's retry button is hidden).
    pub profile_dialog_open: bool,
    /// The desktop's high-contrast preference.
    pub high_contrast: bool,
    pub close_behavior: CloseBehavior,
    /// Used for desktop-specific wording ("top bar" or "system tray").
    pub platform: &'a Platform,
    /// FortiClient has not been read yet: the first refresh after launch
    /// is still running, so "unavailable" would be premature.
    pub first_check: bool,
    /// A tray host is known to be missing (not merely still registering),
    /// so closing the window cannot hide it.
    pub tray_missing: bool,
    /// A message about the window itself, such as why it stays open. It is
    /// shown in the notice card as a warning and never replaces the error
    /// of the last operation, so that error's title and retry stay right.
    pub window_notice: Option<&'a str>,
}

/// Status badge style; the GTK window adds CSS class `badge-<name>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    Idle,
    Connected,
    Progress,
    Authentication,
    Unavailable,
}

impl Badge {
    pub fn css_class(self) -> &'static str {
        match self {
            Self::Idle => "badge-idle",
            Self::Connected => "badge-connected",
            Self::Progress => "badge-progress",
            Self::Authentication => "badge-authentication",
            Self::Unavailable => "badge-unavailable",
        }
    }

    /// Symbolic icon shown in the badge when no operation is running.
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Connected => "emblem-ok-symbolic",
            Self::Unavailable => "dialog-warning-symbolic",
            Self::Authentication => "dialog-password-symbolic",
            Self::Idle => "network-offline-symbolic",
            Self::Progress => "view-refresh-symbolic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    Suggested,
    Destructive,
}

impl ButtonStyle {
    pub fn css_class(self) -> &'static str {
        match self {
            Self::Suggested => "suggested-action",
            Self::Destructive => "destructive-action",
        }
    }
}

/// A menu item or button whose availability is explained by its tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    pub sensitive: bool,
    /// Also used as the accessible description.
    pub tooltip: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonView {
    pub label: String,
    pub sensitive: bool,
    pub visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Error,
    Warning,
}

impl NoticeKind {
    /// CSS class for the notice icon and title.
    pub fn css_class(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// What the notice card's retry button does. Never replays a deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAction {
    CheckAgain,
    TryAgain,
    RetryDisconnect,
    Refresh,
    ManageProfiles,
}

impl RetryAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::CheckAgain => "Check Again",
            Self::TryAgain => "Try Again",
            Self::RetryDisconnect => "Retry Disconnect",
            Self::Refresh => "Refresh",
            Self::ManageProfiles => "Manage Profile",
        }
    }
}

/// Every value the main window displays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainView {
    /// e.g. "Disconnected", "Connecting…", "Connected · Office VPN",
    /// "Connection not confirmed". Announced when it changes.
    pub status_text: String,
    pub badge: Badge,
    /// False while an operation runs (the spinner shows instead).
    pub status_icon_visible: bool,
    pub spinner_active: bool,
    /// Use the green "connected" hero artwork.
    pub hero_connected: bool,
    /// Add `is-connected` to the header and show the decorative glow.
    pub connected_decoration: bool,
    /// Add `high-contrast` to the header.
    pub high_contrast: bool,

    pub setup_group_visible: bool,
    pub profiles_group_visible: bool,
    pub setup_help_visible: bool,
    pub profile_selector_sensitive: bool,
    pub refresh_sensitive: bool,
    pub manage_sensitive: bool,
    pub new_profile: Control,
    pub view_profile: Control,
    pub edit_profile: Control,
    pub delete_profile: Control,
    pub update_password: Control,

    pub summary_visible: bool,
    pub summary_profile: String,
    pub summary_server: String,

    pub credentials_visible: bool,
    pub credentials_sensitive: bool,
    /// Instructions under the summary, when visible.
    pub operation_note: Option<String>,

    pub primary: ButtonView,
    pub primary_style: ButtonStyle,
    pub cancel: ButtonView,

    /// Error text, when the notice card shows it.
    pub error: Option<String>,
    /// Warnings joined by blank lines, when the notice card shows them.
    pub warning: Option<String>,
    /// Notice card title and style, when the card is visible.
    pub notice: Option<(String, NoticeKind)>,
    /// What retrying would do, even while the button is hidden.
    pub retry: Option<RetryAction>,
    /// The notice card's retry button; never visible without the card.
    pub retry_visible: bool,
    pub retry_sensitive: bool,

    /// Footer note about what closing the window does.
    pub footer_note: String,
    pub footer_note_visible: bool,

    /// Tray menu status line and whether its Disconnect item is enabled.
    pub tray_text: String,
    pub tray_disconnect_enabled: bool,
    /// Show the tray's "connected" icon (the Python tray had a single icon;
    /// see the GTK crate's `tray` module).
    pub tray_connected: bool,

    /// The selected profile has a password saved in FortiClient. The window
    /// shows "Remember password" switched on and insensitive, and a normal
    /// Connect passes `remember = false` so FortiClient uses the saved
    /// password; replacing it is the explicit "Update Saved Password" action.
    pub saved_password: bool,
    /// Subtitle of the "Remember password" switch.
    pub remember_subtitle: String,

    /// Description of the profile group when FortiClient has no profiles.
    pub profiles_hint: Option<String>,
    /// Why Connect is insensitive when no profile can be selected; also its
    /// tooltip and accessible description.
    pub primary_hint: Option<String>,
}

/// Status text while the first refresh after launch runs.
pub const CHECKING: &str = "Checking FortiClient…";

/// Port of `UtilityWindow.render()`.
pub fn main_view(inputs: MainInputs<'_>) -> MainView {
    let state = inputs.state;
    let facts = Facts::new(state);
    let selected = inputs.selected.is_some();
    let lock = lock_reason(state, inputs.selected);
    let editable = facts.idle && selected && lock.is_none();
    let status = status_text(&facts);
    // The setup screen replaces operation errors and warnings, but a
    // message about the window must still be seen there.
    let show_operation = !state.setup_required;
    let has_error = show_operation && !state.error.is_empty();
    let operation_warning = if show_operation {
        warning_text(state)
    } else {
        String::new()
    };
    let warning = [
        operation_warning.as_str(),
        inputs.window_notice.unwrap_or(""),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    let only_window_notice = !has_error && operation_warning.is_empty();
    let notice = (has_error || !warning.is_empty())
        .then(|| notice_heading(state, has_error, only_window_notice));
    let retry = retry_choice(state, selected);
    let no_profiles = facts.idle && facts.disconnected && state.profiles.is_empty();
    let mut view = MainView {
        tray_text: status.clone(),
        status_text: status,
        badge: badge(&facts),
        status_icon_visible: !facts.working,
        spinner_active: facts.working,
        hero_connected: facts.connected,
        connected_decoration: facts.phase == Phase::Connected && !inputs.high_contrast,
        high_contrast: inputs.high_contrast,

        setup_group_visible: state.setup_required,
        profiles_group_visible: !state.setup_required,
        setup_help_visible: state.status.state == ConnectionState::Unavailable
            && !state.setup_required,
        profile_selector_sensitive: facts.idle,
        refresh_sensitive: facts.idle,
        manage_sensitive: facts.idle,
        new_profile: control(
            facts.idle && facts.disconnected,
            if facts.disconnected {
                "New profile"
            } else if facts.unavailable {
                UNAVAILABLE_TOOLTIP
            } else {
                "Disconnect the VPN before creating a profile."
            },
        ),
        view_profile: control(facts.idle && selected, "View profile"),
        edit_profile: control(editable, lock.unwrap_or("Edit profile")),
        delete_profile: control(editable, lock.unwrap_or("Delete profile")),
        update_password: control(
            facts.idle && selected && facts.disconnected,
            if facts.disconnected {
                "Reconnect and replace the password saved by FortiClient"
            } else if facts.unavailable {
                UNAVAILABLE_TOOLTIP
            } else {
                "Disconnect the VPN before updating a saved password."
            },
        ),

        summary_visible: facts.connected
            && !matches!(
                facts.phase,
                Phase::Connecting | Phase::AwaitingAuthentication
            ),
        summary_profile: active_name(state).unwrap_or("Unknown profile").to_owned(),
        summary_server: summary_server(state),

        credentials_visible: facts.phase == Phase::Disconnected,
        credentials_sensitive: facts.idle && facts.disconnected && selected,
        operation_note: operation_note(&facts),

        primary: primary(&facts, selected),
        primary_style: if facts.connected {
            ButtonStyle::Destructive
        } else {
            ButtonStyle::Suggested
        },
        cancel: cancel(&facts),

        error: has_error.then(|| state.error.clone()),
        warning: (!warning.is_empty()).then_some(warning),
        notice,
        retry,
        retry_visible: has_error && retry.is_some() && !inputs.profile_dialog_open,
        retry_sensitive: facts.idle,

        footer_note: footer_note(inputs.close_behavior, inputs.platform, inputs.tray_missing),
        footer_note_visible: !state.setup_required,

        tray_disconnect_enabled: facts.connected && facts.idle && !inputs.exit_requested,
        tray_connected: facts.connected,

        saved_password: has_saved_password(inputs.selected),
        remember_subtitle: if has_saved_password(inputs.selected) {
            "Saved in FortiClient. To change it, use Manage Profile → Update Saved Password."
        } else {
            "Stored by FortiClient, never by this app"
        }
        .to_owned(),

        profiles_hint: no_profiles
            .then(|| "No VPN profiles yet. Create one with Manage Profile → New.".to_owned()),
        primary_hint: (facts.idle && facts.disconnected && !selected).then(|| {
            if no_profiles {
                "Create a VPN profile to connect."
            } else {
                "Select a VPN profile to connect."
            }
            .to_owned()
        }),
    };
    if inputs.first_check {
        show_first_check(&mut view);
    }
    view
}

/// Whether FortiClient reports a saved password for `profile`.
fn has_saved_password(profile: Option<&Profile>) -> bool {
    profile.and_then(|profile| profile.field("Save Password")) == Some("Enabled")
}

/// Until the first refresh finishes, nothing is known: show progress
/// instead of "FortiClient unavailable" and its setup help.
fn show_first_check(view: &mut MainView) {
    view.status_text = CHECKING.to_owned();
    view.tray_text = CHECKING.to_owned();
    view.badge = Badge::Progress;
    view.status_icon_visible = false;
    view.spinner_active = true;
    view.setup_help_visible = false;
    view.operation_note = None;
    view.profiles_hint = None;
    view.primary_hint = None;
}

/// Tooltip of profile actions while FortiClient cannot be reached.
const UNAVAILABLE_TOOLTIP: &str = "FortiClient is unavailable.";

/// Port of `UtilityWindow.retry_choice()`.
pub fn retry_choice(state: &State, has_selection: bool) -> Option<RetryAction> {
    let status = state.status.state;
    if state.setup_required {
        Some(RetryAction::CheckAgain)
    } else if reports(state, LOGIN_BLOCKED_MESSAGE) {
        // The server refuses logins for a while, so an immediate retry
        // cannot succeed.
        None
    } else if state.error_origin == Some(Operation::Connecting)
        && status == ConnectionState::Disconnected
        && has_selection
    {
        Some(RetryAction::TryAgain)
    } else if state.error_origin == Some(Operation::Disconnecting)
        && status == ConnectionState::Connected
    {
        Some(RetryAction::RetryDisconnect)
    } else if state.error_origin == Some(Operation::Refreshing)
        || status == ConnectionState::Unavailable
    {
        Some(RetryAction::Refresh)
    } else if matches!(
        state.error_origin,
        Some(Operation::SavingProfile | Operation::RemovingProfile)
    ) {
        Some(RetryAction::ManageProfiles)
    } else {
        None
    }
}

/// One step of "Disconnect and Quit"; port of `advance_exit()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStep {
    /// An operation is running; try again when it finishes.
    Wait,
    /// Re-read the state first ([`crate::workflow::Workflow::check_before_exit`]).
    Check,
    /// The VPN is disconnected; quit now.
    Quit,
    /// Disconnect, then try again.
    Disconnect,
    /// Disconnection cannot be confirmed; stay open with [`EXIT_FAILED`].
    Fail,
}

pub const EXIT_FAILED: &str = "Cannot confirm VPN disconnection. The app remains open. Check FortiClient and retry Disconnect and Quit.";

/// The next step of "Disconnect and Quit". `checked` is whether the window
/// already started [`ExitStep::Check`] for this exit (the step waits until
/// it has finished).
///
/// The window drives the sequence, as `request_exit()` and
/// `advance_exit()` did in Python:
///
/// 1. When `setup_required` is set and `cli_seen` is not (see the module
///    documentation), FortiClient was never usable, so no VPN can be up:
///    quit at once, without calling this function. (It must not skip the
///    check on `!cli_seen` alone: during the first refresh FortiClient may
///    still report a tunnel left up by an earlier session.)
/// 2. Otherwise, unless an exit is already under way, set `exit_requested`,
///    clear `checked`, show the window, and run `cancel_connection()`:
///    disconnect when no operation is running and the status is
///    [`ConnectionState::Connecting`] or
///    [`ConnectionState::AwaitingAuthentication`] (an attempt FortiClient
///    still reports), otherwise cancel the running operation; either way
///    close any open prompt. Then call this function.
/// 3. Act on the step, and call this function again after every render
///    that finds no operation running (see the module documentation):
///    [`ExitStep::Wait`] does nothing; [`ExitStep::Check`] sets `checked`
///    and starts [`crate::workflow::Workflow::check_before_exit`];
///    [`ExitStep::Disconnect`] starts a disconnection; [`ExitStep::Quit`]
///    quits; [`ExitStep::Fail`] clears `exit_requested` and shows
///    [`EXIT_FAILED`] as the error.
///
/// `cli_seen` is the window's flag after the check has been rendered. When
/// FortiClient has never answered, not even to the check (for example
/// because its service is stopped), this app cannot have connected — it
/// only connects from [`ConnectionState::Disconnected`] — so it quits
/// instead of failing. Python failed here and could leave the window
/// impossible to close.
pub fn exit_step(state: &State, checked: bool, cli_seen: bool) -> ExitStep {
    let status = state.status.state;
    if state.busy.is_some() {
        ExitStep::Wait
    } else if !checked {
        ExitStep::Check
    } else if status == ConnectionState::Disconnected
        || (status == ConnectionState::Unavailable && !cli_seen)
    {
        ExitStep::Quit
    } else if status == ConnectionState::Unavailable
        || state.error_origin == Some(Operation::Disconnecting)
    {
        ExitStep::Fail
    } else {
        ExitStep::Disconnect
    }
}

/// Title shown for a busy state, e.g. "Saving profile…" or
/// "Checking before exit". The window shows it through
/// [`MainView::status_text`]; tests check the whole table here.
#[cfg(test)]
fn busy_title(busy: Busy) -> String {
    Phase::of_busy(busy).title().to_owned()
}

/// What the status line describes: the running operation, else the status
/// FortiClient reports. Python compared these as plain strings, so an
/// operation and a status with the same name (`connecting`…) are one phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Unavailable,
    Disconnected,
    Connecting,
    AwaitingAuthentication,
    Connected,
    Disconnecting,
    SavingProfile,
    RemovingProfile,
    CheckingBeforeExit,
    Refreshing,
}

impl Phase {
    fn of_busy(busy: Busy) -> Self {
        match busy {
            Busy::AwaitingAuthentication => Self::AwaitingAuthentication,
            Busy::Operation(Operation::Refreshing) => Self::Refreshing,
            Busy::Operation(Operation::Connecting) => Self::Connecting,
            Busy::Operation(Operation::Disconnecting) => Self::Disconnecting,
            Busy::Operation(Operation::SavingProfile) => Self::SavingProfile,
            Busy::Operation(Operation::RemovingProfile) => Self::RemovingProfile,
            Busy::Operation(Operation::CheckingBeforeExit) => Self::CheckingBeforeExit,
        }
    }

    fn of_status(state: ConnectionState) -> Self {
        match state {
            ConnectionState::Unavailable => Self::Unavailable,
            ConnectionState::Disconnected => Self::Disconnected,
            ConnectionState::Connecting => Self::Connecting,
            ConnectionState::AwaitingAuthentication => Self::AwaitingAuthentication,
            ConnectionState::Connected => Self::Connected,
            ConnectionState::Disconnecting => Self::Disconnecting,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Unavailable => "FortiClient unavailable",
            Self::Disconnected => "Disconnected",
            Self::Connecting => "Connecting…",
            Self::AwaitingAuthentication => "Awaiting authentication…",
            Self::Connected => "Connected",
            Self::Disconnecting => "Disconnecting…",
            Self::SavingProfile => "Saving profile…",
            Self::RemovingProfile => "Deleting profile…",
            // Python shows the capitalised operation name when it has no title.
            Self::CheckingBeforeExit => "Checking before exit",
            Self::Refreshing => "Refreshing",
        }
    }
}

/// What `render()` derives from the state before it sets any widget.
struct Facts<'a> {
    state: &'a State,
    /// An operation other than a refresh is running.
    working: bool,
    idle: bool,
    connected: bool,
    disconnected: bool,
    unavailable: bool,
    /// The running operation can be cancelled.
    cancellable: bool,
    /// FortiClient reports an attempt in progress that no operation of this
    /// app is driving.
    unconfirmed: bool,
    /// The attempt this app made was rejected and may still be pending.
    failed_attempt: bool,
    phase: Phase,
}

impl<'a> Facts<'a> {
    fn new(state: &'a State) -> Self {
        let working = state
            .busy
            .is_some_and(|busy| busy != Busy::Operation(Operation::Refreshing));
        let pending = matches!(
            state.status.state,
            ConnectionState::Connecting | ConnectionState::AwaitingAuthentication
        );
        let unconfirmed = pending && !working;
        let phase = match state.busy {
            Some(busy) if working => Phase::of_busy(busy),
            _ => Phase::of_status(state.status.state),
        };
        Self {
            state,
            working,
            idle: state.busy.is_none(),
            connected: state.status.state == ConnectionState::Connected,
            disconnected: state.status.state == ConnectionState::Disconnected,
            unavailable: state.status.state == ConnectionState::Unavailable,
            cancellable: matches!(
                state.busy,
                Some(Busy::Operation(Operation::Connecting) | Busy::AwaitingAuthentication)
            ),
            unconfirmed,
            failed_attempt: unconfirmed
                && state.error_origin == Some(Operation::Connecting)
                && !state.error.is_empty(),
            phase,
        }
    }
}

fn control(sensitive: bool, tooltip: &str) -> Control {
    Control {
        sensitive,
        tooltip: tooltip.to_owned(),
    }
}

/// The connected profile's name, when FortiClient reports one.
fn active_name(state: &State) -> Option<&str> {
    state
        .status
        .profile
        .as_deref()
        .filter(|name| !name.is_empty())
}

fn status_text(facts: &Facts<'_>) -> String {
    match active_name(facts.state) {
        Some(name) if facts.connected && !facts.working => format!("Connected · {name}"),
        _ if facts.failed_attempt => "Connection not confirmed".to_owned(),
        _ => facts.phase.title().to_owned(),
    }
}

fn badge(facts: &Facts<'_>) -> Badge {
    match facts.phase {
        Phase::Connected => Badge::Connected,
        Phase::Unavailable => Badge::Unavailable,
        _ if facts.failed_attempt => Badge::Unavailable,
        Phase::AwaitingAuthentication => Badge::Authentication,
        Phase::Disconnected => Badge::Idle,
        _ => Badge::Progress,
    }
}

/// Why the selected profile cannot be edited or deleted, if it cannot.
fn lock_reason(state: &State, selected: Option<&Profile>) -> Option<&'static str> {
    let profile = selected?;
    if profile.managed {
        Some("This profile is managed by your organization.")
    } else if state.status.state == ConnectionState::Unavailable {
        Some(UNAVAILABLE_TOOLTIP)
    } else if state.status.state != ConnectionState::Disconnected
        && state
            .status
            .profile
            .as_deref()
            .is_none_or(|active| active == profile.name)
    {
        Some("Disconnect this profile before editing or deleting it.")
    } else {
        None
    }
}

fn summary_server(state: &State) -> String {
    let active = state
        .profiles
        .iter()
        .find(|profile| state.status.profile.as_deref() == Some(profile.name.as_str()));
    active
        .map(Profile::gateway)
        .filter(|gateway| !gateway.is_empty())
        .unwrap_or("Server details unavailable")
        .to_owned()
}

fn operation_note(facts: &Facts<'_>) -> Option<String> {
    let instruction = match facts.phase {
        Phase::Connecting => "Contacting the VPN server. You may be asked to sign in.",
        Phase::AwaitingAuthentication => {
            "Complete the sign-in prompt to continue, or cancel this attempt."
        }
        Phase::Disconnecting => "Disconnecting from the VPN. Please wait.",
        Phase::Unavailable => "Check that FortiClient is installed and running, then refresh.",
        _ => return None,
    };
    if facts.state.setup_required {
        return None;
    }
    let note = if facts.failed_attempt {
        "Check the current status or cancel the unfinished attempt."
    } else {
        instruction
    };
    Some(note.to_owned())
}

fn primary(facts: &Facts<'_>, selected: bool) -> ButtonView {
    let label = if facts.connected {
        "Disconnect"
    } else if facts.phase == Phase::Disconnecting {
        "Cancelling…"
    } else if facts.unconfirmed {
        "Check Status"
    } else {
        "Connect"
    };
    ButtonView {
        label: label.to_owned(),
        sensitive: facts.idle
            && (facts.connected || facts.unconfirmed || (selected && facts.disconnected)),
        visible: !facts.cancellable && !facts.state.setup_required,
    }
}

fn cancel(facts: &Facts<'_>) -> ButtonView {
    let label = if facts.unconfirmed {
        "Cancel Attempt"
    } else {
        "Cancel"
    };
    ButtonView {
        label: label.to_owned(),
        sensitive: facts.cancellable || (facts.unconfirmed && facts.idle),
        visible: facts.cancellable || facts.unconfirmed,
    }
}

/// Warnings that do not repeat the error, separated by blank lines.
fn warning_text(state: &State) -> String {
    let shown: Vec<&str> = state
        .warnings
        .iter()
        .map(String::as_str)
        .filter(|warning| *warning != state.error)
        .collect();
    shown.join("\n\n")
}

/// Whether the error reports `description`, one of the fixed messages
/// [`crate::parse`] gives recognised FortiClient failures. Python looked
/// for a fragment of the wording; looking for the whole constant keeps
/// the match working when `parse` rewords a message.
fn reports(state: &State, description: &str) -> bool {
    state.error.contains(description)
}

/// The notice card's title and style. `has_error` is whether the card
/// shows the operation error; `only_window_notice` whether it shows nothing
/// but [`MainInputs::window_notice`].
fn notice_heading(
    state: &State,
    has_error: bool,
    only_window_notice: bool,
) -> (String, NoticeKind) {
    if !has_error {
        let title = if only_window_notice {
            "Window stays open"
        } else {
            "Warning"
        };
        return (title.to_owned(), NoticeKind::Warning);
    }
    let title = if reports(state, LOGIN_BLOCKED_MESSAGE) {
        "Login temporarily blocked"
    } else if reports(state, PROFILE_LOAD_FAILED) {
        "Profile could not be loaded"
    } else if reports(state, SERVICE_STOPPED) {
        // Not in Python, which had no message for a stopped service.
        "FortiClient service stopped"
    } else if state.error_origin == Some(Operation::Connecting) {
        "Connection issue"
    } else {
        "Operation failed"
    };
    (title.to_owned(), NoticeKind::Error)
}

/// Port of the note `apply_preferences()` sets. Without a tray the window
/// cannot hide, so the note says what closing really does (not in Python).
fn footer_note(close_behavior: CloseBehavior, platform: &Platform, tray_missing: bool) -> String {
    match close_behavior {
        CloseBehavior::Hide if tray_missing => format!(
            "Without a {} icon, this window stays open when closed. Use Disconnect and Quit to exit.",
            platform.tray_term()
        ),
        CloseBehavior::Hide => format!("Close to keep running in the {}.", platform.tray_term()),
        CloseBehavior::Quit => "Closing this window disconnects the VPN and quits.".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{CLI_NOT_FOUND, VpnError};
    use crate::model::Status;
    use crate::platform::{Desktop, DistroFamily, Elevation, OsRelease, Sandbox, SessionType};
    use crate::workflow::testing::{FakeCli, Harness};

    fn platform(desktop: Desktop) -> Platform {
        Platform {
            os: OsRelease::default(),
            family: DistroFamily::Debian,
            ubuntu_based: true,
            enterprise: false,
            image_based: false,
            desktop,
            desktop_name: "ubuntu:GNOME".to_owned(),
            session: SessionType::Wayland,
            sandbox: Sandbox::None,
            architecture: "x86_64".to_owned(),
            elevation: Elevation::Sudo,
            aur_helper: None,
        }
    }

    /// The window inputs besides the state, as the Python UI tests'
    /// `tearDown` leaves them.
    struct Window {
        platform: Platform,
        high_contrast: bool,
        exit_requested: bool,
        profile_dialog_open: bool,
        close_behavior: CloseBehavior,
    }

    impl Window {
        fn new() -> Self {
            Self {
                platform: platform(Desktop::Gnome),
                high_contrast: false,
                exit_requested: false,
                profile_dialog_open: false,
                close_behavior: CloseBehavior::Hide,
            }
        }

        /// Render with the first profile selected, as the profile selector
        /// is after a refresh.
        fn render(&self, state: &State) -> MainView {
            main_view(MainInputs {
                state,
                selected: state.profiles.first(),
                exit_requested: self.exit_requested,
                profile_dialog_open: self.profile_dialog_open,
                high_contrast: self.high_contrast,
                close_behavior: self.close_behavior,
                platform: &self.platform,
                first_check: false,
                tray_missing: false,
                window_notice: None,
            })
        }
    }

    fn office() -> Profile {
        Profile::with_fields(
            "Office VPN",
            [("Remote Gateway", "localhost:443"), ("Username", "worker")],
        )
    }

    /// The state after the window's first refresh in the Python UI tests.
    fn initial() -> State {
        State {
            status: Status::new(ConnectionState::Disconnected),
            profiles: vec![office()],
            ..State::default()
        }
    }

    fn connected(profile: &str) -> Status {
        Status::with_profile(ConnectionState::Connected, profile)
    }

    fn title(view: &MainView) -> Option<&str> {
        view.notice.as_ref().map(|(title, _)| title.as_str())
    }

    #[test]
    fn badge_and_glow_follow_verified_status() {
        let window = Window::new();
        let mut state = initial();
        state.status = connected("Office VPN");
        let view = window.render(&state);
        assert_eq!(view.badge, Badge::Connected);
        assert!(view.connected_decoration);
        state.busy = Some(Busy::Operation(Operation::Disconnecting));
        assert!(!window.render(&state).connected_decoration);
        state.busy = Some(Busy::AwaitingAuthentication);
        let view = window.render(&state);
        assert_eq!(view.badge, Badge::Authentication);
        assert!(!view.primary.visible);
        assert!(view.cancel.visible);
        state.busy = None;
        state.status = Status::new(ConnectionState::Disconnected);
        let view = window.render(&state);
        assert_eq!(view.badge, Badge::Idle);
        assert!(!view.connected_decoration);
        assert!(view.primary.visible);
    }

    #[test]
    fn high_contrast_disables_decorative_glow() {
        let window = Window {
            high_contrast: true,
            ..Window::new()
        };
        let mut state = initial();
        state.status = connected("Office VPN");
        let view = window.render(&state);
        assert!(!view.connected_decoration);
        assert!(view.high_contrast);
        assert!(view.hero_connected);
        assert!(view.status_text.contains("Connected"));
    }

    #[test]
    fn main_window_and_actions() {
        let window = Window::new();
        let mut state = initial();
        let view = window.render(&state);
        assert_eq!(view.status_text, "Disconnected");
        assert_eq!(view.primary.label, "Connect");
        assert!(view.primary.sensitive);
        assert!(view.credentials_visible && view.credentials_sensitive);
        state.status = connected("Office VPN");
        let view = window.render(&state);
        assert!(!view.edit_profile.sensitive);
        assert!(!view.delete_profile.sensitive);
        assert_eq!(view.primary.label, "Disconnect");
        assert!(view.primary.sensitive);
    }

    #[test]
    fn connection_layout_uses_actual_profile() {
        let window = Window::new();
        let mut state = initial();
        state.profiles.push(Profile::with_fields(
            "Other VPN",
            [("Remote Gateway", "vpn.example.org:443")],
        ));
        state.busy = Some(Busy::Operation(Operation::Connecting));
        let view = window.render(&state);
        assert!(!view.credentials_visible);
        assert!(!view.summary_visible);
        assert_eq!(
            view.operation_note.as_deref(),
            Some("Contacting the VPN server. You may be asked to sign in.")
        );
        assert!(view.cancel.sensitive);
        assert!(!view.manage_sensitive);
        state.busy = None;
        state.status = connected("Other VPN");
        let view = window.render(&state);
        assert!(view.summary_visible);
        assert!(!view.credentials_visible);
        assert_eq!(view.summary_profile, "Other VPN");
        assert_eq!(view.summary_server, "vpn.example.org:443");
        assert_eq!(view.primary_style, ButtonStyle::Destructive);
        state.busy = Some(Busy::Operation(Operation::Disconnecting));
        let view = window.render(&state);
        assert!(!view.primary.sensitive);
        assert_eq!(view.primary.label, "Disconnect");
        assert!(!view.cancel.visible);
        state.busy = None;
        state.status = Status::new(ConnectionState::Disconnected);
        let view = window.render(&state);
        assert!(view.credentials_visible);
        assert!(!view.summary_visible);
        assert_eq!(view.primary_style, ButtonStyle::Suggested);
    }

    #[test]
    fn summary_explains_missing_details_without_escaping() {
        let window = Window::new();
        let mut state = initial();
        state.status = Status::new(ConnectionState::Connected);
        let view = window.render(&state);
        assert_eq!(view.status_text, "Connected");
        assert_eq!(view.summary_profile, "Unknown profile");
        assert_eq!(view.summary_server, "Server details unavailable");
        state.status = connected("Office VPN");
        assert_eq!(window.render(&state).summary_server, "localhost:443");
        state.status = connected("R&D <lab>");
        let view = window.render(&state);
        assert_eq!(view.summary_profile, "R&D <lab>");
        assert_eq!(view.status_text, "Connected · R&D <lab>");
    }

    #[test]
    fn error_and_warning_notices_are_visible() {
        let window = Window::new();
        let mut state = initial();
        state.error = LOGIN_BLOCKED_MESSAGE.to_owned();
        state.error_origin = Some(Operation::Connecting);
        let view = window.render(&state);
        assert_eq!(
            view.notice,
            Some(("Login temporarily blocked".to_owned(), NoticeKind::Error))
        );
        assert_eq!(view.error.as_deref(), Some(LOGIN_BLOCKED_MESSAGE));
        // Without the block, this failed connection would offer "Try Again".
        assert_eq!(view.retry, None);
        assert!(!view.retry_visible);
        state.error.clear();
        state.status = connected("Office VPN");
        state.warnings = vec!["Your password could not be saved.".to_owned()];
        let view = window.render(&state);
        assert_eq!(
            view.notice,
            Some(("Warning".to_owned(), NoticeKind::Warning))
        );
        assert_eq!(
            view.warning.as_deref(),
            Some("Your password could not be saved.")
        );
        assert_eq!(view.error, None);
        assert_eq!(view.status_text, "Connected · Office VPN");
    }

    #[test]
    fn notice_titles_and_warnings_do_not_repeat_the_error() {
        let window = Window::new();
        let mut state = initial();
        state.error = PROFILE_LOAD_FAILED.to_owned();
        state.error_origin = Some(Operation::Connecting);
        assert_eq!(
            title(&window.render(&state)),
            Some("Profile could not be loaded")
        );
        state.error = SERVICE_STOPPED.to_owned();
        assert_eq!(
            title(&window.render(&state)),
            Some("FortiClient service stopped")
        );
        state.error = "FortiClient rejected the operation.".to_owned();
        assert_eq!(title(&window.render(&state)), Some("Connection issue"));
        state.error_origin = Some(Operation::SavingProfile);
        assert_eq!(title(&window.render(&state)), Some("Operation failed"));
        state.warnings = ["First.", "FortiClient rejected the operation.", "Second."]
            .map(String::from)
            .to_vec();
        assert_eq!(
            window.render(&state).warning.as_deref(),
            Some("First.\n\nSecond.")
        );
        state.error.clear();
        state.warnings.clear();
        let view = window.render(&state);
        assert_eq!(view.notice, None);
        assert_eq!(view.warning, None);
    }

    #[test]
    fn stopped_service_notice_keeps_the_usual_retry() {
        let window = Window::new();
        let mut state = initial();
        state.status = Status::unavailable();
        state.error = SERVICE_STOPPED.to_owned();
        for origin in [Operation::Refreshing, Operation::Connecting] {
            state.error_origin = Some(origin);
            let view = window.render(&state);
            assert_eq!(
                view.notice,
                Some(("FortiClient service stopped".to_owned(), NoticeKind::Error))
            );
            assert_eq!(view.error.as_deref(), Some(SERVICE_STOPPED));
            assert_eq!(view.retry, Some(RetryAction::Refresh));
            assert!(view.retry_visible);
            assert!(view.setup_help_visible);
        }
        state.status = Status::new(ConnectionState::Disconnected);
        let view = window.render(&state);
        assert_eq!(title(&view), Some("FortiClient service stopped"));
        assert_eq!(view.retry, Some(RetryAction::TryAgain));
    }

    #[test]
    fn notice_contents_are_hidden_with_the_card() {
        let window = Window::new();
        let mut state = initial();
        state.setup_required = true;
        state.status = Status::unavailable();
        state.error = CLI_NOT_FOUND.to_owned();
        state.error_origin = Some(Operation::Refreshing);
        state.warnings = vec!["Your password could not be saved.".to_owned()];
        let view = window.render(&state);
        assert_eq!(view.notice, None);
        assert_eq!(view.error, None);
        assert_eq!(view.warning, None);
        assert_eq!(view.retry, Some(RetryAction::CheckAgain));
        assert!(!view.retry_visible);
        state.setup_required = false;
        let view = window.render(&state);
        assert_eq!(title(&view), Some("Operation failed"));
        assert_eq!(view.error.as_deref(), Some(CLI_NOT_FOUND));
        assert_eq!(
            view.warning.as_deref(),
            Some("Your password could not be saved.")
        );
        assert_eq!(view.retry, Some(RetryAction::Refresh));
        assert!(view.retry_visible);
    }

    #[test]
    fn rejected_connection_with_pending_cli_status_offers_check_and_cancel() {
        let cli = FakeCli::new();
        cli.with(|fake| {
            fake.current = Status::with_profile(ConnectionState::Connecting, "Office VPN");
            fake.connect_error = Some(VpnError::failed("FortiClient rejected the operation."));
        });
        let mut harness = Harness::new(cli);
        harness
            .workflow
            .connect(Profile::new("Office VPN"), String::new(), false);
        harness.settle();
        let state = harness.workflow.state();
        assert_eq!(state.status.state, ConnectionState::Connecting);
        let view = Window::new().render(state);
        assert_eq!(view.status_text, "Connection not confirmed");
        assert_eq!(view.primary.label, "Check Status");
        assert_eq!(view.primary_style, ButtonStyle::Suggested);
        assert!(view.primary.visible && view.primary.sensitive);
        assert_eq!(view.cancel.label, "Cancel Attempt");
        assert!(view.cancel.visible && view.cancel.sensitive);
        assert_eq!(view.badge, Badge::Unavailable);
        assert_eq!(
            view.operation_note.as_deref(),
            Some("Check the current status or cancel the unfinished attempt.")
        );
        assert_eq!(title(&view), Some("Connection issue"));
    }

    #[test]
    fn cancelling_an_unconfirmed_attempt() {
        let mut state = initial();
        state.status = Status::with_profile(ConnectionState::Connecting, "Office VPN");
        state.busy = Some(Busy::Operation(Operation::Disconnecting));
        let view = Window::new().render(&state);
        assert_eq!(view.status_text, "Disconnecting…");
        assert_eq!(view.primary.label, "Cancelling…");
        assert!(!view.primary.sensitive);
        assert!(!view.cancel.visible);
        assert_eq!(view.badge, Badge::Progress);
        assert!(view.spinner_active && !view.status_icon_visible);
        assert_eq!(
            view.operation_note.as_deref(),
            Some("Disconnecting from the VPN. Please wait.")
        );
    }

    #[test]
    fn profile_menu_lock_explanation() {
        let window = Window::new();
        let mut state = initial();
        state.status = connected("Office VPN");
        let view = window.render(&state);
        assert!(view.view_profile.sensitive);
        assert!(!view.edit_profile.sensitive);
        assert!(!view.delete_profile.sensitive);
        assert!(!view.update_password.sensitive);
        assert!(view.edit_profile.tooltip.contains("Disconnect"));
        state.status = Status::new(ConnectionState::Disconnected);
        state.profiles[0] = Profile {
            managed: true,
            ..Profile::new("Office VPN")
        };
        let view = window.render(&state);
        assert!(view.delete_profile.tooltip.contains("organization"));
        assert!(!view.edit_profile.sensitive);
    }

    #[test]
    fn profile_menu_tooltips_explain_availability() {
        let window = Window::new();
        let mut state = initial();
        let view = window.render(&state);
        assert_eq!(view.new_profile, control(true, "New profile"));
        assert_eq!(view.view_profile, control(true, "View profile"));
        assert_eq!(view.edit_profile, control(true, "Edit profile"));
        assert_eq!(view.delete_profile, control(true, "Delete profile"));
        assert_eq!(
            view.update_password,
            control(
                true,
                "Reconnect and replace the password saved by FortiClient"
            )
        );
        state.status = connected("Other VPN");
        let view = window.render(&state);
        assert_eq!(view.edit_profile, control(true, "Edit profile"));
        assert_eq!(
            view.new_profile,
            control(false, "Disconnect the VPN before creating a profile.")
        );
        assert_eq!(
            view.update_password,
            control(
                false,
                "Disconnect the VPN before updating a saved password."
            )
        );
        state.status = Status::new(ConnectionState::Connecting);
        assert_eq!(
            window.render(&state).delete_profile,
            control(
                false,
                "Disconnect this profile before editing or deleting it."
            )
        );
        state.status = Status::new(ConnectionState::Disconnected);
        state.busy = Some(Busy::Operation(Operation::SavingProfile));
        let view = window.render(&state);
        assert_eq!(view.status_text, "Saving profile…");
        assert!(!view.new_profile.sensitive && !view.view_profile.sensitive);
        assert!(!view.profile_selector_sensitive && !view.refresh_sensitive);
        state.busy = None;
        state.profiles.clear();
        let view = window.render(&state);
        assert!(!view.view_profile.sensitive && !view.edit_profile.sensitive);
        assert!(!view.update_password.sensitive && !view.primary.sensitive);
        assert!(view.new_profile.sensitive);
    }

    #[test]
    fn retry_follows_actual_status_and_never_replays_delete() {
        let window = Window::new();
        let mut state = initial();
        state.error = "Operation failed.".to_owned();
        state.error_origin = Some(Operation::Connecting);
        let view = window.render(&state);
        assert_eq!(view.retry.map(RetryAction::label), Some("Try Again"));
        assert!(view.retry_visible && view.retry_sensitive);
        state.status = connected("Office VPN");
        assert!(!window.render(&state).retry_visible);
        state.error_origin = Some(Operation::Disconnecting);
        let view = window.render(&state);
        assert_eq!(view.retry, Some(RetryAction::RetryDisconnect));
        assert!(view.retry_visible);
        state.error_origin = Some(Operation::RemovingProfile);
        let view = window.render(&state);
        assert_eq!(view.retry.map(RetryAction::label), Some("Manage Profile"));
        state.status = Status::unavailable();
        assert_eq!(window.render(&state).retry, Some(RetryAction::Refresh));
    }

    #[test]
    fn retry_button_needs_an_error_and_no_open_editor() {
        let mut state = initial();
        state.error_origin = Some(Operation::Connecting);
        assert_eq!(retry_choice(&state, true), Some(RetryAction::TryAgain));
        assert_eq!(retry_choice(&state, false), None);
        assert!(!Window::new().render(&state).retry_visible);
        state.error = "Operation failed.".to_owned();
        let editing = Window {
            profile_dialog_open: true,
            ..Window::new()
        };
        assert!(!editing.render(&state).retry_visible);
        state.busy = Some(Busy::Operation(Operation::Connecting));
        assert!(!Window::new().render(&state).retry_sensitive);
        let refresh = State {
            error_origin: Some(Operation::Refreshing),
            ..initial()
        };
        assert_eq!(retry_choice(&refresh, true), Some(RetryAction::Refresh));
        let saving = State {
            error_origin: Some(Operation::SavingProfile),
            ..initial()
        };
        assert_eq!(
            retry_choice(&saving, true),
            Some(RetryAction::ManageProfiles)
        );
        let exit_check = State {
            error_origin: Some(Operation::CheckingBeforeExit),
            ..initial()
        };
        assert_eq!(retry_choice(&exit_check, true), None);
    }

    #[test]
    fn missing_cli_setup_screen_and_check_again_recovery() {
        let cli = FakeCli::new();
        cli.with(|fake| fake.missing = true);
        let mut harness = Harness::new(cli);
        harness.refresh();
        let window = Window::new();
        let state = harness.workflow.state();
        assert!(state.setup_required);
        let view = window.render(state);
        assert!(view.setup_group_visible);
        assert!(!view.profiles_group_visible);
        assert!(!view.primary.visible);
        assert!(!view.setup_help_visible);
        assert!(!view.footer_note_visible);
        assert_eq!(view.operation_note, None);
        assert_eq!(view.error, None);
        assert_eq!(view.notice, None);
        assert_eq!(view.retry, Some(RetryAction::CheckAgain));
        // `retry` still says what retrying does, but the card is hidden.
        assert!(!view.retry_visible);
        harness.cli.with(|fake| fake.missing = false);
        harness.refresh();
        let state = harness.workflow.state();
        assert!(!state.setup_required);
        let view = window.render(state);
        assert!(view.primary.visible);
        assert!(!view.setup_group_visible);
        assert!(view.profiles_group_visible);
        assert!(view.footer_note_visible);
        assert_eq!(view.notice, None);
    }

    #[test]
    fn unavailable_client_offers_setup_help() {
        let mut state = initial();
        state.status = Status::unavailable();
        let view = Window::new().render(&state);
        assert!(view.setup_help_visible);
        assert_eq!(view.status_text, "FortiClient unavailable");
        assert_eq!(view.badge, Badge::Unavailable);
        assert_eq!(
            view.operation_note.as_deref(),
            Some("Check that FortiClient is installed and running, then refresh.")
        );
        assert_eq!(view.primary.label, "Connect");
        assert!(!view.primary.sensitive);
        assert!(!view.credentials_visible);
    }

    #[test]
    fn refresh_in_busy_is_not_shown_as_work() {
        let mut state = initial();
        state.busy = Some(Busy::Operation(Operation::Refreshing));
        let view = Window::new().render(&state);
        assert_eq!(view.status_text, "Disconnected");
        assert!(!view.spinner_active && view.status_icon_visible);
        assert!(!view.primary.sensitive && !view.manage_sensitive);
    }

    #[test]
    fn busy_titles() {
        let titles = [
            (Busy::Operation(Operation::Connecting), "Connecting…"),
            (Busy::AwaitingAuthentication, "Awaiting authentication…"),
            (Busy::Operation(Operation::Disconnecting), "Disconnecting…"),
            (Busy::Operation(Operation::SavingProfile), "Saving profile…"),
            (
                Busy::Operation(Operation::RemovingProfile),
                "Deleting profile…",
            ),
            (
                Busy::Operation(Operation::CheckingBeforeExit),
                "Checking before exit",
            ),
            (Busy::Operation(Operation::Refreshing), "Refreshing"),
        ];
        for (busy, expected) in titles {
            assert_eq!(busy_title(busy), expected);
        }
        let mut state = initial();
        state.busy = Some(Busy::Operation(Operation::CheckingBeforeExit));
        let view = Window::new().render(&state);
        assert_eq!(view.status_text, "Checking before exit");
        assert_eq!(view.badge, Badge::Progress);
        assert_eq!(view.operation_note, None);
    }

    #[test]
    fn footer_note_names_the_desktop_tray() {
        let state = initial();
        assert_eq!(
            Window::new().render(&state).footer_note,
            "Close to keep running in the top bar."
        );
        let plasma = Window {
            platform: platform(Desktop::Kde),
            ..Window::new()
        };
        assert_eq!(
            plasma.render(&state).footer_note,
            "Close to keep running in the system tray."
        );
        let quitting = Window {
            close_behavior: CloseBehavior::Quit,
            ..Window::new()
        };
        assert_eq!(
            quitting.render(&state).footer_note,
            "Closing this window disconnects the VPN and quits."
        );
    }

    #[test]
    fn tray_mirrors_status_and_offers_disconnect_only_when_idle() {
        let window = Window::new();
        let mut state = initial();
        state.status = connected("Office VPN");
        let view = window.render(&state);
        assert_eq!(view.tray_text, "Connected · Office VPN");
        assert!(view.tray_disconnect_enabled && view.tray_connected);
        state.busy = Some(Busy::Operation(Operation::Disconnecting));
        let view = window.render(&state);
        assert_eq!(view.tray_text, "Disconnecting…");
        assert!(!view.tray_disconnect_enabled && view.tray_connected);
        state.busy = None;
        let quitting = Window {
            exit_requested: true,
            ..Window::new()
        };
        assert!(!quitting.render(&state).tray_disconnect_enabled);
        state.status = Status::new(ConnectionState::Disconnected);
        let view = window.render(&state);
        assert_eq!(view.tray_text, "Disconnected");
        assert!(!view.tray_disconnect_enabled && !view.tray_connected);
    }

    /// Port of `cancel_connection()`: disconnect an attempt FortiClient
    /// still reports when no operation is running, otherwise cancel the
    /// running one; either way close the open prompt.
    fn cancel_connection(harness: &mut Harness) {
        let state = harness.workflow.state();
        let unconfirmed = state.busy.is_none()
            && matches!(
                state.status.state,
                ConnectionState::Connecting | ConnectionState::AwaitingAuthentication
            );
        if unconfirmed {
            harness.workflow.disconnect();
        } else {
            harness.workflow.cancel();
        }
        // Closing the dialog drops its copy of the challenge.
        harness.challenges.clear();
    }

    /// Drive "Disconnect and Quit" the way `request_exit()` and
    /// `advance_exit()` do, returning every step taken. Like the window's
    /// `cli_seen`, FortiClient counts as seen once any state it reported
    /// was not unavailable.
    fn exit_steps(harness: &mut Harness) -> Vec<ExitStep> {
        let mut cli_seen = harness.workflow.state().status.state != ConnectionState::Unavailable;
        // `request_exit()` stops the current attempt first.
        cancel_connection(harness);
        let mut checked = false;
        let mut steps = Vec::new();
        loop {
            cli_seen |= harness.workflow.state().status.state != ConnectionState::Unavailable;
            let step = exit_step(harness.workflow.state(), checked, cli_seen);
            steps.push(step);
            assert!(steps.len() < 10, "exit never finished: {steps:?}");
            match step {
                ExitStep::Wait => harness.settle(),
                ExitStep::Check => {
                    checked = true;
                    harness.workflow.check_before_exit();
                }
                ExitStep::Disconnect => harness.workflow.disconnect(),
                ExitStep::Quit | ExitStep::Fail => return steps,
            }
        }
    }

    #[test]
    fn quit_disconnects_and_checks_status() {
        use ExitStep::{Check, Disconnect, Quit, Wait};
        let cli = FakeCli::new();
        cli.with(|fake| fake.current = connected("Office VPN"));
        let mut harness = Harness::new(cli);
        // Stale: FortiClient connected after the window last refreshed.
        harness.workflow.state_mut().status = Status::new(ConnectionState::Disconnected);
        let steps = exit_steps(&mut harness);
        assert_eq!(steps, [Check, Wait, Disconnect, Wait, Quit]);
        assert_eq!(
            harness.cli.with(|fake| fake.current.state),
            ConnectionState::Disconnected
        );
    }

    #[test]
    fn failed_disconnect_keeps_app_open() {
        use ExitStep::{Check, Disconnect, Fail, Wait};
        let cli = FakeCli::new();
        cli.with(|fake| {
            fake.current = connected("Office VPN");
            fake.disconnect_error = Some(VpnError::failed("failure"));
        });
        let mut harness = Harness::new(cli);
        harness.workflow.state_mut().status = Status::new(ConnectionState::Disconnected);
        let steps = exit_steps(&mut harness);
        assert_eq!(steps, [Check, Wait, Disconnect, Wait, Fail]);
        // On `Fail` the window stops quitting and shows why.
        harness.workflow.state_mut().error = EXIT_FAILED.to_owned();
        let view = Window::new().render(harness.workflow.state());
        assert_eq!(view.error.as_deref(), Some(EXIT_FAILED));
        assert_eq!(view.retry, Some(RetryAction::RetryDisconnect));
        assert!(view.tray_disconnect_enabled);
    }

    /// FortiClient answered earlier and stopped answering: the app may have
    /// connected and cannot confirm the disconnection, so it stays open.
    #[test]
    fn unconfirmed_disconnection_keeps_app_open() {
        use ExitStep::{Check, Fail, Wait};
        let cli = FakeCli::new();
        let mut harness = Harness::new(cli);
        harness.refresh();
        harness.cli.with(|fake| fake.unavailable = true);
        assert_eq!(exit_steps(&mut harness), [Check, Wait, Fail]);
    }

    /// FortiClient never answered, not even to the exit check (its service
    /// is stopped): this app cannot have connected, so it quits instead of
    /// trapping the user in a window that cannot close.
    #[test]
    fn never_reachable_client_quits_without_disconnecting() {
        use ExitStep::{Check, Quit, Wait};
        let cli = FakeCli::new();
        cli.with(|fake| fake.unavailable = true);
        let mut harness = Harness::new(cli);
        assert_eq!(exit_steps(&mut harness), [Check, Wait, Quit]);
    }

    /// `request_exit()` quits a fresh setup screen (FortiClient never seen)
    /// without consulting `exit_step`. When it is consulted, it quits too
    /// and never asks to disconnect.
    #[test]
    fn fresh_setup_exit_never_disconnects() {
        use ExitStep::{Check, Quit, Wait};
        let cli = FakeCli::new();
        cli.with(|fake| fake.missing = true);
        let mut harness = Harness::new(cli);
        harness.refresh();
        assert!(harness.workflow.state().setup_required);
        assert_eq!(exit_steps(&mut harness), [Check, Wait, Quit]);
    }

    #[test]
    fn quit_disconnects_an_attempt_left_pending() {
        use ExitStep::{Check, Quit, Wait};
        let pending = Status::with_profile(ConnectionState::Connecting, "Office VPN");
        let cli = FakeCli::new();
        cli.with(|fake| fake.current = pending.clone());
        let mut harness = Harness::new(cli);
        harness.refresh();
        assert_eq!(harness.workflow.state().status, pending);
        assert_eq!(harness.workflow.state().busy, None);
        // Nothing of this app runs, so "Disconnect and Quit" disconnects
        // the attempt FortiClient reports, then checks before quitting.
        let steps = exit_steps(&mut harness);
        assert_eq!(steps, [Wait, Check, Wait, Quit]);
        assert_eq!(
            harness.cli.with(|fake| fake.current.state),
            ConnectionState::Disconnected
        );
        assert_eq!(
            harness.completed,
            [
                (
                    Operation::Disconnecting,
                    true,
                    ConnectionState::Disconnected
                ),
                (
                    Operation::CheckingBeforeExit,
                    true,
                    ConnectionState::Disconnected
                ),
            ]
        );
    }

    #[test]
    fn quit_cancels_an_attempt_waiting_for_a_prompt() {
        use ExitStep::{Check, Quit, Wait};
        let mut harness = Harness::new(FakeCli::new());
        harness
            .workflow
            .connect(Profile::new("Office VPN"), String::new(), false);
        harness.pump(|harness| !harness.challenges.is_empty());
        assert_eq!(
            harness.workflow.state().busy,
            Some(Busy::AwaitingAuthentication)
        );
        let steps = exit_steps(&mut harness);
        assert_eq!(steps, [Wait, Check, Wait, Quit]);
        // Cancelled, not disconnected: no Disconnecting operation ran.
        assert_eq!(
            harness.completed,
            [
                (Operation::Connecting, false, ConnectionState::Disconnected),
                (
                    Operation::CheckingBeforeExit,
                    true,
                    ConnectionState::Disconnected
                ),
            ]
        );
    }

    #[test]
    fn exit_waits_for_operations_and_checks_first() {
        let mut state = initial();
        assert_eq!(exit_step(&state, false, true), ExitStep::Check);
        assert_eq!(exit_step(&state, true, true), ExitStep::Quit);
        state.busy = Some(Busy::AwaitingAuthentication);
        assert_eq!(exit_step(&state, false, true), ExitStep::Wait);
        assert_eq!(exit_step(&state, true, true), ExitStep::Wait);
        state.busy = None;
        state.status = connected("Office VPN");
        assert_eq!(exit_step(&state, true, true), ExitStep::Disconnect);
        state.status = Status::with_profile(ConnectionState::Connecting, "Office VPN");
        assert_eq!(exit_step(&state, true, true), ExitStep::Disconnect);
        state.error_origin = Some(Operation::Disconnecting);
        assert_eq!(exit_step(&state, true, true), ExitStep::Fail);
        state.error_origin = None;
        state.status = Status::unavailable();
        assert_eq!(exit_step(&state, true, true), ExitStep::Fail);
        // Never seen: nothing this app started can be up.
        assert_eq!(exit_step(&state, true, false), ExitStep::Quit);
        assert_eq!(exit_step(&state, false, false), ExitStep::Check);
    }
}

#[cfg(test)]
mod saved_password_and_first_check {
    use super::*;
    use crate::model::{ConnectionState, Status};
    use crate::platform::Desktop;
    use crate::platform::{DistroFamily, Elevation, OsRelease, Sandbox, SessionType};

    fn platform() -> Platform {
        Platform {
            os: OsRelease::default(),
            family: DistroFamily::Unknown,
            ubuntu_based: false,
            enterprise: false,
            image_based: false,
            desktop: Desktop::Gnome,
            desktop_name: "GNOME".into(),
            session: SessionType::Wayland,
            sandbox: Sandbox::None,
            architecture: "x86_64".into(),
            elevation: Elevation::Sudo,
            aur_helper: None,
        }
    }

    fn view(state: &State, selected: Option<&Profile>, first_check: bool) -> MainView {
        view_with(state, selected, first_check, false, None)
    }

    fn view_with(
        state: &State,
        selected: Option<&Profile>,
        first_check: bool,
        tray_missing: bool,
        window_notice: Option<&str>,
    ) -> MainView {
        main_view(MainInputs {
            state,
            selected,
            exit_requested: false,
            profile_dialog_open: false,
            high_contrast: false,
            close_behavior: CloseBehavior::Hide,
            platform: &platform(),
            first_check,
            tray_missing,
            window_notice,
        })
    }

    fn disconnected_with(profiles: Vec<Profile>) -> State {
        State {
            status: Status::new(ConnectionState::Disconnected),
            profiles,
            ..State::default()
        }
    }

    #[test]
    fn footer_tells_the_truth_without_a_tray() {
        let state = disconnected_with(vec![Profile::new("Office")]);
        let with_tray = view_with(&state, state.profiles.first(), false, false, None);
        assert_eq!(
            with_tray.footer_note,
            "Close to keep running in the top bar."
        );
        let without = view_with(&state, state.profiles.first(), false, true, None);
        assert_eq!(
            without.footer_note,
            "Without a top bar icon, this window stays open when closed. Use Disconnect and Quit to exit."
        );
    }

    #[test]
    fn window_notices_never_replace_the_operation_error() {
        let mut state = disconnected_with(vec![Profile::new("Office")]);
        let notice = "The top bar icon is unavailable, so the window stays open.";
        let alone = view_with(&state, state.profiles.first(), false, true, Some(notice));
        assert_eq!(
            alone.notice,
            Some(("Window stays open".to_owned(), NoticeKind::Warning))
        );
        assert_eq!(alone.warning.as_deref(), Some(notice));
        assert!(alone.error.is_none() && !alone.retry_visible);

        state.error = "The VPN server rejected authentication.".to_owned();
        state.error_origin = Some(Operation::Connecting);
        let both = view_with(&state, state.profiles.first(), false, true, Some(notice));
        assert_eq!(
            both.notice,
            Some(("Connection issue".to_owned(), NoticeKind::Error))
        );
        assert_eq!(both.error.as_deref(), Some(state.error.as_str()));
        assert_eq!(both.warning.as_deref(), Some(notice));
        assert_eq!(both.retry, Some(RetryAction::TryAgain));
        assert!(both.retry_visible);

        // The setup screen hides operation errors, not the window's notice.
        state.setup_required = true;
        let setup = view_with(&state, None, false, true, Some(notice));
        assert!(setup.error.is_none());
        assert_eq!(setup.warning.as_deref(), Some(notice));
    }

    #[test]
    fn empty_profile_lists_explain_what_to_do() {
        let empty = disconnected_with(Vec::new());
        let none = view(&empty, None, false);
        assert_eq!(
            none.profiles_hint.as_deref(),
            Some("No VPN profiles yet. Create one with Manage Profile → New.")
        );
        assert_eq!(
            none.primary_hint.as_deref(),
            Some("Create a VPN profile to connect.")
        );
        assert!(!none.primary.sensitive && !none.credentials_sensitive);
        assert!(none.new_profile.sensitive);

        let listed = disconnected_with(vec![Profile::new("Office")]);
        let unselected = view(&listed, None, false);
        assert!(unselected.profiles_hint.is_none());
        assert_eq!(
            unselected.primary_hint.as_deref(),
            Some("Select a VPN profile to connect.")
        );
        let selected = view(&listed, listed.profiles.first(), false);
        assert!(selected.primary_hint.is_none() && selected.credentials_sensitive);
        assert!(view(&empty, None, true).profiles_hint.is_none());
    }

    #[test]
    fn unavailable_client_explains_locked_profile_actions() {
        let state = State {
            profiles: vec![Profile::new("Office")],
            ..State::default()
        };
        let unavailable = view(&state, state.profiles.first(), false);
        for control in [
            &unavailable.new_profile,
            &unavailable.edit_profile,
            &unavailable.delete_profile,
            &unavailable.update_password,
        ] {
            assert!(!control.sensitive);
            assert_eq!(control.tooltip, "FortiClient is unavailable.");
        }
    }

    #[test]
    fn saved_password_is_reported_with_an_explanation() {
        let state = State {
            status: Status::new(ConnectionState::Disconnected),
            ..State::default()
        };
        let saved = Profile::with_fields("Office", [("Save Password", "Enabled")]);
        let unsaved = Profile::with_fields("Office", [("Save Password", "Disabled")]);
        let with = view(&state, Some(&saved), false);
        assert!(with.saved_password);
        assert!(with.remember_subtitle.contains("Update Saved Password"));
        let without = view(&state, Some(&unsaved), false);
        assert!(!without.saved_password);
        assert_eq!(
            without.remember_subtitle,
            "Stored by FortiClient, never by this app"
        );
        assert!(!view(&state, None, false).saved_password);
    }

    #[test]
    fn first_check_shows_progress_instead_of_unavailable() {
        let state = State::default();
        let before = view(&state, None, true);
        assert_eq!(before.status_text, CHECKING);
        assert_eq!(before.tray_text, CHECKING);
        assert_eq!(before.badge, Badge::Progress);
        assert!(before.spinner_active && !before.status_icon_visible);
        assert!(!before.setup_help_visible);
        assert_eq!(before.operation_note, None);
        let after = view(&state, None, false);
        assert_eq!(after.status_text, "FortiClient unavailable");
        assert!(after.setup_help_visible);
    }
}
