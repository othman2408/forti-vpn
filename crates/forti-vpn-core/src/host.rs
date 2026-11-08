//! Starting FortiClient on the host, natively or from a Flatpak sandbox.
//!
//! * `Native`: run the program directly with the user's environment plus
//!   `LC_ALL=C`; when `DBUS_SESSION_BUS_ADDRESS` is unset, reuse the
//!   user's existing desktop bus socket (`$XDG_RUNTIME_DIR/bus`, then
//!   `/run/user/<uid>/bus`) only if it is a socket owned by the user. Never
//!   start a separate bus.
//! * `Flatpak`: run `flatpak-spawn --host --watch-bus --env=LC_ALL=C
//!   <program> …`. Standard streams (including a PTY slave) are forwarded
//!   by flatpak-spawn; the sandbox's bus address is never forwarded.
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::Access;
use rustix::io::Errno;

use crate::platform::Sandbox;

const BUS_VARIABLE: &str = "DBUS_SESSION_BUS_ADDRESS";
const FLATPAK_SPAWN: &str = "flatpak-spawn";
/// `confstr(_CS_PATH)`, which Python's `shutil.which` searches when `PATH`
/// is unset.
const DEFAULT_SEARCH_PATH: &str = "/bin:/usr/bin";
const HOST_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
const WAIT_INTERVAL: Duration = Duration::from_millis(10);
/// The longest single `poll`; callers loop until their own deadline.
const POLL_LIMIT: Duration = Duration::from_secs(1);
/// The deadline used for a timeout too large for the clock: no practical
/// limit.
const FAR_FUTURE: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// Prints, without a newline, the first regular executable file among `$1`
/// (looked up on `PATH` unless it contains a slash) and the remaining
/// arguments. Names are only ever positional arguments, never script text.
const HOST_LOOKUP_SCRIPT: &str = r#"name=$1
shift
found() {
    [ -f "$1" ] && [ -x "$1" ] && printf '%s' "$1" && exit 0
}
case $name in
*/*) found "$name" ;;
*)
    set -f
    IFS=:
    for directory in $PATH; do
        [ -n "$directory" ] && found "$directory/$name"
    done
    ;;
esac
for fallback do
    found "$fallback"
done
exit 1
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRunner {
    Native,
    Flatpak,
}

impl HostRunner {
    /// `Flatpak` when running inside a Flatpak sandbox.
    pub fn detect() -> Self {
        match Sandbox::detect() {
            Sandbox::Flatpak => Self::Flatpak,
            Sandbox::None => Self::Native,
        }
    }

    /// A command that runs `program` on the host. Callers add arguments and
    /// standard streams.
    pub fn command(self, program: &Path) -> Command {
        match self {
            Self::Native => native_command(program),
            Self::Flatpak => {
                let mut command = Command::new(FLATPAK_SPAWN);
                command
                    .args(["--host", "--watch-bus", "--env=LC_ALL=C"])
                    .arg(program);
                command
            }
        }
    }

    /// Find an executable on the host. A `name` containing `/` is checked
    /// as a path; otherwise `PATH` is searched; then each of `fallbacks`
    /// (absolute paths) is tried. Only regular files the user may execute
    /// count.
    pub fn find_executable(self, name: &str, fallbacks: &[&str]) -> Option<PathBuf> {
        self.find_executable_in(name, None, fallbacks, deadline_after(HOST_LOOKUP_TIMEOUT))
    }

    /// [`Self::find_executable`], searching `search_path` instead of `PATH`
    /// when one is given, and giving up at `deadline` (a native search
    /// never waits).
    pub(crate) fn find_executable_in(
        self,
        name: &str,
        search_path: Option<&OsStr>,
        fallbacks: &[&str],
        deadline: Instant,
    ) -> Option<PathBuf> {
        match self {
            Self::Native => {
                let inherited = env::var_os("PATH");
                let search_path = search_path
                    .or(inherited.as_deref())
                    .unwrap_or(OsStr::new(DEFAULT_SEARCH_PATH));
                find_native(name, search_path, fallbacks)
            }
            Self::Flatpak => {
                let mut command = host_lookup_command(name, search_path, fallbacks);
                let output = run_captured(&mut command, remaining(deadline)?)
                    .ok()
                    .flatten()?;
                let found = output.status.success() && !output.stdout.is_empty();
                found.then(|| PathBuf::from(OsString::from_vec(output.stdout)))
            }
        }
    }

    /// `path` with every symbolic link resolved, as the host sees it, or
    /// `None` when it does not exist. A Flatpak sandbox has a file system
    /// of its own, so there the host's `readlink -e` resolves it, giving up
    /// at `deadline`.
    pub(crate) fn canonical_path(self, path: &Path, deadline: Instant) -> Option<PathBuf> {
        match self {
            Self::Native => fs::canonicalize(path).ok(),
            Self::Flatpak => {
                let mut command = readlink_command(self, path);
                let output = run_captured(&mut command, remaining(deadline)?)
                    .ok()
                    .flatten()?;
                readlink_answer(output)
            }
        }
    }
}

/// `readlink -e -- <path>`, run on the host through `runner`: like
/// `fs::canonicalize`, it fails unless every component exists.
fn readlink_command(runner: HostRunner, path: &Path) -> Command {
    let mut command = runner.command(Path::new("readlink"));
    command.args(["-e", "--"]).arg(path);
    command
}

/// The path `readlink -e` printed, without the newline it ends with.
fn readlink_answer(output: Captured) -> Option<PathBuf> {
    let mut path = output.stdout;
    if path.last() == Some(&b'\n') {
        path.pop();
    }
    let path = PathBuf::from(OsString::from_vec(path));
    (output.status.success() && path.is_absolute()).then_some(path)
}

/// The `DBUS_SESSION_BUS_ADDRESS` to give FortiClient: `current` when set
/// and non-empty; otherwise `unix:path=<socket>` for the first candidate
/// socket that `probe` reports as a socket owned by `uid`. `probe` returns
/// `(owner uid, is socket)` or `None` when the path does not exist.
pub fn session_bus_address(
    current: Option<&str>,
    runtime_dir: Option<&str>,
    uid: u32,
    probe: &dyn Fn(&Path) -> Option<(u32, bool)>,
) -> Option<String> {
    if let Some(current) = current.filter(|current| !current.is_empty()) {
        return Some(current.to_owned());
    }
    let runtime_socket = runtime_dir
        .map(Path::new)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join("bus"));
    let user_socket = PathBuf::from(format!("/run/user/{uid}/bus"));
    runtime_socket
        .into_iter()
        .chain([user_socket])
        .find(|socket| probe(socket) == Some((uid, true)))
        .map(|socket| bus_address(&socket))
}

/// A `unix:path=` address, escaped as the D-Bus specification requires.
fn bus_address(socket: &Path) -> String {
    let mut address = String::from("unix:path=");
    for &byte in socket.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-_/.\\*".contains(&byte) {
            address.push(char::from(byte));
        } else {
            let _ = write!(address, "%{byte:02x}");
        }
    }
    address
}

fn native_command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command.env("LC_ALL", "C");
    let current = env::var_os(BUS_VARIABLE);
    let current = current.as_deref().map(OsStr::to_string_lossy);
    let runtime_dir = env::var("XDG_RUNTIME_DIR").ok();
    let uid = rustix::process::getuid().as_raw();
    if let Some(address) = session_bus_address(
        current.as_deref(),
        runtime_dir.as_deref(),
        uid,
        &socket_owner,
    ) && current.as_deref() != Some(address.as_str())
    {
        command.env(BUS_VARIABLE, address);
    }
    command
}

fn socket_owner(path: &Path) -> Option<(u32, bool)> {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.uid(), metadata.file_type().is_socket()))
}

fn find_native(name: &str, search_path: &OsStr, fallbacks: &[&str]) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if name.contains('/') {
        candidates.push(PathBuf::from(name));
    } else {
        let directories =
            env::split_paths(search_path).filter(|directory| !directory.as_os_str().is_empty());
        candidates.extend(directories.map(|directory| directory.join(name)));
    }
    candidates.extend(fallbacks.iter().map(PathBuf::from));
    candidates
        .into_iter()
        .find(|candidate| is_executable_file(candidate))
}

/// A regular file with an execute bit that this user may run. As with
/// Python's `shutil.which`, `access(X_OK)` has the last word, so ownership,
/// ACLs, and `noexec` mounts are respected.
fn is_executable_file(path: &Path) -> bool {
    let has_execute_bit = fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0);
    has_execute_bit && rustix::fs::access(path, Access::EXEC_OK).is_ok()
}

/// [`HOST_LOOKUP_SCRIPT`] run on the host, with `PATH` set to
/// `search_path` when one is given.
fn host_lookup_command(name: &str, search_path: Option<&OsStr>, fallbacks: &[&str]) -> Command {
    let mut command = Command::new(FLATPAK_SPAWN);
    // --watch-bus ends the host shell too if the time limit kills us.
    command.args(["--host", "--watch-bus"]);
    if let Some(search_path) = search_path {
        let mut variable = OsString::from("--env=PATH=");
        variable.push(search_path);
        command.arg(variable);
    }
    command
        .args(["/bin/sh", "-c", HOST_LOOKUP_SCRIPT, "sh", name])
        .args(fallbacks);
    command
}

/// A command that finished within its time limit.
#[derive(Debug)]
pub(crate) struct Captured {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
}

/// Run `command` without a terminal: standard input and error are null and
/// standard output is captured. `Ok(None)` means it did not finish within
/// `timeout`; it has then been killed and reaped.
pub(crate) fn run_captured(
    command: &mut Command,
    timeout: Duration,
) -> io::Result<Option<Captured>> {
    let deadline = deadline_after(timeout);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let result = collect(&mut child, deadline);
    if !matches!(result, Ok(Some(_))) {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn collect(child: &mut Child, deadline: Instant) -> io::Result<Option<Captured>> {
    let mut stdout = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        let pipe = OwnedFd::from(pipe);
        let mut buffer = [0; 8192];
        loop {
            let Some(remaining) = remaining(deadline) else {
                return Ok(None);
            };
            if !readable(&pipe, remaining)? {
                continue;
            }
            match rustix::io::read(&pipe, &mut buffer[..]) {
                Ok(0) => break,
                Ok(count) => stdout.extend_from_slice(&buffer[..count]),
                Err(Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(wait_until(child, deadline)?.map(|status| Captured { status, stdout }))
}

/// Wait for `child` to exit, giving up at `deadline`.
pub(crate) fn wait_until(child: &mut Child, deadline: Instant) -> io::Result<Option<ExitStatus>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let Some(remaining) = remaining(deadline) else {
            return Ok(None);
        };
        thread::sleep(remaining.min(WAIT_INTERVAL));
    }
}

/// Whether reading `fd` would not block (data, end of file, or an error),
/// waiting up to `timeout`.
pub(crate) fn readable(fd: impl AsFd, timeout: Duration) -> io::Result<bool> {
    let timeout = Timespec::try_from(timeout.min(POLL_LIMIT)).unwrap_or_default();
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    match poll(&mut fds, Some(&timeout)) {
        Ok(_) => Ok(!fds[0].revents().is_empty()),
        Err(Errno::INTR) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// The time left until `deadline`, or `None` once it has passed.
pub(crate) fn remaining(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
}

/// When something starting now times out. An absurd configured timeout must
/// not crash the app by overflowing the clock.
pub(crate) fn deadline_after(timeout: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(timeout)
        .or_else(|| now.checked_add(FAR_FUTURE))
        // Unreachable with a monotonic clock that counts from boot; failing
        // at once is still better than panicking.
        .unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::unix::net::UnixListener;
    use std::os::unix::process::ExitStatusExt;

    const UID: u32 = 1000;

    fn executable(path: &Path, mode: u32) {
        File::create(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// A directory for test executables. Not in /tmp, which may be mounted
    /// noexec: beside this test binary, which evidently may run.
    fn executable_directory() -> tempfile::TempDir {
        let binary = env::current_exe().unwrap();
        tempfile::tempdir_in(binary.parent().unwrap()).unwrap()
    }

    fn search(directories: &[&Path]) -> OsString {
        env::join_paths(directories).unwrap()
    }

    #[test]
    fn session_bus_fallback_uses_only_existing_user_socket() {
        let socket = |path: &Path| (path == Path::new("/run/user/1000/bus")).then_some((UID, true));
        assert_eq!(
            session_bus_address(Some(""), None, UID, &socket).as_deref(),
            Some("unix:path=/run/user/1000/bus")
        );
        assert_eq!(
            session_bus_address(None, None, UID, &socket).as_deref(),
            Some("unix:path=/run/user/1000/bus")
        );
        let regular_file = |_: &Path| Some((UID, false));
        assert_eq!(
            session_bus_address(Some(""), None, UID, &regular_file),
            None
        );
        let existing = session_bus_address(Some("unix:abstract=existing"), None, UID, &socket);
        assert_eq!(existing.as_deref(), Some("unix:abstract=existing"));
    }

    #[test]
    fn session_bus_requires_the_users_own_socket() {
        let other_owner = |_: &Path| Some((0, true));
        assert_eq!(
            session_bus_address(None, Some("/run/user/1000"), UID, &other_owner),
            None
        );
        let missing = |_: &Path| None;
        assert_eq!(
            session_bus_address(None, Some("/run/user/1000"), UID, &missing),
            None
        );
    }

    #[test]
    fn session_bus_prefers_the_runtime_directory() {
        let any_socket = |_: &Path| Some((UID, true));
        let address = session_bus_address(None, Some("/tmp/runtime dir"), UID, &any_socket);
        assert_eq!(address.as_deref(), Some("unix:path=/tmp/runtime%20dir/bus"));
        // Relative or empty runtime directories are ignored.
        for runtime_dir in ["relative", ""] {
            let address = session_bus_address(None, Some(runtime_dir), UID, &any_socket);
            assert_eq!(address.as_deref(), Some("unix:path=/run/user/1000/bus"));
        }
        let only_fallback = |path: &Path| path.starts_with("/run/user").then_some((UID, true));
        let address = session_bus_address(None, Some("/tmp/elsewhere"), UID, &only_fallback);
        assert_eq!(address.as_deref(), Some("unix:path=/run/user/1000/bus"));
    }

    #[test]
    fn socket_owner_reports_real_sockets() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("bus");
        let _listener = UnixListener::bind(&socket).unwrap();
        let uid = rustix::process::getuid().as_raw();
        assert_eq!(socket_owner(&socket), Some((uid, true)));
        let file = directory.path().join("file");
        File::create(&file).unwrap();
        assert_eq!(socket_owner(&file), Some((uid, false)));
        assert_eq!(socket_owner(&directory.path().join("missing")), None);
    }

    #[test]
    fn native_search_follows_path_and_fallbacks() {
        let directory = executable_directory();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        fs::create_dir_all(first.join("forticlient")).unwrap();
        fs::create_dir(&second).unwrap();
        executable(&second.join("forticlient"), 0o755);
        executable(&first.join("plain"), 0o644);
        let fallback = directory.path().join("forticlient-cli");
        executable(&fallback, 0o700);
        let fallback = fallback.to_str().unwrap();

        // Directories and empty entries are skipped; the first file wins.
        let path = format!(":{}::{}", first.display(), second.display());
        let found = find_native("forticlient", OsStr::new(&path), &[fallback]);
        assert_eq!(found, Some(second.join("forticlient")));
        // Without a PATH match the fallback is used, but only if executable.
        assert_eq!(
            find_native("missing", &search(&[&first]), &[fallback]).as_deref(),
            Some(Path::new(fallback))
        );
        assert_eq!(find_native("plain", &search(&[&first]), &[]), None);
        let plain = first.join("plain");
        assert_eq!(
            find_native("missing", &search(&[&first]), &[plain.to_str().unwrap()]),
            None
        );
        assert_eq!(find_native("missing", OsStr::new(""), &[]), None);
    }

    #[test]
    fn execute_bits_for_others_do_not_count() {
        let directory = executable_directory();
        let program = directory.path().join("forticlient");
        // rw---x--x: others may run it, but its owner (this user) may not.
        executable(&program, 0o611);
        let name = program.to_str().unwrap();
        // Root may run any file with an execute bit, and some shared-folder
        // filesystems (such as Docker Desktop's) answer access(2) from their
        // own rules instead of the mode bits; the kernel's answer decides.
        let owner_may_run = rustix::fs::access(&program, rustix::fs::Access::EXEC_OK).is_ok();
        let expected = owner_may_run.then(|| program.clone());
        assert_eq!(find_native(name, OsStr::new(""), &[]), expected);
        assert_eq!(find_native("missing", OsStr::new(""), &[name]), expected);
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(find_native(name, OsStr::new(""), &[]), Some(program));
    }

    #[test]
    fn names_with_a_slash_are_paths() {
        let directory = executable_directory();
        let program = directory.path().join("forticlient");
        executable(&program, 0o750);
        let name = program.to_str().unwrap();
        assert_eq!(
            find_native(name, OsStr::new(""), &[]),
            Some(program.clone())
        );
        assert_eq!(
            HostRunner::Native.find_executable(name, &[]),
            Some(program.clone())
        );
        // A path name is not looked up on PATH.
        assert_eq!(
            find_native("sub/forticlient", &search(&[directory.path()]), &[]),
            None
        );
        let missing = directory.path().join("missing");
        assert_eq!(
            HostRunner::Native.find_executable(missing.to_str().unwrap(), &[name]),
            Some(program)
        );
        assert_eq!(
            HostRunner::Native.find_executable(directory.path().to_str().unwrap(), &[]),
            None
        );
    }

    #[test]
    fn a_search_path_replaces_path() {
        let directory = executable_directory();
        let tools = directory.path().join("tools");
        fs::create_dir(&tools).unwrap();
        executable(&tools.join("dpkg-query"), 0o755);
        let find = |name: &str, search_path: Option<&OsStr>| {
            let deadline = deadline_after(Duration::from_secs(5));
            HostRunner::Native.find_executable_in(name, search_path, &[], deadline)
        };
        let replaced = Some(tools.as_os_str());
        assert_eq!(find("dpkg-query", replaced), Some(tools.join("dpkg-query")));
        // Only the replacement is searched, not PATH.
        assert!(find("sh", None).is_some());
        assert_eq!(find("sh", replaced), None);
        // An empty search path has no directories, as with `PATH=`.
        assert_eq!(find("dpkg-query", Some(OsStr::new(""))), None);
    }

    #[test]
    fn host_lookups_can_replace_path() {
        let args = |command: &Command| {
            command
                .get_args()
                .map(|arg| arg.to_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        let official = "/opt/forticlient/forticlient-cli";
        let lookup = host_lookup_command("forticlient", None, &[official]);
        assert_eq!(lookup.get_program(), "flatpak-spawn");
        assert_eq!(
            args(&lookup),
            [
                "--host",
                "--watch-bus",
                "/bin/sh",
                "-c",
                HOST_LOOKUP_SCRIPT,
                "sh",
                "forticlient",
                official
            ]
        );
        let replaced = host_lookup_command("rpm", Some(OsStr::new("/tools:/more")), &[]);
        assert_eq!(
            args(&replaced),
            [
                "--host",
                "--watch-bus",
                "--env=PATH=/tools:/more",
                "/bin/sh",
                "-c",
                HOST_LOOKUP_SCRIPT,
                "sh",
                "rpm"
            ]
        );
    }

    #[test]
    fn canonical_paths_resolve_links_natively_and_on_the_host() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("forticlient-cli");
        File::create(&target).unwrap();
        let link = directory.path().join("forticlient");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let canonical = fs::canonicalize(&target).unwrap();
        let deadline = deadline_after(Duration::from_secs(5));
        assert_eq!(
            HostRunner::Native.canonical_path(&link, deadline),
            Some(canonical.clone())
        );
        let missing = directory.path().join("missing/forticlient");
        assert_eq!(HostRunner::Native.canonical_path(&missing, deadline), None);

        // From a sandbox the host's readlink resolves the path; this
        // system's readlink must agree.
        let flatpak = readlink_command(HostRunner::Flatpak, &link);
        let args: Vec<_> = flatpak.get_args().collect();
        assert_eq!(
            args[..4],
            ["--host", "--watch-bus", "--env=LC_ALL=C", "readlink"]
        );
        assert_eq!(
            args[4..],
            [OsStr::new("-e"), OsStr::new("--"), link.as_os_str()]
        );
        let host = |path: &Path| {
            let mut command = readlink_command(HostRunner::Native, path);
            let output = run_captured(&mut command, Duration::from_secs(5))
                .unwrap()
                .unwrap();
            readlink_answer(output)
        };
        assert_eq!(host(&link), Some(canonical));
        assert_eq!(host(&missing), None);
    }

    #[test]
    fn readlink_answers_must_be_absolute_paths() {
        let answer = |raw_status: i32, stdout: &[u8]| {
            readlink_answer(Captured {
                status: ExitStatus::from_raw(raw_status),
                stdout: stdout.to_vec(),
            })
        };
        let cli = Path::new("/opt/forticlient/forticlient-cli");
        assert_eq!(
            answer(0, b"/opt/forticlient/forticlient-cli\n").as_deref(),
            Some(cli)
        );
        assert_eq!(
            answer(0, b"/opt/forticlient/forticlient-cli").as_deref(),
            Some(cli)
        );
        // Exit code 1.
        assert_eq!(answer(1 << 8, b"/opt/forticlient/forticlient-cli\n"), None);
        assert_eq!(answer(0, b""), None);
        assert_eq!(answer(0, b"\n"), None);
        assert_eq!(answer(0, b"forticlient-cli\n"), None);
    }

    #[test]
    fn native_command_runs_in_the_c_locale() {
        let command = HostRunner::Native.command(Path::new("/usr/bin/forticlient"));
        assert_eq!(command.get_program(), "/usr/bin/forticlient");
        assert_eq!(command.get_args().count(), 0);
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "LC_ALL" && value == Some(OsStr::new("C")))
        );
    }

    #[test]
    fn flatpak_command_runs_on_the_host_without_the_sandbox_bus() {
        let command = HostRunner::Flatpak.command(Path::new("/usr/bin/forticlient"));
        assert_eq!(command.get_program(), "flatpak-spawn");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "--host",
                "--watch-bus",
                "--env=LC_ALL=C",
                "/usr/bin/forticlient"
            ]
        );
        assert_eq!(command.get_envs().count(), 0);
    }

    #[test]
    fn host_lookup_script_finds_the_first_executable() {
        let directory = executable_directory();
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).unwrap();
        executable(&bin.join("forticlient"), 0o755);
        let fallback = directory.path().join("fallback");
        executable(&fallback, 0o755);
        let lookup = |name: &str, path: &OsStr, fallbacks: &[&Path]| {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", HOST_LOOKUP_SCRIPT, "sh", name])
                .args(fallbacks)
                .env("PATH", path);
            let output = run_captured(&mut command, Duration::from_secs(5))
                .unwrap()
                .unwrap();
            output
                .status
                .success()
                .then(|| PathBuf::from(OsString::from_vec(output.stdout)))
        };
        let path = format!("::{}:", bin.display());
        assert_eq!(
            lookup("forticlient", OsStr::new(&path), &[]),
            Some(bin.join("forticlient"))
        );
        assert_eq!(
            lookup("*", OsStr::new(&path), &[&fallback]),
            Some(fallback.clone())
        );
        assert_eq!(lookup("missing; exit 0", OsStr::new(&path), &[]), None);
        let name = bin.join("forticlient");
        assert_eq!(
            lookup(name.to_str().unwrap(), OsStr::new(""), &[]),
            Some(name)
        );
        assert_eq!(lookup("bin", OsStr::new(directory.path()), &[]), None);
    }

    #[test]
    fn captured_commands_report_output_and_status() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 'out\\r\\n'; printf 'err' >&2; exit 3"]);
        let output = run_captured(&mut command, Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(output.stdout, b"out\r\n");
        assert_eq!(output.status.code(), Some(3));
    }

    #[test]
    fn captured_commands_are_killed_at_the_deadline() {
        let started = Instant::now();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exec sleep 5"]);
        assert!(
            run_captured(&mut command, Duration::from_millis(100))
                .unwrap()
                .is_none()
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        let error = run_captured(
            &mut Command::new("/nonexistent/forticlient"),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn absurd_timeouts_do_not_overflow_the_deadline() {
        let before = Instant::now();
        assert!(deadline_after(Duration::MAX) >= before + FAR_FUTURE);
        let deadline = deadline_after(Duration::from_secs(1));
        assert!(deadline >= before + Duration::from_secs(1));
        assert!(deadline < before + Duration::from_secs(60));

        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf ok"]);
        let output = run_captured(&mut command, Duration::MAX).unwrap().unwrap();
        assert_eq!(output.stdout, b"ok");
    }
}
