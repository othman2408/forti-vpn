//! `forti_vpn_core::cli::Cli` against the simulated FortiClient CLI: a port
//! of `IntegrationTests` in the Python `tests/test_backend.py` and of the
//! `CLI.information` tests in `tests/test_support.py`, plus checks of the
//! pseudo-terminal itself and of the package-database fallback.
//!
//! Tests run in parallel, so none of them touches the process environment:
//! each gets its own directory with a wrapper script that points the fake
//! CLI at that test's state file and VPN log. Each CLI also looks
//! FortiClient's package up only in that directory, with fake package
//! tools, so no test depends on what the host has installed.
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use forti_vpn_core::backend::{Ask, Backend, OperationContext};
use forti_vpn_core::cancel::CancelToken;
use forti_vpn_core::cli::{Cli, CliConfig, PackageDatabase};
use forti_vpn_core::error::{CANCELLED, CLI_NOT_FOUND, VpnError};
use forti_vpn_core::host::HostRunner;
use forti_vpn_core::model::{
    AuthenticationMode, CertificateKind, ClientInfo, ConnectionState, Profile, ProfileSettings,
    PromptKind, Secret, Status,
};
use forti_vpn_core::parse::{SERVICE_STOPPED, STORAGE_WARNING, auth_prompts};
use forti_vpn_testkit::{shell_quote, write_executable, write_script};
use serde_json::{Value, json};
use tempfile::TempDir;

const FAKE: &str = env!("CARGO_BIN_EXE_fake-forticlient");
const PASSWORD: &str = "SUPERSECRET";
const CODE: &str = "123SECRET";
const INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(3);
/// The version the fake CLI reports while its service runs.
const FAKE_VERSION: &str = "7.0.13.0376";
const VERSION_UNKNOWN: &str = "Could not determine";
const VERSION_UNAVAILABLE: &str = "Unavailable";
/// Where `install_packaged` puts FortiClient, as the official packages do.
const INSTALL_DIRECTORY: &str = "opt/forticlient";
/// The queries `Cli::information` may run, as the fake tools record them.
const DPKG: &str = "dpkg-query -W -f=${db:Status-Abbrev}${Version} forticlient";
const RPM: &str = "rpm -q --qf %{VERSION} forticlient";
const PACMAN_AUR: &str = "pacman -Q forticlient-vpn";
const PACMAN: &str = "pacman -Q forticlient";
/// What dpkg answers for the installed FortiClient package: a release that
/// neither the fake CLI nor, most likely, the host reports.
const DPKG_INSTALLED: &str = "ii 7.4.1.1736";
const PACKAGED_VERSION: &str = "7.4.1.1736";

fn fields() -> Value {
    json!({
        "Remote Gateway": "localhost:443",
        "Username": "worker",
        "Client Certificate": "None",
        "Single Sign On (SSO) for VPN Tunnel": "Disabled",
        "Auto Connect": "Enabled",
        "Always Up": "Disabled",
        "Save Password": "Disabled",
    })
}

fn temporary_directory() -> TempDir {
    // Inside the target directory, because /tmp may be mounted noexec.
    tempfile::Builder::new()
        .prefix("cli-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

fn quote(path: &Path) -> String {
    shell_quote(path.to_str().unwrap())
}

fn config(executable: &Path) -> CliConfig {
    CliConfig {
        executable: executable.to_str().unwrap().to_owned(),
        read_timeout: Duration::from_secs(10),
        interactive_timeout: INTERACTIVE_TIMEOUT,
        vpn_log: executable.with_file_name("sslvpn.log"),
        runner: HostRunner::Native,
    }
}

/// A CLI for `config` that looks FortiClient's package up only in `root`:
/// installed in `root/opt/forticlient` (see `install_packaged`), with the
/// package tools in `root/tools` (see `fake_package_tools`).
fn cli_in(root: &Path, config: CliConfig) -> Cli {
    Cli::new(config).with_package_database(PackageDatabase {
        install_directory: root.join(INSTALL_DIRECTORY),
        search_path: Some(root.join("tools").into_os_string()),
    })
}

/// A script that runs the fake CLI with the state and VPN log in `root`.
fn fake_wrapper(root: &Path) -> String {
    format!(
        "export FAKE_STATE={} FAKE_VPN_LOG={}\nexec {} \"$@\"\n",
        quote(&root.join("state.json")),
        quote(&root.join("sslvpn.log")),
        shell_quote(FAKE)
    )
}

/// Install a CLI as the official packages do: `write` creates it as
/// `root/opt/forticlient/forticlient-cli`, which is reached through the
/// link `root/bin/forticlient` as `/usr/bin/forticlient` is. Returns the
/// link.
fn install_packaged(root: &Path, write: impl FnOnce(&Path) -> io::Result<()>) -> PathBuf {
    let directory = root.join(INSTALL_DIRECTORY);
    fs::create_dir_all(&directory).unwrap();
    let installed = directory.join("forticlient-cli");
    write(&installed).unwrap();
    fs::create_dir_all(root.join("bin")).unwrap();
    let link = root.join("bin/forticlient");
    symlink(&installed, &link).unwrap();
    link
}

/// Fake `dpkg-query`, `rpm`, and `pacman` in `root/tools`. Each records its
/// query in `root/queries` and runs the shell command `answers` lists for
/// that query; any other query fails, as for a package the tool does not
/// know.
fn fake_package_tools(root: &Path, answers: &[(&str, &str)]) {
    let tools = root.join("tools");
    fs::create_dir_all(&tools).unwrap();
    let log = quote(&root.join("queries"));
    for tool in ["dpkg-query", "rpm", "pacman"] {
        let mut body = format!("printf '%s %s\\n' {tool} \"$*\" >> {log}\ncase \"$*\" in\n");
        for (query, command) in answers {
            let arguments = query
                .strip_prefix(tool)
                .and_then(|rest| rest.strip_prefix(' '));
            if let Some(arguments) = arguments {
                writeln!(body, "{}) {command} ;;", shell_quote(arguments)).unwrap();
            }
        }
        body.push_str("*) exit 1 ;;\nesac\n");
        write_script(&tools.join(tool), &body).unwrap();
    }
}

/// A shell command that prints `text` as it is.
fn prints(text: &str) -> String {
    format!("printf '%s' {}", shell_quote(text))
}

/// The queries the fake package tools in `root` answered, oldest first.
fn package_queries(root: &Path) -> Vec<String> {
    fs::read_to_string(root.join("queries"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn secret(value: &str) -> Result<Option<Secret>, VpnError> {
    Ok(Some(Secret::new(value.to_owned())))
}

/// The fake CLI with its own state and VPN log, plus what the operations
/// asked and logged.
struct Harness {
    directory: TempDir,
    cli: Cli,
    cancel: CancelToken,
    prompts: Mutex<Vec<PromptKind>>,
    logs: Mutex<Vec<String>>,
}

impl Harness {
    fn new() -> Self {
        Self::with_config(|config| config)
    }

    fn with_config(adjust: impl FnOnce(CliConfig) -> CliConfig) -> Self {
        let directory = temporary_directory();
        let root = directory.path();
        let wrapper = root.join("forticlient");
        write_script(&wrapper, &fake_wrapper(root)).unwrap();
        let cli = cli_in(root, adjust(config(&wrapper)));
        Self::with_cli(directory, cli)
    }

    /// The fake CLI installed as the official packages install it (see
    /// `install_packaged`), with fake package tools that answer `answers`
    /// (see `fake_package_tools`).
    fn packaged(answers: &[(&str, &str)], adjust: impl FnOnce(CliConfig) -> CliConfig) -> Self {
        let directory = temporary_directory();
        let root = directory.path();
        let link = install_packaged(root, |installed| {
            write_script(installed, &fake_wrapper(root))
        });
        fake_package_tools(root, answers);
        let config = CliConfig {
            vpn_log: root.join("sslvpn.log"),
            ..config(&link)
        };
        let cli = cli_in(root, adjust(config));
        Self::with_cli(directory, cli)
    }

    fn with_cli(directory: TempDir, cli: Cli) -> Self {
        let harness = Self {
            cli,
            directory,
            cancel: CancelToken::new(),
            prompts: Mutex::default(),
            logs: Mutex::default(),
        };
        harness.write_state("", json!({}));
        harness
    }

    /// The executable the CLI runs, as configured.
    fn executable(&self) -> PathBuf {
        PathBuf::from(&self.cli.config().executable)
    }

    fn package_queries(&self) -> Vec<String> {
        package_queries(self.directory.path())
    }

    fn state_path(&self) -> PathBuf {
        self.directory.path().join("state.json")
    }

    fn vpn_log(&self) -> PathBuf {
        self.directory.path().join("sslvpn.log")
    }

    fn write_state(&self, mode: &str, extra: Value) {
        let mut state = json!({ "profiles": { "Office VPN": fields() }, "mode": mode });
        if let (Value::Object(state), Value::Object(extra)) = (&mut state, extra) {
            state.extend(extra);
        }
        fs::write(self.state_path(), state.to_string()).unwrap();
    }

    /// "Office VPN", with a password saved in FortiClient.
    fn write_saved_password(&self) {
        let mut saved = fields();
        saved["Save Password"] = json!("Enabled");
        self.write_state("", json!({ "profiles": { "Office VPN": saved } }));
    }

    fn answer(&self, kind: PromptKind) -> Result<Option<Secret>, VpnError> {
        self.prompts.lock().unwrap().push(kind);
        match kind {
            PromptKind::Password => secret(PASSWORD),
            PromptKind::Mfa => secret(CODE),
            PromptKind::Username => secret("worker"),
            other => panic!("unexpected prompt {other:?}"),
        }
    }

    /// Run `operation` with a context that ends at `deadline` and answers
    /// prompts with `ask`.
    fn run<T>(
        &self,
        deadline: Instant,
        ask: &Ask<'_>,
        operation: impl FnOnce(&OperationContext<'_>) -> T,
    ) -> T {
        let log = |line: &str| self.logs.lock().unwrap().push(line.to_owned());
        operation(&OperationContext {
            cancel: &self.cancel,
            deadline,
            ask,
            log: &log,
        })
    }

    fn with_context<T>(&self, operation: impl FnOnce(&OperationContext<'_>) -> T) -> T {
        self.run(
            Instant::now() + INTERACTIVE_TIMEOUT,
            &|kind| self.answer(kind),
            operation,
        )
    }

    fn office(&self) -> Profile {
        self.cli.view(&Profile::new("Office VPN")).unwrap()
    }

    fn connect(&self) -> Result<Status, VpnError> {
        self.connect_within(INTERACTIVE_TIMEOUT)
    }

    fn connect_within(&self, timeout: Duration) -> Result<Status, VpnError> {
        self.connect_with(true, timeout)
    }

    /// Connect to "Office VPN" as `worker`, asking FortiClient to save the
    /// password when `remember` is set.
    fn connect_with(&self, remember: bool, timeout: Duration) -> Result<Status, VpnError> {
        let profile = self.office();
        self.run(
            Instant::now() + timeout,
            &|kind| self.answer(kind),
            |context| self.cli.connect(&profile, "worker", remember, context),
        )
    }

    fn save(
        &self,
        settings: &ProfileSettings,
        existing: Option<&Profile>,
    ) -> Result<Profile, VpnError> {
        self.with_context(|context| self.cli.save_profile(settings, existing, context))
    }

    fn state(&self) -> ConnectionState {
        self.cli.status().unwrap().state
    }

    fn prompts(&self) -> Vec<PromptKind> {
        self.prompts.lock().unwrap().clone()
    }

    fn log_lines(&self) -> Vec<String> {
        self.logs.lock().unwrap().clone()
    }

    fn logs(&self) -> String {
        self.log_lines().join("\n")
    }

    fn recorded_arguments(&self) -> String {
        fs::read_to_string(self.state_path().with_extension("args")).unwrap_or_default()
    }

    fn connect_call(&self) -> Vec<String> {
        let calls: Vec<Vec<String>> = self
            .recorded_arguments()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        calls
            .into_iter()
            .find(|call| call.get(1).is_some_and(|command| command == "connect"))
            .unwrap()
    }

    fn assert_no_credentials(&self, text: &str) {
        assert!(!text.contains(PASSWORD), "{text}");
        assert!(!text.contains(CODE), "{text}");
    }
}

#[test]
fn connect_and_credentials_not_logged_or_argv() {
    let harness = Harness::new();
    assert_eq!(
        harness.connect().unwrap(),
        Status::with_profile(ConnectionState::Connected, "Office VPN")
    );
    assert_eq!(harness.prompts(), [PromptKind::Password, PromptKind::Mfa]);
    harness.assert_no_credentials(&(harness.logs() + &harness.recorded_arguments()));
    let command = harness.connect_call();
    assert!(command.iter().any(|argument| argument == "-s"));
    assert_eq!(command[2], "Office VPN");
    assert_eq!(
        command,
        ["vpn", "connect", "Office VPN", "-u", "worker", "-p", "-s"]
    );
    assert_eq!(
        harness.log_lines(),
        [
            "Started FortiClient connect.",
            "FortiClient requested password.",
            "FortiClient requested mfa.",
            "FortiClient CLI exited with code 0.",
            "FortiClient finished connect; verifying the result.",
        ]
    );
}

#[test]
fn mfa_retry() {
    let harness = Harness::new();
    harness.write_state("retry", json!({}));
    assert_eq!(harness.connect().unwrap().state, ConnectionState::Connected);
    assert_eq!(
        harness
            .prompts()
            .iter()
            .filter(|&&kind| kind == PromptKind::Mfa)
            .count(),
        2
    );
}

#[test]
fn saved_passwords_are_reused_without_a_prompt() {
    let harness = Harness::new();
    harness.write_saved_password();
    assert_eq!(harness.office().field("Save Password"), Some("Enabled"));
    assert_eq!(
        harness.connect_with(false, INTERACTIVE_TIMEOUT).unwrap(),
        Status::with_profile(ConnectionState::Connected, "Office VPN")
    );
    // FortiClient uses the saved password; only the MFA code is asked for.
    assert_eq!(harness.prompts(), [PromptKind::Mfa]);
    assert_eq!(
        harness.connect_call(),
        ["vpn", "connect", "Office VPN", "-u", "worker"]
    );
    harness.assert_no_credentials(&(harness.recorded_arguments() + &harness.logs()));
}

#[test]
fn updating_saved_password_forces_a_fresh_prompt() {
    let harness = Harness::new();
    harness.write_saved_password();
    assert_eq!(harness.connect().unwrap().state, ConnectionState::Connected);
    assert_eq!(harness.prompts(), [PromptKind::Password, PromptKind::Mfa]);
    assert_eq!(
        harness.connect_call(),
        ["vpn", "connect", "Office VPN", "-u", "worker", "-p", "-s"]
    );
    harness.assert_no_credentials(&(harness.recorded_arguments() + &harness.logs()));
}

#[test]
fn keyring_warning_still_verifies_actual_connection() {
    let harness = Harness::new();
    harness.write_state("keyring_warning", json!({}));
    assert_eq!(harness.connect().unwrap().state, ConnectionState::Connected);
    assert!(harness.logs().contains("password storage"));
    assert!(
        harness
            .log_lines()
            .contains(&format!("Warning: {STORAGE_WARNING}"))
    );
    harness.write_state("keyring_false_success", json!({}));
    // A warning by itself never substitutes for the status check.
    let error = harness.connect().unwrap_err();
    assert!(error.message().contains("did not confirm"), "{error:?}");
    assert_eq!(harness.state(), ConnectionState::Disconnected);
}

#[test]
fn profile_load_failure_with_zero_exit_is_not_authentication_failure() {
    let harness = Harness::new();
    harness.write_state("profile_load_failure", json!({}));
    let error = harness.connect().unwrap_err();
    assert!(
        error.message().contains("could not load this VPN profile"),
        "{error:?}"
    );
    assert_eq!(harness.state(), ConnectionState::Disconnected);
    assert!(!harness.logs().contains("rejected authentication"));
    assert!(harness.logs().contains("could not load this VPN profile"));
}

#[test]
fn server_login_block_reported_from_fresh_log_without_credentials() {
    let harness = Harness::new();
    harness.write_state("login_blocked", json!({}));
    let error = harness.connect().unwrap_err();
    assert!(
        error.message().contains("temporarily blocked login"),
        "{error:?}"
    );
    let diagnostic = harness.logs();
    assert!(diagnostic.contains("temporarily blocked login"));
    harness.assert_no_credentials(&diagnostic);
    assert!(!format!("{error:?}").contains("SECRET"));
}

#[test]
fn old_login_block_log_does_not_override_new_failure() {
    let harness = Harness::new();
    fs::write(harness.vpn_log(), "Too many bad login attempts\n").unwrap();
    harness.write_state("rejected", json!({}));
    let error = harness.connect().unwrap_err();
    assert!(
        error.message().contains("rejected authentication"),
        "{error:?}"
    );
    assert!(!harness.logs().contains("temporarily blocked"));
}

#[test]
fn rejected_password() {
    let harness = Harness::new();
    harness.write_state("rejected", json!({}));
    let error = harness.connect().unwrap_err();
    assert!(!error.is_cancelled() && !error.is_not_found(), "{error:?}");
    assert_eq!(harness.state(), ConnectionState::Disconnected);
}

#[test]
fn no_false_connection_success() {
    let harness = Harness::new();
    harness.write_state("false_success", json!({}));
    let error = harness.connect().unwrap_err();
    assert!(error.message().contains("did not confirm"), "{error:?}");
}

#[test]
fn unknown_certificate_prompt() {
    let harness = Harness::new();
    harness.write_state("unknown", json!({}));
    let error = harness.connect().unwrap_err();
    assert!(
        error.message().contains("unsupported response"),
        "{error:?}"
    );
    assert_eq!(harness.prompts(), []);
}

#[test]
fn cancel() {
    let harness = Harness::new();
    harness.write_state("hang", json!({}));
    let cancel = harness.cancel.clone();
    let timer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        cancel.cancel();
    });
    let error = harness.connect().unwrap_err();
    assert!(error.is_cancelled(), "{error:?}");
    timer.join().unwrap();
}

#[test]
fn timeout() {
    let harness = Harness::new();
    harness.write_state("hang", json!({}));
    let error = harness
        .connect_within(Duration::from_millis(200))
        .unwrap_err();
    assert!(error.message().contains("timed out"), "{error:?}");
    assert!(harness.cancel.is_cancelled());
}

#[test]
fn cancel_password_dialog() {
    let harness = Harness::new();
    let profile = harness.office();
    let dismissed = |_: PromptKind| Ok(None);
    let deadline = Instant::now() + INTERACTIVE_TIMEOUT;
    let error = harness.run(deadline, &dismissed, |context| {
        harness.cli.connect(&profile, "", false, context)
    });
    assert_eq!(error, Err(VpnError::cancelled(CANCELLED)));
}

#[test]
fn answers_must_be_a_single_line() {
    let harness = Harness::new();
    let profile = harness.office();
    let deadline = Instant::now() + INTERACTIVE_TIMEOUT;
    let multiline = |_: PromptKind| secret("first\nsecond");
    let error = harness.run(deadline, &multiline, |context| {
        harness.cli.connect(&profile, "", false, context)
    });
    assert_eq!(
        error.unwrap_err().message(),
        "Responses must contain a single line."
    );
}

#[test]
fn answers_after_the_deadline_are_not_sent() {
    let harness = Harness::new();
    let profile = harness.office();
    let deadline = Instant::now() + Duration::from_secs(1);
    let late = |_: PromptKind| {
        thread::sleep(Duration::from_millis(1300));
        secret(PASSWORD)
    };
    let error = harness.run(deadline, &late, |context| {
        harness.cli.connect(&profile, "", false, context)
    });
    assert_eq!(error.unwrap_err().message(), "The operation timed out.");
}

#[test]
fn prompt_errors_are_passed_on() {
    let harness = Harness::new();
    let profile = harness.office();
    let deadline = Instant::now() + INTERACTIVE_TIMEOUT;
    let failing = |_: PromptKind| {
        Err(VpnError::failed(
            "Authentication timed out. Retry when you are ready to sign in.",
        ))
    };
    let error = harness.run(deadline, &failing, |context| {
        harness.cli.connect(&profile, "", false, context)
    });
    assert_eq!(
        error,
        Err(VpnError::failed(
            "Authentication timed out. Retry when you are ready to sign in."
        ))
    );
}

#[test]
fn crud_and_preserve_advanced_settings() {
    let harness = Harness::new();
    let existing = harness.office();
    let settings = ProfileSettings {
        name: "Office VPN".into(),
        gateway: "new.example:443".into(),
        ..Default::default()
    };
    let saved = harness.save(&settings, Some(&existing)).unwrap();
    assert_eq!(saved.gateway(), "new.example:443");
    assert_eq!(saved.username(), "worker");
    assert_eq!(saved.field("Auto Connect"), Some("Enabled"));

    let second = ProfileSettings {
        name: "Second VPN".into(),
        gateway: "localhost:443".into(),
        authentication: AuthenticationMode::Save,
        username: "newuser".into(),
        certificate_kind: CertificateKind::Disabled,
        certificate: String::new(),
    };
    harness.save(&second, None).unwrap();
    assert_eq!(
        harness
            .cli
            .view(&Profile::new("Second VPN"))
            .unwrap()
            .username(),
        "newuser"
    );
    harness.cli.remove(&Profile::new("Second VPN")).unwrap();
    let names: Vec<String> = harness
        .cli
        .list_profiles()
        .unwrap()
        .into_iter()
        .map(|profile| profile.name)
        .collect();
    assert_eq!(names, ["Office VPN"]);
    assert!(
        harness
            .log_lines()
            .contains(&"FortiClient finished edit; verifying the result.".to_owned())
    );
}

#[test]
fn false_save_and_delete() {
    let harness = Harness::new();
    harness.write_state("false_save", json!({}));
    let settings = ProfileSettings {
        name: "Office VPN".into(),
        gateway: "changed".into(),
        ..Default::default()
    };
    let error = harness
        .save(&settings, Some(&harness.office()))
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("did not save the requested gateway"),
        "{error:?}"
    );
    harness.write_state("false_remove", json!({}));
    let error = harness.cli.remove(&Profile::new("Office VPN")).unwrap_err();
    assert_eq!(error.message(), "FortiClient did not remove the profile.");
}

#[test]
fn duplicate_creation() {
    let harness = Harness::new();
    let settings = ProfileSettings {
        name: "Office VPN".into(),
        gateway: "host".into(),
        ..Default::default()
    };
    let error = harness.save(&settings, None).unwrap_err();
    assert!(error.message().contains("already exists"), "{error:?}");
}

#[test]
fn renaming_is_refused() {
    let harness = Harness::new();
    let settings = ProfileSettings {
        name: "Renamed".into(),
        gateway: "host".into(),
        ..Default::default()
    };
    let error = harness
        .save(&settings, Some(&harness.office()))
        .unwrap_err();
    assert_eq!(error.message(), "Profile renaming is not supported.");
}

#[test]
fn connected_and_managed_profiles_protected() {
    let harness = Harness::new();
    harness.write_state("", json!({ "status": "Connected", "active": "Office VPN" }));
    let error = harness.cli.remove(&Profile::new("Office VPN")).unwrap_err();
    assert_eq!(
        error.message(),
        "Disconnect the active VPN before changing this profile."
    );
    let managed = Profile {
        managed: true,
        ..Profile::new("Company")
    };
    let error = harness.cli.remove(&managed).unwrap_err();
    assert_eq!(
        error.message(),
        "Company-managed profiles cannot be changed in this app."
    );
    let error = harness.connect().unwrap_err();
    assert!(
        error
            .message()
            .starts_with("A VPN operation is already active."),
        "{error:?}"
    );
}

#[test]
fn disconnect_verification() {
    let harness = Harness::new();
    harness.write_state("", json!({ "status": "Connected", "active": "Office VPN" }));
    assert_eq!(
        harness.cli.disconnect().unwrap().state,
        ConnectionState::Disconnected
    );
    harness.write_state(
        "false_disconnect",
        json!({ "status": "Connected", "active": "Office VPN" }),
    );
    let error = harness.cli.disconnect().unwrap_err();
    assert!(
        error.message().contains("did not confirm disconnected"),
        "{error:?}"
    );
}

#[test]
fn cli_discovery_falls_back_to_official_location_and_recovers() {
    let directory = temporary_directory();
    let official = directory.path().join("forticlient-cli");
    let official = official.to_str().unwrap();
    let absent = "forti-vpn-test-absent-cli";
    assert_eq!(
        HostRunner::Native.find_executable(absent, &[official]),
        None
    );
    // Installed without a PATH symlink: the official location is used.
    write_script(Path::new(official), "exit 0\n").unwrap();
    assert_eq!(
        HostRunner::Native
            .find_executable(absent, &[official])
            .as_deref(),
        Some(Path::new(official))
    );
    // A command on PATH wins over the fallback.
    let on_path = HostRunner::Native
        .find_executable("sh", &[official])
        .unwrap();
    assert_ne!(on_path, Path::new(official));
    // A configured CLI is found as soon as it is installed.
    let configured = directory.path().join("forticlient");
    let cli = cli_in(directory.path(), config(&configured));
    assert_eq!(
        cli.resolve_executable(),
        Err(VpnError::not_found(CLI_NOT_FOUND))
    );
    write_script(&configured, "exit 0\n").unwrap();
    assert_eq!(cli.resolve_executable(), Ok(configured));
}

#[test]
fn unavailable() {
    let missing = Cli::new(CliConfig {
        executable: "/does/not/exist".into(),
        runner: HostRunner::Native,
        ..CliConfig::default()
    });
    let error = missing.status().unwrap_err();
    assert!(
        error.is_not_found() && error.message().contains("CLI was not found"),
        "{error:?}"
    );
    let harness = Harness::new();
    let deadline = Instant::now() + INTERACTIVE_TIMEOUT;
    let answer = |_: PromptKind| secret("");
    let error = harness.run(deadline, &answer, |context| {
        missing.interactive(&["connect", "test"], &[], context.ask, context)
    });
    assert_eq!(error, Err(VpnError::not_found(CLI_NOT_FOUND)));

    // An installed file that cannot be started (its interpreter is missing).
    let directory = temporary_directory();
    let broken = directory.path().join("forticlient");
    write_executable(&broken, "#!/nonexistent/interpreter\n").unwrap();
    let cli = cli_in(directory.path(), config(&broken));
    assert_eq!(cli.status(), Err(VpnError::not_found(CLI_NOT_FOUND)));
    let error = harness.run(deadline, &answer, |context| {
        cli.interactive(&["connect", "test"], &[], context.ask, context)
    });
    assert_eq!(error, Err(VpnError::not_found(CLI_NOT_FOUND)));
}

#[test]
fn read_timeout() {
    // A CLI that never answers, so the result cannot depend on how fast the
    // fake starts.
    let directory = temporary_directory();
    let slow = directory.path().join("forticlient");
    write_script(&slow, "exec sleep 5\n").unwrap();
    let cli = Cli::new(CliConfig {
        read_timeout: Duration::from_millis(200),
        ..config(&slow)
    });
    let started = Instant::now();
    let error = cli.status().unwrap_err();
    assert!(error.message().contains("did not respond"), "{error:?}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn malformed_output() {
    let harness = Harness::new();
    harness.write_state("malformed", json!({}));
    assert!(harness.cli.list_profiles().is_err());
}

#[test]
fn failed_reads_name_the_operation() {
    let harness = Harness::new();
    let error = harness.cli.view(&Profile::new("Missing")).unwrap_err();
    assert_eq!(
        error.message(),
        "FortiClient could not complete view. Check its service and permissions."
    );
}

#[test]
fn stopped_service_is_reported_by_status_list_and_connect() {
    let harness = Harness::new();
    let profile = harness.office();
    harness.write_state("service_stopped", json!({}));
    let stopped = VpnError::failed(SERVICE_STOPPED);
    assert_eq!(harness.cli.status(), Err(stopped.clone()));
    assert_eq!(harness.cli.list_profiles(), Err(stopped.clone()));
    assert_eq!(harness.cli.view(&profile), Err(stopped.clone()));
    let connected =
        harness.with_context(|context| harness.cli.connect(&profile, "worker", false, context));
    assert_eq!(connected, Err(stopped));
    assert_eq!(harness.prompts(), []);
}

#[test]
fn stopped_service_fails_interactive_commands_too() {
    let harness = Harness::new();
    harness.write_state("service_stopped", json!({}));
    let error = harness.with_context(|context| {
        harness.cli.interactive(
            &["connect", "Office VPN", "-p"],
            auth_prompts(),
            context.ask,
            context,
        )
    });
    assert_eq!(error, Err(VpnError::failed(SERVICE_STOPPED)));
    assert!(harness.log_lines().contains(&SERVICE_STOPPED.to_owned()));
    assert_eq!(harness.prompts(), []);
}

#[test]
fn stopped_service_version_comes_from_the_package_database() {
    let harness = Harness::packaged(&[(DPKG, &prints(DPKG_INSTALLED))], |config| config);
    let running = harness.cli.information();
    assert_eq!(
        running,
        ClientInfo {
            version: FAKE_VERSION.into(),
            path: shown(&harness.executable())
        }
    );
    // A version the CLI reports needs no package database.
    assert!(harness.package_queries().is_empty());
    harness.write_state("service_stopped", json!({}));
    assert_eq!(
        harness.cli.information(),
        ClientInfo {
            version: PACKAGED_VERSION.into(),
            path: running.path
        }
    );
    assert_eq!(harness.package_queries(), [DPKG]);
}

#[test]
fn package_databases_are_asked_in_distribution_order() {
    let not_installed = "echo 'package forticlient is not installed'; exit 1";
    let aur = prints("forticlient-vpn 7.4.0.1636-1\n");
    let plain = prints("forticlient 7.2.4.0850-1\n");
    // Arch, where only pacman knows FortiClient, under its plain name.
    let harness = Harness::packaged(&[(RPM, not_installed), (PACMAN, &plain)], |config| config);
    harness.write_state("service_stopped", json!({}));
    assert_eq!(harness.cli.information().version, "7.2.4.0850");
    assert_eq!(harness.package_queries(), [DPKG, RPM, PACMAN_AUR, PACMAN]);

    // The AUR package comes first.
    let harness = Harness::packaged(&[(PACMAN_AUR, &aur), (PACMAN, &plain)], |config| config);
    harness.write_state("service_stopped", json!({}));
    assert_eq!(harness.cli.information().version, "7.4.0.1636");
    assert_eq!(harness.package_queries(), [DPKG, RPM, PACMAN_AUR]);
}

#[test]
fn only_an_installed_dpkg_package_has_a_version() {
    // Removed, with its configuration kept: dpkg still knows the version.
    let harness = Harness::packaged(&[(DPKG, &prints("rc 7.4.1.1736"))], |config| config);
    harness.write_state("service_stopped", json!({}));
    assert_eq!(harness.cli.information().version, VERSION_UNKNOWN);
    assert_eq!(harness.package_queries(), [DPKG, RPM, PACMAN_AUR, PACMAN]);
}

#[test]
fn other_clis_are_not_looked_up_in_the_package_database() {
    // A CLI elsewhere, as one FORTI_VPN_CLI names, while the package
    // database knows FortiClient.
    let harness = Harness::new();
    let root = harness.directory.path();
    fs::create_dir_all(root.join(INSTALL_DIRECTORY)).unwrap();
    fake_package_tools(root, &[(DPKG, &prints(DPKG_INSTALLED))]);
    harness.write_state("service_stopped", json!({}));
    assert_eq!(harness.cli.information().version, VERSION_UNKNOWN);
    assert!(harness.package_queries().is_empty());

    // Inside the install directory, but only a link to a CLI elsewhere.
    let wrapper = root.join(INSTALL_DIRECTORY).join("forticlient-cli");
    symlink(root.join("forticlient"), &wrapper).unwrap();
    let linked = cli_in(root, config(&wrapper));
    assert_eq!(linked.information().version, VERSION_UNKNOWN);
    assert!(harness.package_queries().is_empty());
}

/// What `information()` shows for a packaged CLI that `write` creates
/// (see `install_packaged`), with a short read timeout and fake package
/// tools that answer `answers`; and the queries they answered.
fn packaged_information(
    write: impl FnOnce(&Path) -> io::Result<()>,
    answers: &[(&str, &str)],
) -> (ClientInfo, Vec<String>) {
    let directory = temporary_directory();
    let root = directory.path();
    fake_package_tools(root, answers);
    let link = install_packaged(root, write);
    let config = CliConfig {
        read_timeout: Duration::from_millis(500),
        ..config(&link)
    };
    let started = Instant::now();
    let info = cli_in(root, config).information();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(info.path, shown(&link));
    (info, package_queries(root))
}

#[test]
fn an_unanswered_version_comes_from_the_package_database() {
    let slow = |cli: &Path| write_script(cli, "exec sleep 10\n");
    let broken = |cli: &Path| write_executable(cli, "#!/nonexistent/interpreter\n");
    let dpkg = prints(DPKG_INSTALLED);
    let installed = [(DPKG, dpkg.as_str())];
    // Neither a CLI that does not answer in time nor one that cannot start
    // reports a version, but the package database knows it.
    for (info, queries) in [
        packaged_information(slow, &installed),
        packaged_information(broken, &installed),
    ] {
        assert_eq!(info.version, PACKAGED_VERSION);
        assert_eq!(queries, [DPKG]);
    }
    // "Unavailable" only when the package database does not know it either.
    for (info, queries) in [
        packaged_information(slow, &[]),
        packaged_information(broken, &[]),
    ] {
        assert_eq!(info.version, VERSION_UNAVAILABLE);
        assert_eq!(queries, [DPKG, RPM, PACMAN_AUR, PACMAN]);
    }
}

#[test]
fn package_queries_share_one_deadline() {
    let hang = "exec sleep 10";
    let answers = [
        (DPKG, hang),
        (RPM, hang),
        (PACMAN_AUR, hang),
        (PACMAN, hang),
    ];
    let directory = temporary_directory();
    let root = directory.path();
    fake_package_tools(root, &answers);
    let silent = install_packaged(root, |cli| write_script(cli, "exit 0\n"));
    let cli = cli_in(
        root,
        CliConfig {
            read_timeout: Duration::from_millis(500),
            ..config(&silent)
        },
    );
    let started = Instant::now();
    assert_eq!(cli.information().version, VERSION_UNKNOWN);
    // One deadline for the whole fallback: the first query is stopped at
    // it, and no other query starts after it.
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_millis(1500), "{elapsed:?}");
    assert_eq!(package_queries(root), [DPKG]);
}

#[test]
fn lone_carriage_returns_end_lines_as_in_python() {
    let directory = temporary_directory();
    let script = directory.path().join("forticlient");
    let body = r"case $1:$2 in
vpn:status) printf 'Status: Connected\rVPN Name: Office VPN\r' ;;
vpn:list) printf 'VPNs:\r  Personal VPNs:\r\n    Office VPN\r' ;;
vpn:view) printf 'VPN: Office VPN\r  Username: worker\r  Save Password: Enabled\r' ;;
version:) printf 'FortiClient Version: 7.0.13.0376\rFortiClient UID: SECRET-UID\r' ;;
esac
";
    write_script(&script, body).unwrap();
    let cli = cli_in(directory.path(), config(&script));
    assert_eq!(
        cli.read(&["status"]).unwrap(),
        "Status: Connected\nVPN Name: Office VPN\n"
    );
    assert_eq!(
        cli.status().unwrap(),
        Status::with_profile(ConnectionState::Connected, "Office VPN")
    );
    assert_eq!(cli.list_profiles().unwrap(), [Profile::new("Office VPN")]);
    let profile = cli.view(&Profile::new("Office VPN")).unwrap();
    assert_eq!(profile.username(), "worker");
    assert_eq!(profile.field("Save Password"), Some("Enabled"));
    assert_eq!(cli.information().version, FAKE_VERSION);
}

#[test]
fn huge_timeouts_do_not_overflow() {
    let unlimited = |config| CliConfig {
        read_timeout: Duration::MAX,
        ..config
    };
    let harness = Harness::packaged(&[(DPKG, &prints(DPKG_INSTALLED))], unlimited);
    assert_eq!(harness.state(), ConnectionState::Disconnected);
    assert_eq!(
        harness.cli.disconnect().unwrap().state,
        ConnectionState::Disconnected
    );
    assert_eq!(harness.cli.information().version, FAKE_VERSION);
    // Only the fake package tools answer, without a time limit either.
    harness.write_state("service_stopped", json!({}));
    assert_eq!(harness.cli.information().version, PACKAGED_VERSION);
    assert_eq!(harness.package_queries(), [DPKG]);
}

/// What `information()` shows for `path`, by the CLI's own rule: only an
/// absolute home directory other than `/` is shortened to `~`.
fn shown(path: &Path) -> String {
    std::env::home_dir()
        .filter(|home| home.is_absolute() && home.parent().is_some())
        .and_then(|home| path.strip_prefix(home).ok())
        .filter(|relative| !relative.as_os_str().is_empty())
        .map_or_else(
            || path.display().to_string(),
            |relative| format!("~/{}", relative.display()),
        )
}

#[test]
fn version_output_drops_device_identifiers() {
    let directory = temporary_directory();
    let script = directory.path().join("forticlient");
    let arguments = directory.path().join("arguments");
    let body = format!(
        "printf '%s\\n' \"$@\" > {}\nprintf 'FortiClient Version: 7.4.3.5411\\nFortiClient Serial: SECRET-SERIAL\\nFortiClient UID: SECRET-UID\\n'\n",
        quote(&arguments)
    );
    write_script(&script, &body).unwrap();
    let info = cli_in(directory.path(), config(&script)).information();
    assert_eq!(
        info,
        ClientInfo {
            version: "7.4.3.5411".into(),
            path: shown(&script)
        }
    );
    assert!(!format!("{info:?}").contains("SECRET"));
    assert_eq!(fs::read_to_string(arguments).unwrap(), "version\n");
}

#[test]
fn version_failure_never_includes_raw_output() {
    let directory = temporary_directory();
    let failing = directory.path().join("failing");
    write_script(
        &failing,
        "echo 'FortiClient Version: 7.4.3 SECRET'\nexit 1\n",
    )
    .unwrap();
    let unparsable = directory.path().join("unparsable");
    write_script(&unparsable, "echo SECRET\n").unwrap();
    // The package database knows a version, but none of these CLIs is the
    // packaged one, so it is never asked.
    fake_package_tools(directory.path(), &[(DPKG, &prints(DPKG_INSTALLED))]);
    let info = |script: &Path| cli_in(directory.path(), config(script)).information();
    for script in [&failing, &unparsable] {
        let shown_info = info(script);
        assert_eq!(
            shown_info,
            ClientInfo {
                version: VERSION_UNKNOWN.into(),
                path: shown(script)
            }
        );
        assert!(!format!("{shown_info:?}").contains("SECRET"));
    }

    let slow = directory.path().join("slow");
    write_script(&slow, "exec sleep 10\n").unwrap();
    let impatient = cli_in(
        directory.path(),
        CliConfig {
            read_timeout: Duration::from_millis(200),
            ..config(&slow)
        },
    );
    let started = Instant::now();
    assert_eq!(impatient.information().version, VERSION_UNAVAILABLE);
    assert!(started.elapsed() < Duration::from_secs(5));

    let broken = directory.path().join("broken");
    write_executable(&broken, "#!/nonexistent/interpreter\n").unwrap();
    assert_eq!(
        info(&broken),
        ClientInfo {
            version: VERSION_UNAVAILABLE.into(),
            path: shown(&broken)
        }
    );

    assert_eq!(
        info(&directory.path().join("missing")),
        ClientInfo::unavailable()
    );
    assert!(package_queries(directory.path()).is_empty());
}

#[test]
fn interactive_commands_get_a_private_terminal_without_echo() {
    let directory = temporary_directory();
    let script = directory.path().join("forticlient");
    // Asks for a password only on a terminal with echo off, as the leader of
    // its own session; succeeds only if the answer arrives intact.
    let body = r#"session=$(cut -d ' ' -f 6 /proc/$$/stat)
if [ "$session" = "$$" ] && stty -a | grep -Eq '(^| )-echo( |;|$)'; then
    printf 'Password:'
    IFS= read -r answer
    [ "$answer" = 'pass phrase; with $pecial "chars"' ] && exit 0
fi
exit 1
"#;
    write_script(&script, body).unwrap();
    let harness = Harness::new();
    let cli = cli_in(directory.path(), config(&script));
    let deadline = Instant::now() + INTERACTIVE_TIMEOUT;
    let answer = |_: PromptKind| secret(r#"pass phrase; with $pecial "chars""#);
    harness
        .run(deadline, &answer, |context| {
            cli.interactive(&["probe"], auth_prompts(), context.ask, context)
        })
        .unwrap();
    assert!(
        harness
            .log_lines()
            .contains(&"FortiClient requested password.".to_owned())
    );
}

#[test]
fn exited_commands_finish_even_if_a_child_keeps_the_terminal() {
    let directory = temporary_directory();
    let script = directory.path().join("forticlient");
    write_script(&script, "(sleep 3 &)\necho DONE.\n").unwrap();
    let harness = Harness::new();
    let cli = cli_in(directory.path(), config(&script));
    let started = Instant::now();
    let deadline = started + INTERACTIVE_TIMEOUT;
    harness
        .run(deadline, &|kind| harness.answer(kind), |context| {
            cli.interactive(&["edit"], &[], context.ask, context)
        })
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn stubborn_commands_are_killed_after_the_grace_period() {
    let directory = temporary_directory();
    let script = directory.path().join("forticlient");
    let pid_file = directory.path().join("pid");
    write_script(
        &script,
        &format!(
            "trap '' TERM\necho $$ > {}\nexec sleep 30\n",
            quote(&pid_file)
        ),
    )
    .unwrap();
    let harness = Harness::new();
    let cli = cli_in(directory.path(), config(&script));
    let cancel = harness.cancel.clone();
    let timer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        cancel.cancel();
    });
    let started = Instant::now();
    let error = harness.run(
        started + Duration::from_secs(20),
        &|kind| harness.answer(kind),
        |context| cli.interactive(&["connect"], auth_prompts(), context.ask, context),
    );
    assert_eq!(error, Err(VpnError::cancelled(CANCELLED)));
    assert!(started.elapsed() < Duration::from_secs(5));
    timer.join().unwrap();
    let pid = fs::read_to_string(&pid_file).unwrap();
    assert!(
        !Path::new("/proc").join(pid.trim()).exists(),
        "FortiClient {pid} is still running"
    );
}

#[test]
fn scripts_run_their_body_but_not_when_probed() {
    let directory = temporary_directory();
    let marker = directory.path().join("ran");
    let script = directory.path().join("script");
    write_script(&script, &format!("touch {}\n", quote(&marker))).unwrap();
    assert!(!marker.exists());
    assert!(
        std::process::Command::new(&script)
            .status()
            .unwrap()
            .success()
    );
    assert!(marker.exists());
}
