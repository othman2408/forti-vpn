//! Test support for Forti-VPN. Nothing here ships in packages.
//!
//! The `fake-forticlient` binary simulates the FortiClient CLI, and the
//! integration tests in `tests/` drive `forti_vpn_core::cli::Cli` against
//! it and against small shell scripts written with [`write_script`].
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// The argument that makes a script written by [`write_script`] exit at
/// once without doing anything.
pub const PROBE: &str = "--probe";

const PROBE_ATTEMPTS: u32 = 500;
const PROBE_INTERVAL: Duration = Duration::from_millis(2);

/// Quote `value` for a POSIX shell.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Write an executable `/bin/sh` script that runs `body`, ready to execute
/// when this returns.
pub fn write_script(path: &Path, body: &str) -> io::Result<()> {
    write_executable(
        path,
        &format!("#!/bin/sh\n[ \"$1\" = {PROBE} ] && exit 0\n{body}"),
    )
}

/// Write an executable file, ready to execute when this returns.
/// `contents` must exit at once, doing nothing, when its first argument is
/// [`PROBE`] (or fail to start at all).
///
/// A process that a test running in parallel forks while the file is still
/// open for writing keeps it busy (`ETXTBSY`) until that process calls exec,
/// so the file is probed until the kernel agrees to run it. Once our own
/// descriptor is closed no new process can inherit it, so one successful
/// probe is enough.
pub fn write_executable(path: &Path, contents: &str) -> io::Result<()> {
    fs::write(path, contents)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    for _ in 0..PROBE_ATTEMPTS {
        let probe = Command::new(path)
            .arg(PROBE)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match probe {
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                thread::sleep(PROBE_INTERVAL)
            }
            _ => return Ok(()),
        }
    }
    Err(io::Error::from(io::ErrorKind::ExecutableFileBusy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_single_quotes() {
        assert_eq!(shell_quote("/tmp/plain"), "'/tmp/plain'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
