//! The FortiClient CLI boundary: no toolkit, no shell.
//!
//! Started as a port of the `CLI` class in the Python 0.1.0
//! `forti_vpn/backend.py` and verified by the integration tests in
//! `crates/forti-vpn-testkit`. Additions: the stopped-service message, the
//! package-database version fallback, and the Flatpak host runner.
//! Interactive commands run on a pseudo-terminal with echo disabled, in a
//! new session; answers are written to the PTY and never appear in
//! arguments, logs, or errors.
use std::borrow::Cow;
use std::env;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime};

use regex::Regex;
use rustix::fs::{Mode, OFlags};
use rustix::process::{Pid, Signal, kill_process_group};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};
use zeroize::Zeroizing;

use crate::backend::{Ask, Backend, Log, OperationContext};
use crate::cancel::CancelToken;
use crate::error::{CANCELLED, CLI_NOT_FOUND, VpnError};
use crate::host::{
    Captured, HostRunner, deadline_after, readable, remaining, run_captured, wait_until,
};
use crate::model::{
    AuthenticationMode, CertificateKind, ClientInfo, ConnectionState, Profile, ProfileSettings,
    PromptKind, Secret, Status,
};
use crate::parse::{
    GENERIC_FAILURE, SAVE_PASSWORD_FIELD, SERVICE_STOPPED, STORAGE_WARNING, auth_prompts,
    cli_failure, cli_messages, edit_prompts, normalize_newlines, parse_dpkg_version,
    parse_package_version, parse_profile, parse_profiles, parse_status, parse_version,
    service_stopped, vpn_log_messages,
};

pub const DEFAULT_EXECUTABLE: &str = "forticlient";
/// Where the official packages install the CLI, with or without a `PATH`
/// symlink.
pub const OFFICIAL_CLI_PATH: &str = "/opt/forticlient/forticlient-cli";
/// Where the official packages install FortiClient.
const OFFICIAL_INSTALL_DIRECTORY: &str = "/opt/forticlient";
pub const DEFAULT_VPN_LOG: &str = "/var/log/forticlient/sslvpn.log";
/// Environment variable that overrides the executable (development, tests,
/// unusual installs).
pub const CLI_OVERRIDE_VARIABLE: &str = "FORTI_VPN_CLI";

const NO_RESPONSE: &str = "FortiClient did not respond within ten seconds.";
const START_FAILURE: &str =
    "FortiClient could not be started. Check its installation and permissions.";
const TIMED_OUT: &str = "The operation timed out. Retry when you are ready to authenticate.";
const ANSWER_TOO_LATE: &str = "The operation timed out.";
const MULTILINE_ANSWER: &str = "Responses must contain a single line.";
const UNSUPPORTED_PROMPT: &str =
    "FortiClient requested an unsupported response. The operation was stopped safely.";
const VERSION_UNKNOWN: &str = "Could not determine";
const VERSION_UNAVAILABLE: &str = "Unavailable";

/// A package-database tool and the queries that ask it for the installed
/// FortiClient version, in order.
struct PackageTool {
    name: &'static str,
    queries: &'static [&'static [&'static str]],
    /// The version in the tool's answer, if it names one.
    version: fn(&str) -> Option<String>,
}

/// Tried in order when the CLI reports no version: Debian and Ubuntu,
/// RPM-based distributions, then Arch (the AUR package, then the plain
/// name).
const PACKAGE_TOOLS: [PackageTool; 3] = [
    PackageTool {
        name: "dpkg-query",
        // The status tells an installed package from one that was removed
        // with its configuration kept.
        queries: &[&["-W", "-f=${db:Status-Abbrev}${Version}", "forticlient"]],
        version: parse_dpkg_version,
    },
    PackageTool {
        name: "rpm",
        queries: &[&["-q", "--qf", "%{VERSION}", "forticlient"]],
        version: parse_package_version,
    },
    PackageTool {
        name: "pacman",
        queries: &[&["-Q", "forticlient-vpn"], &["-Q", "forticlient"]],
        version: parse_package_version,
    },
];

/// Settings `vpn edit` must leave alone when an existing profile is saved.
const ADVANCED_SETTINGS: [&str; 3] = [
    "Single Sign On (SSO) for VPN Tunnel",
    "Auto Connect",
    "Always Up",
];

const STATUS_INTERVAL: Duration = Duration::from_millis(250);
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_INTERVAL: Duration = Duration::from_millis(100);
/// How long FortiClient may stay silent after unanswerable output before
/// it counts as an unsupported question.
const QUIET_PERIOD: Duration = Duration::from_millis(600);
const EXIT_GRACE: Duration = Duration::from_secs(2);
const CHUNK_SIZE: usize = 4096;
/// Characters of terminal output kept for prompt matching.
const TRANSCRIPT_LIMIT: usize = 16384;
/// Bytes of the VPN log read after an operation.
const LOG_TAIL: u64 = 65536;

static ANSI_SEQUENCE: LazyLock<Regex> = LazyLock::new(|| compile(r"\x1b\[[0-?]*[ -/]*[@-~]"));
static QUESTION_WORD: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?i)(?:input|enter|accept|confirm|\[y/n\])"));

fn compile(pattern: &str) -> Regex {
    // Constant patterns covered by the tests below.
    Regex::new(pattern).expect("built-in patterns are valid")
}

#[derive(Debug, Clone)]
pub struct CliConfig {
    /// A name looked up on `PATH`, or a path.
    pub executable: String,
    /// Limit for non-interactive commands (`list`, `status`, `view`…).
    pub read_timeout: Duration,
    /// Limit for interactive commands (`connect`, `edit`).
    pub interactive_timeout: Duration,
    pub vpn_log: PathBuf,
    pub runner: HostRunner,
}

impl Default for CliConfig {
    /// `forticlient`, 10 s, 120 s, the default VPN log, detected runner.
    fn default() -> Self {
        Self {
            executable: DEFAULT_EXECUTABLE.to_owned(),
            read_timeout: Duration::from_secs(10),
            interactive_timeout: Duration::from_secs(120),
            vpn_log: PathBuf::from(DEFAULT_VPN_LOG),
            runner: HostRunner::detect(),
        }
    }
}

impl CliConfig {
    /// Defaults, with `executable` taken from `FORTI_VPN_CLI` when set.
    pub fn from_environment() -> Self {
        Self::with_executable(env::var(CLI_OVERRIDE_VARIABLE).ok())
    }

    /// Defaults, with `executable` replaced when `executable` is non-empty.
    fn with_executable(executable: Option<String>) -> Self {
        let mut config = Self::default();
        if let Some(executable) = executable.filter(|value| !value.is_empty()) {
            config.executable = executable;
        }
        config
    }
}

/// Where [`Cli::information`] looks for FortiClient's package. Tests point
/// it at fake package tools, since they must not change the process
/// environment.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDatabase {
    /// Where the official packages install FortiClient: only a CLI whose
    /// canonical path lies inside it came from a package.
    pub install_directory: PathBuf,
    /// Searched for `dpkg-query`, `rpm`, and `pacman` instead of `PATH`.
    pub search_path: Option<OsString>,
}

impl Default for PackageDatabase {
    /// `/opt/forticlient`, with the tools looked up on `PATH`.
    fn default() -> Self {
        Self {
            install_directory: PathBuf::from(OFFICIAL_INSTALL_DIRECTORY),
            search_path: None,
        }
    }
}

pub struct Cli {
    config: CliConfig,
    packages: PackageDatabase,
}

impl Cli {
    pub fn new(config: CliConfig) -> Self {
        Self {
            config,
            packages: PackageDatabase::default(),
        }
    }

    /// Look FortiClient's package up in `packages` instead of the host's
    /// package database.
    #[doc(hidden)]
    pub fn with_package_database(self, packages: PackageDatabase) -> Self {
        Self { packages, ..self }
    }

    pub fn config(&self) -> &CliConfig {
        &self.config
    }

    /// The executable to run, falling back to [`OFFICIAL_CLI_PATH`] when the
    /// configured executable is the default name.
    pub fn resolve_executable(&self) -> Result<PathBuf, VpnError> {
        resolve_with(&self.config.executable, |name, fallbacks| {
            self.config.runner.find_executable(name, fallbacks)
        })
    }

    /// Run `forticlient vpn <args>` without a terminal and return stdout,
    /// with `\r\n` and lone `\r` line endings turned into `\n` as Python's
    /// `text=True` does.
    pub fn read(&self, args: &[&str]) -> Result<String, VpnError> {
        let mut command = self.vpn_command(args)?;
        let output = match run_captured(&mut command, self.config.read_timeout) {
            Ok(Some(output)) => output,
            Ok(None) => return Err(VpnError::failed(NO_RESPONSE)),
            Err(error) => return Err(start_error(&error)),
        };
        let stdout = normalize_newlines(&String::from_utf8_lossy(&output.stdout));
        if !output.status.success() {
            return Err(read_failure(operation_name(args), &stdout));
        }
        Ok(stdout)
    }

    /// Poll status until `expected`, while FortiClient reports a transitional
    /// state, until `deadline` or cancellation.
    pub fn wait_status(
        &self,
        expected: ConnectionState,
        cancel: &CancelToken,
        deadline: Instant,
    ) -> Result<Status, VpnError> {
        poll_status(expected, cancel, deadline, || self.status())
    }

    /// Refuse to change managed profiles or the active connection's profile.
    pub fn guard_profile(&self, profile: &Profile) -> Result<(), VpnError> {
        if profile.managed {
            return Err(VpnError::failed(
                "Company-managed profiles cannot be changed in this app.",
            ));
        }
        let status = self.status()?;
        let affected = status
            .profile
            .as_ref()
            .is_none_or(|active| *active == profile.name);
        if status.state != ConnectionState::Disconnected && affected {
            return Err(VpnError::failed(
                "Disconnect the active VPN before changing this profile.",
            ));
        }
        Ok(())
    }

    /// Drive `forticlient vpn <args>` on a PTY, answering `prompts` with
    /// `answer` until the command exits; then translate failures.
    pub fn interactive(
        &self,
        args: &[&str],
        prompts: &[(PromptKind, Regex)],
        answer: &Ask<'_>,
        context: &OperationContext<'_>,
    ) -> Result<(), VpnError> {
        let name = operation_name(args);
        let checkpoint = (name == "connect").then(|| LogCheckpoint::take(&self.config.vpn_log));
        let mut terminal = Terminal::spawn(self.vpn_command(args)?)?;
        (context.log)(&format!("Started FortiClient {name}."));
        let output = converse(&mut terminal, prompts, answer, context)?;
        let code = terminal.exit_code()?;
        (context.log)(&format!("FortiClient CLI exited with code {code}."));
        if let Some(failure) = cli_failure(&output, code) {
            let details = checkpoint
                .map(|checkpoint| checkpoint.messages(&self.config.vpn_log))
                .unwrap_or_default();
            for detail in &details {
                (context.log)(detail);
            }
            return Err(VpnError::failed(
                details.first().copied().unwrap_or(failure),
            ));
        }
        (context.log)(&format!(
            "FortiClient finished {name}; verifying the result."
        ));
        Ok(())
    }

    /// `<executable> vpn <args>`, started on the host.
    fn vpn_command(&self, args: &[&str]) -> Result<Command, VpnError> {
        let mut command = self.config.runner.command(&self.resolve_executable()?);
        command.arg("vpn").args(args);
        Ok(command)
    }

    /// FortiClient's version in the package database, when `executable` is
    /// the packaged CLI. Every lookup and query shares one deadline.
    fn packaged_version(&self, executable: &Path) -> Option<String> {
        let runner = self.config.runner;
        let deadline = deadline_after(self.config.read_timeout);
        let install_directory = &self.packages.install_directory;
        if !is_packaged(runner, executable, install_directory, deadline) {
            return None;
        }
        let search_path = self.packages.search_path.as_deref();
        package_version(
            |tool| runner.find_executable_in(tool, search_path, &[], deadline),
            |program, args| run_query(runner, program, args, deadline),
        )
    }
}

impl Backend for Cli {
    fn interactive_timeout(&self) -> Duration {
        self.config.interactive_timeout
    }

    fn status(&self) -> Result<Status, VpnError> {
        parse_status(&self.read(&["status"])?)
    }

    fn list_profiles(&self) -> Result<Vec<Profile>, VpnError> {
        parse_profiles(&self.read(&["list"])?)
    }

    fn view(&self, profile: &Profile) -> Result<Profile, VpnError> {
        parse_profile(
            &profile.name,
            &self.read(&["view", &profile.name])?,
            profile.managed,
        )
    }

    fn connect(
        &self,
        profile: &Profile,
        username: &str,
        remember: bool,
        context: &OperationContext<'_>,
    ) -> Result<Status, VpnError> {
        if self.status()?.state != ConnectionState::Disconnected {
            return Err(VpnError::failed(
                "A VPN operation is already active. Disconnect it before connecting again.",
            ));
        }
        self.interactive(
            &connect_arguments(profile, username, remember),
            auth_prompts(),
            context.ask,
            context,
        )?;
        self.wait_status(ConnectionState::Connected, context.cancel, context.deadline)
    }

    fn disconnect(&self) -> Result<Status, VpnError> {
        self.read(&["disconnect"])?;
        self.wait_status(
            ConnectionState::Disconnected,
            &CancelToken::new(),
            deadline_after(DISCONNECT_TIMEOUT),
        )
    }

    fn save_profile(
        &self,
        settings: &ProfileSettings,
        existing: Option<&Profile>,
        context: &OperationContext<'_>,
    ) -> Result<Profile, VpnError> {
        settings.validate()?;
        let profile = existing
            .cloned()
            .unwrap_or_else(|| Profile::new(settings.name.clone()));
        self.guard_profile(&profile)?;
        let names = self.list_profiles()?;
        if existing.is_none() && names.iter().any(|known| known.name == settings.name) {
            return Err(VpnError::failed("A profile with this name already exists."));
        }
        if existing.is_some_and(|existing| existing.name != settings.name) {
            return Err(VpnError::failed("Profile renaming is not supported."));
        }
        let respond = |kind: PromptKind| match edit_answer(settings, existing.is_some(), kind) {
            Some(value) => Ok(Some(Secret::new(value.to_owned()))),
            None => (context.ask)(kind),
        };
        self.interactive(&["edit", &settings.name], edit_prompts(), &respond, context)?;
        let saved = self.view(&profile)?;
        verify_saved(settings, existing, &saved)?;
        Ok(saved)
    }

    fn remove(&self, profile: &Profile) -> Result<(), VpnError> {
        self.guard_profile(profile)?;
        self.read(&["remove", &profile.name])?;
        if self
            .list_profiles()?
            .iter()
            .any(|known| known.name == profile.name)
        {
            return Err(VpnError::failed("FortiClient did not remove the profile."));
        }
        Ok(())
    }

    fn information(&self) -> ClientInfo {
        let Ok(executable) = self.resolve_executable() else {
            return ClientInfo::unavailable();
        };
        let mut command = self.config.runner.command(&executable);
        command.arg("version");
        let reported = run_captured(&mut command, self.config.read_timeout);
        ClientInfo {
            version: client_version(reported, || self.packaged_version(&executable)),
            path: display_path(&executable, env::home_dir().as_deref()),
        }
    }
}

/// Find `executable` with `find(name, fallbacks)`, which is offered the
/// official location only for the default name.
fn resolve_with(
    executable: &str,
    find: impl FnOnce(&str, &[&str]) -> Option<PathBuf>,
) -> Result<PathBuf, VpnError> {
    find(executable, executable_fallbacks(executable))
        .ok_or_else(|| VpnError::not_found(CLI_NOT_FOUND))
}

/// Locations tried after `PATH` for `executable`: the official package may
/// be installed without a `PATH` symlink, but only the default name means
/// that package.
fn executable_fallbacks(executable: &str) -> &'static [&'static str] {
    if executable == DEFAULT_EXECUTABLE {
        &[OFFICIAL_CLI_PATH]
    } else {
        &[]
    }
}

/// The first argument names the operation in diagnostics and errors.
fn operation_name<'a>(args: &[&'a str]) -> &'a str {
    args.first().copied().unwrap_or_default()
}

/// Why `forticlient vpn <operation>` exited unsuccessfully. Only fixed
/// descriptions are returned, never the output itself.
fn read_failure(operation: &str, stdout: &str) -> VpnError {
    if service_stopped(stdout) {
        return VpnError::failed(SERVICE_STOPPED);
    }
    VpnError::failed(format!(
        "FortiClient could not complete {operation}. Check its service and permissions."
    ))
}

fn start_error(error: &io::Error) -> VpnError {
    if error.kind() == io::ErrorKind::NotFound {
        VpnError::not_found(CLI_NOT_FOUND)
    } else {
        VpnError::failed(START_FAILURE)
    }
}

fn cancelled() -> VpnError {
    VpnError::cancelled(CANCELLED)
}

fn poll_status(
    expected: ConnectionState,
    cancel: &CancelToken,
    deadline: Instant,
    mut status: impl FnMut() -> Result<Status, VpnError>,
) -> Result<Status, VpnError> {
    loop {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let current = status()?;
        if current.state == expected {
            return Ok(current);
        }
        let transitional = matches!(
            current.state,
            ConnectionState::Connecting
                | ConnectionState::Disconnecting
                | ConnectionState::AwaitingAuthentication
        );
        if !transitional || Instant::now() >= deadline {
            return Err(VpnError::failed(format!(
                "FortiClient did not confirm {}. Refresh status and try again.",
                expected.as_str()
            )));
        }
        cancel.wait_timeout(STATUS_INTERVAL);
    }
}

fn connect_arguments<'a>(profile: &'a Profile, username: &'a str, remember: bool) -> Vec<&'a str> {
    // --username is advertised by help but rejected by the installed delegate.
    let mut args = vec!["connect", profile.name.as_str()];
    if !username.is_empty() {
        args.extend(["-u", username]);
    }
    if profile.field(SAVE_PASSWORD_FIELD) != Some("Enabled") || remember {
        args.push("-p");
    }
    if remember {
        args.push("-s");
    }
    args
}

/// The answer to a `vpn edit` prompt taken from `settings`, or `None` when
/// the user must answer it.
fn edit_answer(settings: &ProfileSettings, editing: bool, kind: PromptKind) -> Option<&str> {
    match kind {
        PromptKind::Gateway => Some(&settings.gateway),
        PromptKind::Authentication => Some(settings.authentication.cli_value()),
        PromptKind::Username => Some(&settings.username),
        PromptKind::CertificateType => Some(settings.certificate_kind.cli_value()),
        PromptKind::Certificate => Some(&settings.certificate),
        // An empty answer keeps the certificate already saved in the profile.
        PromptKind::CertificatePassword | PromptKind::VerifyPassword
            if editing && settings.certificate_kind == CertificateKind::Keep =>
        {
            Some("")
        }
        PromptKind::CertificatePassword
        | PromptKind::VerifyPassword
        | PromptKind::Password
        | PromptKind::Mfa => None,
    }
}

/// Confirm FortiClient saved what was requested, and nothing else changed.
fn verify_saved(
    settings: &ProfileSettings,
    existing: Option<&Profile>,
    saved: &Profile,
) -> Result<(), VpnError> {
    if saved.gateway() != settings.gateway {
        return Err(VpnError::failed(
            "FortiClient did not save the requested gateway. Review the profile before retrying.",
        ));
    }
    if settings.authentication == AuthenticationMode::Save && saved.username() != settings.username
    {
        return Err(VpnError::failed(
            "FortiClient did not save the requested username.",
        ));
    }
    if settings.certificate_kind.needs_path()
        && saved.field("Client Certificate") != Some(settings.certificate.as_str())
    {
        return Err(VpnError::failed(
            "FortiClient did not save the requested certificate. Review the profile.",
        ));
    }
    if let Some(existing) = existing
        && ADVANCED_SETTINGS
            .iter()
            .any(|key| saved.field(key) != existing.field(key))
    {
        return Err(VpnError::failed(
            "FortiClient changed an advanced setting. Review the profile before connecting.",
        ));
    }
    Ok(())
}

/// The version shown for a `forticlient version` run: the one it reports,
/// else the one `packaged()` finds in the package database, as while the
/// service is stopped (the CLI then reports none) or when the CLI does not
/// answer at all. Raw output is never returned: only a parsed version
/// number or a fixed description, "Unavailable" when nothing answered.
fn client_version(
    reported: io::Result<Option<Captured>>,
    packaged: impl FnOnce() -> Option<String>,
) -> String {
    let answered = matches!(reported, Ok(Some(_)));
    reported
        .ok()
        .flatten()
        .filter(|output| output.status.success())
        .and_then(|output| parse_version(&String::from_utf8_lossy(&output.stdout)))
        .or_else(packaged)
        .unwrap_or_else(|| {
            let description = if answered {
                VERSION_UNKNOWN
            } else {
                VERSION_UNAVAILABLE
            };
            description.to_owned()
        })
}

/// Whether `executable` is the CLI the official packages install: its
/// canonical path lies inside `install_directory`. A CLI anywhere else (a
/// `FORTI_VPN_CLI` override, a wrapper, a test double) is not described by
/// the package's version.
fn is_packaged(
    runner: HostRunner,
    executable: &Path,
    install_directory: &Path,
    deadline: Instant,
) -> bool {
    let Some(executable) = runner.canonical_path(executable, deadline) else {
        return false;
    };
    runner
        .canonical_path(install_directory, deadline)
        .is_some_and(|directory| executable.starts_with(directory))
}

/// The version from the first package query that answers with one.
/// `find(tool)` locates a tool, once per tool, or returns `None` when it
/// is not installed; `query(tool, args)` returns the tool's output when it
/// succeeded.
fn package_version<T>(
    mut find: impl FnMut(&str) -> Option<T>,
    mut query: impl FnMut(&T, &[&str]) -> Option<String>,
) -> Option<String> {
    PACKAGE_TOOLS.iter().find_map(|tool| {
        let program = find(tool.name)?;
        tool.queries
            .iter()
            .find_map(|args| query(&program, args).and_then(|output| (tool.version)(&output)))
    })
}

/// Run one package query on the host, returning its output when it
/// succeeds before `deadline`. Nothing starts once the deadline has passed.
fn run_query(
    runner: HostRunner,
    program: &Path,
    args: &[&str],
    deadline: Instant,
) -> Option<String> {
    let mut command = runner.command(program);
    command.args(args);
    match run_captured(&mut command, remaining(deadline)?) {
        Ok(Some(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        _ => None,
    }
}

/// The executable as shown to users, with the home directory shortened to
/// `~`.
fn display_path(executable: &Path, home: Option<&Path>) -> String {
    let relative = home
        .filter(|home| home.is_absolute() && home.parent().is_some())
        .and_then(|home| executable.strip_prefix(home).ok())
        .filter(|relative| !relative.as_os_str().is_empty());
    match relative {
        Some(relative) => format!("~/{}", relative.display()),
        None => executable.display().to_string(),
    }
}

/// Answer `prompts` until FortiClient exits; returns the output that
/// followed the last answer.
fn converse(
    terminal: &mut Terminal,
    prompts: &[(PromptKind, Regex)],
    answer: &Ask<'_>,
    context: &OperationContext<'_>,
) -> Result<String, VpnError> {
    let mut buffer = [0; CHUNK_SIZE];
    let mut transcript = Transcript::default();
    let mut reported = Vec::new();
    let mut last_data = Instant::now();
    loop {
        if context.cancel.is_cancelled() {
            return Err(cancelled());
        }
        if Instant::now() > context.deadline {
            context.cancel.cancel();
            return Err(VpnError::failed(TIMED_OUT));
        }
        match terminal
            .receive(&mut buffer, READ_INTERVAL)
            .map_err(|_| VpnError::failed(GENERIC_FAILURE))?
        {
            Received::Closed => break,
            Received::Data(count) => {
                transcript.push(&buffer[..count]);
                last_data = Instant::now();
            }
            Received::Nothing => {}
        }
        report_messages(transcript.text(), &mut reported, context.log);
        if let Some(kind) = matching_prompt(prompts, transcript.text()) {
            respond(terminal, kind, answer, context)?;
            transcript.clear();
        } else if last_data.elapsed() > QUIET_PERIOD && awaits_unsupported_answer(transcript.text())
        {
            return Err(VpnError::failed(UNSUPPORTED_PROMPT));
        }
        if terminal.has_exited() && !terminal.has_pending_output() {
            break;
        }
    }
    Ok(transcript.into_text())
}

/// Log each recognised CLI message once per operation.
fn report_messages(text: &str, reported: &mut Vec<&'static str>, log: &Log<'_>) {
    for description in cli_messages(text) {
        if reported.contains(&description) {
            continue;
        }
        reported.push(description);
        if description == STORAGE_WARNING {
            log(&format!("Warning: {description}"));
        } else {
            log(description);
        }
    }
}

fn matching_prompt(prompts: &[(PromptKind, Regex)], text: &str) -> Option<PromptKind> {
    prompts
        .iter()
        .find(|(_, pattern)| pattern.is_match(text))
        .map(|&(kind, _)| kind)
}

/// Whether the last line looks like a question none of the prompts covers.
fn awaits_unsupported_answer(text: &str) -> bool {
    let tail = text.trim_end().rsplit('\n').next().unwrap_or_default();
    !tail.is_empty() && (tail.ends_with([':', '?', ']']) || QUESTION_WORD.is_match(tail))
}

fn respond(
    terminal: &Terminal,
    kind: PromptKind,
    answer: &Ask<'_>,
    context: &OperationContext<'_>,
) -> Result<(), VpnError> {
    (context.log)(&format!("FortiClient requested {}.", kind.description()));
    let Some(value) = answer(kind)? else {
        return Err(cancelled());
    };
    if context.cancel.is_cancelled() {
        return Err(cancelled());
    }
    if value.contains(['\r', '\n', '\0']) {
        return Err(VpnError::failed(MULTILINE_ANSWER));
    }
    if Instant::now() > context.deadline {
        return Err(VpnError::failed(ANSWER_TOO_LATE));
    }
    // Sized up front so the secret is never copied by a reallocation.
    let mut line = Zeroizing::new(Vec::with_capacity(value.len() + 1));
    line.extend_from_slice(value.as_bytes());
    line.push(b'\n');
    terminal
        .send(&line)
        .map_err(|_| VpnError::failed(GENERIC_FAILURE))
}

/// Terminal output since the last answer, as text for prompt matching.
#[derive(Default)]
struct Transcript {
    text: String,
    /// The start of a multi-byte character split across reads.
    partial: Vec<u8>,
}

impl Transcript {
    fn push(&mut self, bytes: &[u8]) {
        let decoded = self.decode(bytes);
        self.text.push_str(&decoded.replace('\r', ""));
        // The whole text is cleaned, so a sequence split across reads is
        // removed once it is complete.
        let cleaned = match ANSI_SEQUENCE.replace_all(&self.text, "") {
            Cow::Owned(cleaned) => Some(cleaned),
            Cow::Borrowed(_) => None,
        };
        if let Some(cleaned) = cleaned {
            self.text = cleaned;
        }
        keep_last_chars(&mut self.text, TRANSCRIPT_LIMIT);
    }

    /// Lossy UTF-8 decoding that keeps an incomplete trailing character for
    /// the next read.
    fn decode(&mut self, bytes: &[u8]) -> String {
        let mut input = std::mem::take(&mut self.partial);
        input.extend_from_slice(bytes);
        let mut text = String::with_capacity(input.len());
        let mut chunks = input.utf8_chunks().peekable();
        while let Some(chunk) = chunks.next() {
            text.push_str(chunk.valid());
            let invalid = chunk.invalid();
            if invalid.is_empty() {
                continue;
            }
            let truncated =
                std::str::from_utf8(invalid).is_err_and(|error| error.error_len().is_none());
            if chunks.peek().is_none() && truncated {
                self.partial = invalid.to_vec();
            } else {
                text.push(char::REPLACEMENT_CHARACTER);
            }
        }
        text
    }

    fn text(&self) -> &str {
        &self.text
    }

    fn clear(&mut self) {
        self.text.clear();
    }

    fn into_text(self) -> String {
        self.text
    }
}

fn keep_last_chars(text: &mut String, limit: usize) {
    if text.len() <= limit {
        return;
    }
    if let Some((start, _)) = text.char_indices().rev().nth(limit - 1) {
        text.drain(..start);
    }
}

enum Received {
    Data(usize),
    Nothing,
    Closed,
}

/// FortiClient on a pseudo-terminal in a session of its own. Dropping it
/// stops the session if FortiClient is still running.
struct Terminal {
    master: File,
    child: Child,
}

impl Terminal {
    fn spawn(mut command: Command) -> Result<Self, VpnError> {
        let (master, slave) = open_pty().map_err(|_| VpnError::failed(START_FAILURE))?;
        attach(&mut command, slave).map_err(|_| VpnError::failed(START_FAILURE))?;
        let child = command.spawn().map_err(|error| start_error(&error));
        // Dropping the command closes the parent's copies of the slave, so the
        // master reports end of file once FortiClient's side is closed.
        drop(command);
        Ok(Self {
            master: File::from(master),
            child: child?,
        })
    }

    fn receive(&self, buffer: &mut [u8], timeout: Duration) -> io::Result<Received> {
        if !readable(&self.master, timeout)? {
            return Ok(Received::Nothing);
        }
        match (&self.master).read(buffer) {
            Ok(0) => Ok(Received::Closed),
            Ok(count) => Ok(Received::Data(count)),
            // Linux reports a closed slave side as EIO.
            Err(error) if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => {
                Ok(Received::Closed)
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(Received::Nothing),
            Err(error) => Err(error),
        }
    }

    fn send(&self, bytes: &[u8]) -> io::Result<()> {
        (&self.master).write_all(bytes)
    }

    fn has_exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    fn has_pending_output(&self) -> bool {
        readable(&self.master, Duration::ZERO).unwrap_or(false)
    }

    fn exit_code(&mut self) -> Result<i32, VpnError> {
        match wait_until(&mut self.child, deadline_after(EXIT_GRACE)) {
            Ok(Some(status)) => Ok(exit_code(status)),
            Ok(None) | Err(_) => Err(VpnError::failed(GENERIC_FAILURE)),
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(None)) {
            return;
        }
        // FortiClient leads its own session, so its process group id is its pid.
        let group = Pid::from_child(&self.child);
        let _ = kill_process_group(group, Signal::TERM);
        if !matches!(
            wait_until(&mut self.child, deadline_after(EXIT_GRACE)),
            Ok(Some(_))
        ) {
            let _ = kill_process_group(group, Signal::KILL);
            let _ = self.child.wait();
        }
    }
}

fn open_pty() -> io::Result<(OwnedFd, OwnedFd)> {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    let name = ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(
        name,
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    // Answers must never be echoed back into the transcript.
    let mut attributes = tcgetattr(&slave)?;
    attributes
        .local_modes
        .remove(LocalModes::ECHO | LocalModes::ECHONL);
    tcsetattr(&slave, OptionalActions::Now, &attributes)?;
    Ok((master, slave))
}

/// Give `command` the slave as its standard streams and a new session, so
/// it has no access to the app's terminal and can be stopped as a group.
fn attach(command: &mut Command, slave: OwnedFd) -> io::Result<()> {
    command
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // rustix 1.1 has no `close_range`, so other inherited descriptors are
    // not closed here; only close-on-exec keeps them from FortiClient (std
    // and `open_pty` always set it).
    // SAFETY: the hook runs in the forked child before exec, where only
    // async-signal-safe operations are allowed. `setsid` is a single system
    // call and converting its error does not allocate.
    unsafe {
        command.pre_exec(|| rustix::process::setsid().map(drop).map_err(io::Error::from));
    }
    Ok(())
}

/// Python's `returncode`: the exit status, or the negated signal number.
fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
        .unwrap_or(-1)
}

/// Where FortiClient's VPN log ended when an operation started, so only
/// lines it writes during the operation are reported.
struct LogCheckpoint {
    inode: Option<u64>,
    offset: u64,
    started: SystemTime,
}

impl LogCheckpoint {
    fn take(log: &Path) -> Self {
        let metadata = std::fs::metadata(log).ok();
        Self {
            inode: metadata.as_ref().map(MetadataExt::ino),
            offset: metadata.map_or(0, |metadata| metadata.len()),
            started: SystemTime::now(),
        }
    }

    /// Fixed descriptions for what FortiClient logged since the checkpoint.
    fn messages(&self, log: &Path) -> Vec<&'static str> {
        self.new_lines(log)
            .map(|text| vpn_log_messages(&text))
            .unwrap_or_default()
    }

    fn new_lines(&self, log: &Path) -> io::Result<String> {
        // Non-blocking, so a FIFO at the log path cannot stall the operation.
        let nonblocking = i32::try_from(OFlags::NONBLOCK.bits()).unwrap_or_default();
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(nonblocking)
            .open(log)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.modified()? < self.started {
            return Ok(String::new());
        }
        // A replaced or truncated log is read from its start.
        let replaced = Some(metadata.ino()) != self.inode || metadata.len() < self.offset;
        let offset = if replaced { 0 } else { self.offset };
        file.seek(SeekFrom::Start(
            offset.max(metadata.len().saturating_sub(LOG_TAIL)),
        ))?;
        let mut bytes = Vec::new();
        file.take(LOG_TAIL).read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use crate::parse::LOGIN_BLOCKED_MESSAGE;

    const DPKG: &str = "dpkg-query -W -f=${db:Status-Abbrev}${Version} forticlient";
    const RPM: &str = "rpm -q --qf %{VERSION} forticlient";
    const PACMAN_AUR: &str = "pacman -Q forticlient-vpn";
    const PACMAN: &str = "pacman -Q forticlient";
    const TOOLS: [&str; 3] = ["dpkg-query", "rpm", "pacman"];
    const STOPPED_VERSION_OUTPUT: &str = "Unable to display version info: unable to retrieve version info: client port not found: open /var/run/forticlient/service_port: no such file or directory\n";

    fn captured(raw_status: i32, stdout: &str) -> io::Result<Option<Captured>> {
        Ok(Some(Captured {
            status: ExitStatus::from_raw(raw_status),
            stdout: stdout.as_bytes().to_vec(),
        }))
    }

    /// Ask a package database whose `installed` tools answer only the
    /// command lines in `answers`. Returns the version found, the tools
    /// looked up, and the command lines run, in order.
    fn ask_database(
        installed: &[&str],
        answers: &[(&str, &str)],
    ) -> (Option<String>, Vec<String>, Vec<String>) {
        let mut lookups = Vec::new();
        let mut queries = Vec::new();
        let version = package_version(
            |tool| {
                lookups.push(tool.to_owned());
                installed.contains(&tool).then(|| tool.to_owned())
            },
            |tool: &String, args| {
                let line = format!("{tool} {}", args.join(" "));
                let output = answers
                    .iter()
                    .find(|(known, _)| *known == line)
                    .map(|(_, output)| (*output).to_owned());
                queries.push(line);
                output
            },
        );
        (version, lookups, queries)
    }

    fn no_database() -> Option<String> {
        None
    }

    fn unused_database() -> Option<String> {
        panic!("the package database was consulted")
    }

    fn database_version() -> Option<String> {
        Some("7.4.3.5411".to_owned())
    }

    fn statuses(states: &[Status]) -> impl FnMut() -> Result<Status, VpnError> {
        let mut queue: VecDeque<Status> = states.iter().cloned().collect();
        move || {
            queue
                .pop_front()
                .ok_or_else(|| VpnError::failed("no more statuses"))
        }
    }

    #[test]
    fn defaults_match_the_official_cli() {
        let config = CliConfig::default();
        assert_eq!(config.executable, "forticlient");
        assert_eq!(config.read_timeout, Duration::from_secs(10));
        assert_eq!(config.interactive_timeout, Duration::from_secs(120));
        assert_eq!(config.vpn_log, Path::new("/var/log/forticlient/sslvpn.log"));
        assert_eq!(
            Cli::new(config).interactive_timeout(),
            Duration::from_secs(120)
        );
    }

    #[test]
    fn the_executable_can_be_overridden() {
        let config = CliConfig::with_executable(Some("/opt/custom/forticlient".into()));
        assert_eq!(config.executable, "/opt/custom/forticlient");
        assert_eq!(
            CliConfig::with_executable(Some(String::new())).executable,
            DEFAULT_EXECUTABLE
        );
        assert_eq!(
            CliConfig::with_executable(None).executable,
            DEFAULT_EXECUTABLE
        );
    }

    #[test]
    fn only_the_default_name_falls_back_to_the_official_cli() {
        // What `resolve_executable` offers `find` for `executable`.
        let offered = |executable: &str| {
            let mut offered = Vec::new();
            let resolved = resolve_with(executable, |name, fallbacks| {
                assert_eq!(name, executable);
                offered.extend(fallbacks.iter().map(|fallback| (*fallback).to_owned()));
                None
            });
            assert_eq!(resolved, Err(VpnError::not_found(CLI_NOT_FOUND)));
            offered
        };
        assert_eq!(offered("forticlient"), ["/opt/forticlient/forticlient-cli"]);
        for executable in [
            "/usr/bin/forticlient",
            "/opt/forticlient/forticlient-cli",
            "forticlient-cli",
            "Forticlient",
            "",
        ] {
            assert!(offered(executable).is_empty(), "{executable:?}");
        }
        let found = PathBuf::from("/usr/bin/forticlient");
        assert_eq!(
            resolve_with("forticlient", |_, _| Some(found.clone())),
            Ok(found)
        );
    }

    #[test]
    fn the_official_cli_is_inside_the_install_directory() {
        assert!(Path::new(OFFICIAL_CLI_PATH).starts_with(OFFICIAL_INSTALL_DIRECTORY));
        assert_eq!(
            PackageDatabase::default(),
            PackageDatabase {
                install_directory: PathBuf::from("/opt/forticlient"),
                search_path: None,
            }
        );
    }

    #[test]
    fn failed_reads_report_a_stopped_service() {
        let stopped = VpnError::failed(SERVICE_STOPPED);
        assert_eq!(
            read_failure("status", "Failed to get response from confighandler\n"),
            stopped
        );
        assert_eq!(read_failure("list", STOPPED_VERSION_OUTPUT), stopped);
        assert_eq!(
            read_failure("view", "SECRET\n").message(),
            "FortiClient could not complete view. Check its service and permissions."
        );
    }

    #[test]
    fn pending_status_confirmation() {
        let sequence = [
            Status::new(ConnectionState::Connecting),
            Status::with_profile(ConnectionState::Connected, "Office"),
        ];
        let deadline = Instant::now() + Duration::from_secs(1);
        let status = poll_status(
            ConnectionState::Connected,
            &CancelToken::new(),
            deadline,
            statuses(&sequence),
        );
        assert_eq!(
            status.unwrap(),
            Status::with_profile(ConnectionState::Connected, "Office")
        );
    }

    #[test]
    fn status_wait_stops_on_settled_states_deadlines_and_cancellation() {
        let deadline = Instant::now() + Duration::from_secs(1);
        let settled = [Status::new(ConnectionState::Disconnected)];
        let error = poll_status(
            ConnectionState::Connected,
            &CancelToken::new(),
            deadline,
            statuses(&settled),
        );
        assert_eq!(
            error.unwrap_err().message(),
            "FortiClient did not confirm connected. Refresh status and try again."
        );

        let pending = vec![Status::new(ConnectionState::Disconnecting); 3];
        let past = Instant::now();
        let error = poll_status(
            ConnectionState::Disconnected,
            &CancelToken::new(),
            past,
            statuses(&pending),
        );
        assert_eq!(
            error.unwrap_err().message(),
            "FortiClient did not confirm disconnected. Refresh status and try again."
        );

        let cancel = CancelToken::new();
        cancel.cancel();
        let error = poll_status(
            ConnectionState::Connected,
            &cancel,
            deadline,
            statuses(&pending),
        )
        .unwrap_err();
        assert!(error.is_cancelled());
    }

    #[test]
    fn connect_arguments_never_include_secrets() {
        let disabled = Profile::with_fields("Office", [("Save Password", "Disabled")]);
        let enabled = Profile::with_fields("Office", [("Save Password", "Enabled")]);
        assert_eq!(
            connect_arguments(&disabled, "", false),
            ["connect", "Office", "-p"]
        );
        assert_eq!(
            connect_arguments(&enabled, "worker", false),
            ["connect", "Office", "-u", "worker"]
        );
        // Remembering a password always asks for a fresh one to save.
        assert_eq!(
            connect_arguments(&enabled, "", true),
            ["connect", "Office", "-p", "-s"]
        );
        assert_eq!(
            connect_arguments(&disabled, "worker", true),
            ["connect", "Office", "-u", "worker", "-p", "-s"]
        );
        assert_eq!(
            connect_arguments(&Profile::new("New"), "", false),
            ["connect", "New", "-p"]
        );
    }

    #[test]
    fn edit_answers_come_from_the_settings() {
        let settings = ProfileSettings {
            name: "Lab".into(),
            gateway: "vpn.example.org:10443".into(),
            authentication: AuthenticationMode::Save,
            username: "worker".into(),
            certificate_kind: CertificateKind::Local,
            certificate: "/home/worker/cert.p12".into(),
        };
        assert_eq!(
            edit_answer(&settings, true, PromptKind::Gateway),
            Some("vpn.example.org:10443")
        );
        assert_eq!(
            edit_answer(&settings, true, PromptKind::Authentication),
            Some("2")
        );
        assert_eq!(
            edit_answer(&settings, true, PromptKind::Username),
            Some("worker")
        );
        assert_eq!(
            edit_answer(&settings, true, PromptKind::CertificateType),
            Some("1")
        );
        assert_eq!(
            edit_answer(&settings, true, PromptKind::Certificate),
            Some("/home/worker/cert.p12")
        );
        // A new certificate's password always comes from the user.
        for editing in [true, false] {
            assert_eq!(
                edit_answer(&settings, editing, PromptKind::CertificatePassword),
                None
            );
            assert_eq!(
                edit_answer(&settings, editing, PromptKind::VerifyPassword),
                None
            );
        }
        let keep = ProfileSettings {
            certificate_kind: CertificateKind::Keep,
            ..settings
        };
        assert_eq!(
            edit_answer(&keep, true, PromptKind::CertificatePassword),
            Some("")
        );
        assert_eq!(
            edit_answer(&keep, true, PromptKind::VerifyPassword),
            Some("")
        );
        assert_eq!(
            edit_answer(&keep, false, PromptKind::CertificatePassword),
            None
        );
        assert_eq!(
            edit_answer(&keep, true, PromptKind::CertificateType),
            Some("")
        );
        assert_eq!(edit_answer(&keep, true, PromptKind::Password), None);
        assert_eq!(edit_answer(&keep, true, PromptKind::Mfa), None);
    }

    #[test]
    fn saved_profiles_are_verified() {
        let existing = Profile::with_fields(
            "Lab",
            [
                ("Remote Gateway", "old"),
                ("Username", "worker"),
                ("Auto Connect", "Enabled"),
                ("Always Up", "Disabled"),
            ],
        );
        let settings = ProfileSettings {
            name: "Lab".into(),
            gateway: "new".into(),
            ..Default::default()
        };
        let mut saved = existing.clone();
        saved.fields[0].1 = "new".into();
        assert_eq!(verify_saved(&settings, Some(&existing), &saved), Ok(()));
        assert!(
            verify_saved(&settings, Some(&existing), &existing)
                .unwrap_err()
                .message()
                .contains("gateway")
        );

        let mut changed = saved.clone();
        changed.fields[2].1 = "Disabled".into();
        let error = verify_saved(&settings, Some(&existing), &changed).unwrap_err();
        assert!(error.message().contains("advanced setting"));
        assert_eq!(verify_saved(&settings, None, &changed), Ok(()));

        let save = ProfileSettings {
            authentication: AuthenticationMode::Save,
            username: "other".into(),
            ..settings.clone()
        };
        assert!(
            verify_saved(&save, Some(&existing), &saved)
                .unwrap_err()
                .message()
                .contains("username")
        );

        let certificate = ProfileSettings {
            certificate_kind: CertificateKind::Smartcard,
            certificate: "lib.so".into(),
            ..settings
        };
        assert!(
            verify_saved(&certificate, Some(&existing), &saved)
                .unwrap_err()
                .message()
                .contains("certificate")
        );
        let mut with_certificate = saved.clone();
        with_certificate
            .fields
            .push(("Client Certificate".into(), "lib.so".into()));
        assert_eq!(
            verify_saved(&certificate, Some(&existing), &with_certificate),
            Ok(())
        );
    }

    #[test]
    fn version_output_drops_device_identifiers() {
        let output = "FortiClient Version: 7.4.3.5411\nFortiClient Serial: SECRET-SERIAL\nFortiClient UID: SECRET-UID\n";
        assert_eq!(
            client_version(captured(0, output), unused_database),
            "7.4.3.5411"
        );
        let lone_returns = "FortiClient Version: 7.4.3.5411\rFortiClient UID: SECRET-UID\r";
        assert_eq!(
            client_version(captured(0, lone_returns), unused_database),
            "7.4.3.5411"
        );
    }

    #[test]
    fn version_failure_never_includes_raw_output() {
        // Raw wait statuses: exit code 1, and killed by SIGKILL.
        assert_eq!(
            client_version(captured(1 << 8, "SECRET"), no_database),
            "Could not determine"
        );
        assert_eq!(
            client_version(captured(9, "FortiClient Version: 7.4.3"), no_database),
            "Could not determine"
        );
        assert_eq!(
            client_version(captured(0, "SECRET"), no_database),
            "Could not determine"
        );
        // A CLI that did not answer, or could not start, is "Unavailable"
        // when the package database does not know better either.
        assert_eq!(client_version(Ok(None), no_database), "Unavailable");
        assert_eq!(
            client_version(
                Err(io::Error::from(io::ErrorKind::PermissionDenied)),
                no_database
            ),
            "Unavailable"
        );
    }

    #[test]
    fn a_missing_version_comes_from_the_package_database() {
        assert_eq!(
            client_version(captured(0, STOPPED_VERSION_OUTPUT), database_version),
            "7.4.3.5411"
        );
        // A failed run's own output is never trusted.
        assert_eq!(
            client_version(
                captured(1 << 8, "FortiClient Version: 9.9.9"),
                database_version
            ),
            "7.4.3.5411"
        );
        // Neither a CLI that timed out nor one that could not start has
        // the last word.
        assert_eq!(client_version(Ok(None), database_version), "7.4.3.5411");
        assert_eq!(
            client_version(
                Err(io::Error::from(io::ErrorKind::NotFound)),
                database_version
            ),
            "7.4.3.5411"
        );
    }

    #[test]
    fn package_queries_follow_distribution_order() {
        let (version, lookups, queries) = ask_database(&TOOLS, &[]);
        assert_eq!(version, None);
        // Each tool is looked up once; pacman answers for both names.
        assert_eq!(lookups, TOOLS);
        assert_eq!(queries, [DPKG, RPM, PACMAN_AUR, PACMAN]);

        // The first database that knows a version wins.
        let everywhere = [
            (DPKG, "ii 1:7.4.3.5411-1"),
            (RPM, "7.4.0.1636"),
            (PACMAN, "forticlient 7.2.4.0850-1"),
        ];
        let (version, lookups, queries) = ask_database(&TOOLS, &everywhere);
        assert_eq!(version.as_deref(), Some("7.4.3.5411"));
        assert_eq!(
            (lookups, queries),
            (vec!["dpkg-query".to_owned()], vec![DPKG.to_owned()])
        );

        let (version, lookups, queries) = ask_database(&TOOLS, &everywhere[1..]);
        assert_eq!(version.as_deref(), Some("7.4.0.1636"));
        assert_eq!(lookups, ["dpkg-query", "rpm"]);
        assert_eq!(queries, [DPKG, RPM]);

        let (version, lookups, queries) = ask_database(&TOOLS, &everywhere[2..]);
        assert_eq!(version.as_deref(), Some("7.2.4.0850"));
        assert_eq!(lookups, TOOLS);
        assert_eq!(queries, [DPKG, RPM, PACMAN_AUR, PACMAN]);
    }

    #[test]
    fn tools_that_are_not_installed_are_skipped() {
        let answers = [
            (DPKG, "ii 7.4.3.5411"),
            (PACMAN_AUR, "forticlient-vpn 7.4.0.1636-1\n"),
        ];
        let (version, lookups, queries) = ask_database(&["pacman"], &answers);
        assert_eq!(version.as_deref(), Some("7.4.0.1636"));
        assert_eq!(lookups, TOOLS);
        assert_eq!(queries, [PACMAN_AUR]);
        let (version, _, queries) = ask_database(&[], &answers);
        assert_eq!(version, None);
        assert!(queries.is_empty());
    }

    #[test]
    fn package_answers_without_a_version_are_skipped() {
        let answers = [
            // Removed, with its configuration kept.
            (DPKG, "rc 7.4.3.5411"),
            (RPM, "SECRET 1.2.3.4.5"),
            (PACMAN_AUR, "forticlient-vpn 7.4.0.1636-1\n"),
        ];
        let (version, _, queries) = ask_database(&TOOLS, &answers);
        assert_eq!(version.as_deref(), Some("7.4.0.1636"));
        assert_eq!(queries, [DPKG, RPM, PACMAN_AUR]);
        let (version, _, _) = ask_database(&TOOLS, &answers[..2]);
        assert_eq!(version, None);
        // dpkg's answer counts only for an installed package.
        let (version, _, _) = ask_database(&TOOLS, &[(DPKG, "7.4.3.5411")]);
        assert_eq!(version, None);
    }

    #[test]
    fn package_queries_need_a_tool_that_succeeds_in_time() {
        let shell = Path::new("/bin/sh");
        let query = |args: &[&str], timeout| {
            run_query(HostRunner::Native, shell, args, deadline_after(timeout))
        };
        let soon = Duration::from_secs(5);
        assert_eq!(
            query(&["-c", "printf 'forticlient 7.4.3.5411-1'"], soon).as_deref(),
            Some("forticlient 7.4.3.5411-1")
        );
        assert_eq!(query(&["-c", "echo 7.4.3.5411; exit 1"], soon), None);
        let absent = Path::new("/nonexistent/dpkg-query");
        assert_eq!(
            run_query(HostRunner::Native, absent, &[], deadline_after(soon)),
            None
        );
        let started = Instant::now();
        assert_eq!(
            query(&["-c", "exec sleep 5"], Duration::from_millis(100)),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn nothing_is_queried_after_the_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("ran");
        let marker = marker.to_str().unwrap();
        let touch = ["-c", r#"touch "$0""#, marker];
        let shell = Path::new("/bin/sh");
        assert_eq!(
            run_query(HostRunner::Native, shell, &touch, Instant::now()),
            None
        );
        assert!(!Path::new(marker).exists());
        let soon = deadline_after(Duration::from_secs(5));
        assert_eq!(
            run_query(HostRunner::Native, shell, &touch, soon).as_deref(),
            Some("")
        );
        assert!(Path::new(marker).exists());
    }

    #[test]
    fn only_a_cli_inside_the_install_directory_is_packaged() {
        use std::fs;
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let install = root.join("opt/forticlient");
        fs::create_dir_all(&install).unwrap();
        let cli = install.join("forticlient-cli");
        fs::write(&cli, "").unwrap();
        let packaged = |path: &Path, directory: &Path| {
            let deadline = deadline_after(Duration::from_secs(5));
            is_packaged(HostRunner::Native, path, directory, deadline)
        };
        assert!(packaged(&cli, &install));
        // Reached through a link on PATH, as /usr/bin/forticlient is.
        fs::create_dir(root.join("bin")).unwrap();
        let link = root.join("bin/forticlient");
        symlink(&cli, &link).unwrap();
        assert!(packaged(&link, &install));
        // An install directory behind a link, as /opt is on Fedora Atomic.
        symlink(root.join("opt"), root.join("var-opt")).unwrap();
        assert!(packaged(&link, &root.join("var-opt/forticlient")));

        // Anywhere else, the package does not describe the CLI.
        let elsewhere = root.join("forticlient");
        fs::write(&elsewhere, "").unwrap();
        assert!(!packaged(&elsewhere, &install));
        let sibling = root.join("opt/forticlient-old");
        fs::create_dir(&sibling).unwrap();
        fs::write(sibling.join("forticlient-cli"), "").unwrap();
        assert!(!packaged(&sibling.join("forticlient-cli"), &install));
        // A link inside the directory that leads out of it.
        let wrapper = install.join("wrapper");
        symlink(&elsewhere, &wrapper).unwrap();
        assert!(!packaged(&wrapper, &install));
        assert!(!packaged(&root.join("missing"), &install));
        assert!(!packaged(&cli, &root.join("missing")));
    }

    #[test]
    fn display_paths_shorten_the_home_directory() {
        let home = Some(Path::new("/home/worker"));
        assert_eq!(
            display_path(Path::new("/home/worker/bin/forticlient"), home),
            "~/bin/forticlient"
        );
        assert_eq!(
            display_path(
                Path::new("/home/worker/bin/forticlient"),
                Some(Path::new("/home/worker/"))
            ),
            "~/bin/forticlient"
        );
        assert_eq!(
            display_path(Path::new("/home/workers/forticlient"), home),
            "/home/workers/forticlient"
        );
        assert_eq!(
            display_path(Path::new("/opt/forticlient/forticlient-cli"), home),
            "/opt/forticlient/forticlient-cli"
        );
        assert_eq!(
            display_path(Path::new("/home/worker"), home),
            "/home/worker"
        );
        // A missing, relative, or root home directory is never abbreviated.
        assert_eq!(
            display_path(Path::new("/usr/bin/forticlient"), Some(Path::new("/"))),
            "/usr/bin/forticlient"
        );
        assert_eq!(
            display_path(Path::new("bin/forticlient"), Some(Path::new(""))),
            "bin/forticlient"
        );
        assert_eq!(
            display_path(Path::new("/usr/bin/forticlient"), None),
            "/usr/bin/forticlient"
        );
    }

    #[test]
    fn exit_codes_follow_python() {
        assert_eq!(exit_code(ExitStatus::from_raw(0)), 0);
        assert_eq!(exit_code(ExitStatus::from_raw(3 << 8)), 3);
        assert_eq!(exit_code(ExitStatus::from_raw(15)), -15);
    }

    #[test]
    fn transcript_removes_carriage_returns_and_escape_sequences() {
        let mut transcript = Transcript::default();
        transcript.push(b"\x1b[1mPlease input \x1b[0;3");
        transcript.push(b"2mpassword.\r\n");
        assert_eq!(transcript.text(), "Please input password.\n");
        transcript.clear();
        assert_eq!(transcript.text(), "");
        transcript.push(b"\x1b[?25lDONE.\x1b[K");
        assert_eq!(transcript.into_text(), "DONE.");
    }

    #[test]
    fn transcript_joins_characters_split_across_reads() {
        let mut transcript = Transcript::default();
        let text = "Café ✓ 🔐:".as_bytes();
        for byte in text {
            transcript.push(std::slice::from_ref(byte));
        }
        assert_eq!(transcript.text(), "Café ✓ 🔐:");
        transcript.push(b"\xff ok \xe2\x82");
        assert_eq!(transcript.text(), "Café ✓ 🔐:\u{fffd} ok ");
        transcript.push(b"\xac");
        assert_eq!(transcript.text(), "Café ✓ 🔐:\u{fffd} ok €");
        // An incomplete character followed by more data is replaced.
        transcript.push(b"\xe2\x82!");
        assert!(transcript.text().ends_with("€\u{fffd}!"));
    }

    #[test]
    fn transcript_keeps_the_most_recent_characters() {
        let mut transcript = Transcript::default();
        transcript.push("é".repeat(TRANSCRIPT_LIMIT).as_bytes());
        transcript.push(b"Password:");
        let text = transcript.text();
        assert_eq!(text.chars().count(), TRANSCRIPT_LIMIT);
        assert!(text.ends_with("éPassword:"));
    }

    #[test]
    fn unsupported_questions_are_detected() {
        for text in [
            "Accept untrusted certificate? [y/n] ",
            "Choose a realm:",
            "Proceed [yes]",
            "Press enter to continue",
            "Do you want to continue?\n\n",
            "Please CONFIRM the request",
        ] {
            assert!(awaits_unsupported_answer(text), "{text:?}");
        }
        for text in [
            "",
            "  \n",
            "Connecting",
            "Status: Connected\nDONE.\n",
            "Tunnel is up",
        ] {
            assert!(!awaits_unsupported_answer(text), "{text:?}");
        }
    }

    #[test]
    fn messages_are_reported_once_and_storage_warnings_flagged() {
        let lines = std::sync::Mutex::new(Vec::new());
        let log = |line: &str| lines.lock().unwrap().push(line.to_owned());
        let mut reported = Vec::new();
        report_messages(
            "Failed to save credential to key store.",
            &mut reported,
            &log,
        );
        report_messages(
            "Failed to save credential to key store.\nToo many bad login attempts",
            &mut reported,
            &log,
        );
        assert_eq!(
            *lines.lock().unwrap(),
            [
                format!("Warning: {STORAGE_WARNING}"),
                LOGIN_BLOCKED_MESSAGE.to_owned()
            ]
        );
    }

    #[test]
    fn unresolvable_cli_is_reported_as_missing() {
        let cli = Cli::new(CliConfig {
            executable: "/does/not/exist".into(),
            runner: HostRunner::Native,
            ..CliConfig::default()
        });
        assert_eq!(
            cli.resolve_executable(),
            Err(VpnError::not_found(CLI_NOT_FOUND))
        );
        assert_eq!(
            cli.read(&["status"]),
            Err(VpnError::not_found(CLI_NOT_FOUND))
        );
        assert_eq!(cli.information(), ClientInfo::unavailable());
    }

    mod log_checkpoint {
        use super::*;
        use std::fs;

        fn write(path: &Path, text: &str, modified: SystemTime) {
            fs::write(path, text).unwrap();
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }

        fn append(path: &Path, text: &str, modified: SystemTime) {
            let mut file = File::options().append(true).open(path).unwrap();
            file.write_all(text.as_bytes()).unwrap();
            file.set_modified(modified).unwrap();
        }

        #[test]
        fn a_new_log_is_read_from_its_start() {
            let directory = tempfile::tempdir().unwrap();
            let log = directory.path().join("sslvpn.log");
            let checkpoint = LogCheckpoint::take(&log);
            assert_eq!(checkpoint.inode, None);
            write(
                &log,
                "Login failed: Too many bad login attempts.\npassword SECRET\n",
                checkpoint.started + Duration::from_secs(1),
            );
            assert_eq!(checkpoint.messages(&log), [LOGIN_BLOCKED_MESSAGE]);
        }

        #[test]
        fn lines_from_before_the_operation_are_ignored() {
            let directory = tempfile::tempdir().unwrap();
            let log = directory.path().join("sslvpn.log");
            let earlier = SystemTime::now() - Duration::from_secs(60);
            write(&log, "Too many bad login attempts\n", earlier);
            let checkpoint = LogCheckpoint::take(&log);
            // Untouched since the checkpoint.
            assert!(checkpoint.messages(&log).is_empty());
            // Appended to: only the new lines count.
            append(
                &log,
                "DBUS session bus address not found\n",
                checkpoint.started + Duration::from_secs(1),
            );
            let messages = checkpoint.messages(&log);
            assert_eq!(messages.len(), 1);
            assert!(messages[0].contains("desktop session bus"));
        }

        #[test]
        fn replaced_or_truncated_logs_are_read_again() {
            let directory = tempfile::tempdir().unwrap();
            let log = directory.path().join("sslvpn.log");
            write(
                &log,
                &"filler line\n".repeat(100),
                SystemTime::now() - Duration::from_secs(60),
            );
            let checkpoint = LogCheckpoint::take(&log);
            let later = checkpoint.started + Duration::from_secs(1);
            write(&log, "Too many bad login attempts\n", later);
            assert_eq!(checkpoint.messages(&log), [LOGIN_BLOCKED_MESSAGE]);

            let rotated = directory.path().join("rotated.log");
            write(&rotated, &"Too many bad login attempts\n".repeat(10), later);
            fs::rename(&rotated, &log).unwrap();
            assert_eq!(checkpoint.messages(&log), [LOGIN_BLOCKED_MESSAGE]);
        }

        #[test]
        fn unreadable_logs_report_nothing() {
            let directory = tempfile::tempdir().unwrap();
            let checkpoint = LogCheckpoint::take(directory.path());
            assert!(checkpoint.messages(directory.path()).is_empty());
            assert!(
                checkpoint
                    .messages(&directory.path().join("missing"))
                    .is_empty()
            );
        }

        #[test]
        fn only_the_end_of_a_large_log_is_read() {
            let directory = tempfile::tempdir().unwrap();
            let log = directory.path().join("sslvpn.log");
            let checkpoint = LogCheckpoint::take(&log);
            let text = format!("Too many bad login attempts\n{}", "x".repeat(70_000));
            write(&log, &text, checkpoint.started + Duration::from_secs(1));
            assert!(checkpoint.messages(&log).is_empty());
        }
    }
}
