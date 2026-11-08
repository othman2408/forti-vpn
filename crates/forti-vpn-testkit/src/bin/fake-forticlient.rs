//! A simulated `forticlient` executable for integration tests: a port of
//! the Python suite's `tests/fake_cli.py`, plus `forticlient version`.
//!
//! `FAKE_STATE` names a JSON file holding `profiles` (name → fields), an
//! optional `status` and `active` profile, and a `mode` that selects a
//! scenario. Every run appends its arguments as a JSON line to the same path
//! with the extension `.args`. Answers are read from standard input and are
//! never written anywhere. `FAKE_VPN_LOG` is the VPN log the `login_blocked`
//! scenario writes. The `service_stopped` mode answers every command the way
//! FortiClient 7.4.3 does while `forticlient.service` is stopped.
use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use serde_json::{Map, Value};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// The pause between the two halves of every prompt, which exercises
/// prompts split across terminal reads.
const PROMPT_PAUSE: Duration = Duration::from_millis(30);
const HANG: Duration = Duration::from_secs(10);
/// What `version` prints: an old release, unlikely to be the one installed,
/// so tests can tell it from a package database's answer.
const VERSION_OUTPUT: &str = "FortiClient Version: 7.0.13.0376\nFortiClient Serial: SECRET-SERIAL\nFortiClient UID: SECRET-UID";
/// Printed by `vpn` commands, which then fail, while the service is stopped.
const CONFIGHANDLER_FAILURE: &str = "Failed to get response from confighandler";
/// Printed by `version`, which still succeeds, while the service is stopped.
const SERVICE_PORT_MISSING: &str = "Unable to display version info: unable to retrieve version info: client port not found: open /var/run/forticlient/service_port: no such file or directory";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("fake-forticlient: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode> {
    let path = PathBuf::from(env::var_os("FAKE_STATE").ok_or("FAKE_STATE is not set")?);
    let Value::Object(state) = serde_json::from_str(&fs::read_to_string(&path)?)? else {
        return Err("the state must be a JSON object".into());
    };
    let args: Vec<String> = env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.with_extension("args"))?;
    writeln!(log, "{}", serde_json::to_string(&args)?)?;
    let mode = state
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut fake = Fake {
        path,
        state,
        args,
        mode,
    };
    if fake.arg(0)? == "version" {
        return fake.version();
    }
    if fake.mode == "service_stopped" {
        say(CONFIGHANDLER_FAILURE)?;
        return Ok(ExitCode::FAILURE);
    }
    match fake.arg(1)? {
        "list" => fake.list(),
        "status" => fake.status(),
        "view" => fake.view(),
        "connect" => fake.connect(),
        "disconnect" => fake.disconnect(),
        "remove" => fake.remove(),
        "edit" => fake.edit(),
        _ => Ok(ExitCode::SUCCESS),
    }
}

struct Fake {
    path: PathBuf,
    state: Map<String, Value>,
    args: Vec<String>,
    mode: String,
}

impl Fake {
    fn arg(&self, index: usize) -> Result<&str> {
        self.args
            .get(index)
            .map(String::as_str)
            .ok_or_else(|| format!("missing argument {index}").into())
    }

    fn text(&self, key: &str) -> Option<&str> {
        self.state.get(key).and_then(Value::as_str)
    }

    fn profiles(&self) -> Result<&Map<String, Value>> {
        self.state
            .get("profiles")
            .and_then(Value::as_object)
            .ok_or_else(|| "the state has no profiles".into())
    }

    fn profiles_mut(&mut self) -> Result<&mut Map<String, Value>> {
        self.state
            .get_mut("profiles")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| "the state has no profiles".into())
    }

    fn save(&self) -> Result<()> {
        fs::write(&self.path, serde_json::to_string(&self.state)?)?;
        Ok(())
    }

    fn version(&self) -> Result<ExitCode> {
        if self.mode == "service_stopped" {
            say(SERVICE_PORT_MISSING)?;
        } else {
            say(VERSION_OUTPUT)?;
        }
        Ok(ExitCode::SUCCESS)
    }

    /// FortiClient asks for the password unless the profile has one saved
    /// and `-p` does not ask for a new one.
    fn asks_for_password(&self) -> Result<bool> {
        let saved = self
            .profiles()?
            .get(self.arg(2)?)
            .and_then(|fields| fields.get("Save Password"))
            .and_then(Value::as_str)
            == Some("Enabled");
        let prompt_requested = self.args.iter().skip(3).any(|argument| argument == "-p");
        Ok(!saved || prompt_requested)
    }

    fn list(&self) -> Result<ExitCode> {
        if self.mode == "malformed" {
            say("nonsense")?;
        } else {
            say("VPNs:\n  Personal VPNs:")?;
            for name in self.profiles()?.keys() {
                say(&format!("    {name}"))?;
            }
        }
        Ok(ExitCode::SUCCESS)
    }

    fn status(&self) -> Result<ExitCode> {
        say(&format!(
            "Status: {}",
            self.text("status").unwrap_or("Not Running")
        ))?;
        if let Some(active) = self.text("active").filter(|active| !active.is_empty()) {
            say(&format!("VPN Name: {active}"))?;
        }
        Ok(ExitCode::SUCCESS)
    }

    fn view(&self) -> Result<ExitCode> {
        let name = self.arg(2)?;
        let fields = self
            .profiles()?
            .get(name)
            .and_then(Value::as_object)
            .filter(|fields| !fields.is_empty());
        let Some(fields) = fields else {
            return Ok(ExitCode::FAILURE);
        };
        say(&format!("VPN: {name}"))?;
        for (key, value) in fields {
            say(&format!("  {key}: {}", plain(value)))?;
        }
        Ok(ExitCode::SUCCESS)
    }

    fn connect(&mut self) -> Result<ExitCode> {
        if self.mode == "hang" {
            thread::sleep(HANG);
        }
        if self.mode == "unknown" {
            prompt("Accept untrusted certificate? [y/n] ")?;
            return Ok(ExitCode::FAILURE);
        }
        if self.asks_for_password()? {
            let _password = prompt("Please input password.\n")?;
        }
        match self.mode.as_str() {
            "login_blocked" => {
                let log = env::var_os("FAKE_VPN_LOG").ok_or("FAKE_VPN_LOG is not set")?;
                fs::write(
                    log,
                    "Login failed: Too many bad login attempts.\npassword SUPERSECRET token 123SECRET\n",
                )?;
                say("Authentication failed")?;
                return Ok(ExitCode::SUCCESS);
            }
            "profile_load_failure" => {
                say("Status: Disconnected")?;
                say("Notification: Load VPN profile was failed. Check log file for details.")?;
                say("DONE.")?;
                return Ok(ExitCode::SUCCESS);
            }
            "rejected" => {
                say("Incorrect password")?;
                return Ok(ExitCode::FAILURE);
            }
            _ => {}
        }
        let _code = prompt("Enter verification code: ")?;
        if self.mode == "retry" {
            say("Invalid code; try again")?;
            let _code = prompt("Enter verification code: ")?;
        }
        if !matches!(
            self.mode.as_str(),
            "false_success" | "keyring_false_success"
        ) {
            let active = self.arg(2)?.to_owned();
            self.state.insert("status".into(), "Connected".into());
            self.state.insert("active".into(), active.into());
            self.save()?;
        }
        if matches!(
            self.mode.as_str(),
            "keyring_warning" | "keyring_false_success"
        ) {
            say("Failed to save credential to key store.")?;
        }
        say("DONE.")?;
        Ok(ExitCode::SUCCESS)
    }

    fn disconnect(&mut self) -> Result<ExitCode> {
        if self.mode != "false_disconnect" {
            self.state.insert("status".into(), "Not Running".into());
            self.state.remove("active");
            self.save()?;
        }
        say("DONE.")?;
        Ok(ExitCode::SUCCESS)
    }

    fn remove(&mut self) -> Result<ExitCode> {
        if self.mode != "false_remove" {
            let name = self.arg(2)?.to_owned();
            self.profiles_mut()?.remove(&name);
            self.save()?;
        }
        say("DONE.")?;
        Ok(ExitCode::SUCCESS)
    }

    fn edit(&mut self) -> Result<ExitCode> {
        let name = self.arg(2)?.to_owned();
        let old = self
            .profiles()?
            .get(&name)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let field = |key: &str, default: &str| {
            old.get(key)
                .and_then(Value::as_str)
                .unwrap_or(default)
                .to_owned()
        };
        let gateway = prompt(&format!(
            "Remote Gateway [current={}]: ",
            field("Remote Gateway", "")
        ))?;
        let mut authentication =
            prompt("Authentication (1.prompt / 2.save / 3.disable) [current=save]: ")?;
        if authentication.is_empty() {
            authentication = "2".into();
        }
        let mut username = field("Username", "");
        if authentication == "2" {
            let answer = prompt(&format!("  Username: [current={username}]"))?;
            if !answer.is_empty() {
                username = answer;
            }
        }
        let certificate_type = prompt(
            "Certificate Type (1.local (pkcs12) / 2.smartcard (pkcs11) / 3.disable) [current=disable]: ",
        )?;
        let mut certificate = field("Client Certificate", "None");
        if matches!(certificate_type.as_str(), "1" | "2") {
            certificate = prompt("Client Certificate path (.p12 file URL) [default=None]: ")?;
        }
        if self.mode != "false_save" {
            let mut fields = old;
            fields.insert("Remote Gateway".into(), gateway.into());
            fields.insert("Username".into(), username.into());
            fields.insert("Client Certificate".into(), certificate.into());
            self.profiles_mut()?.insert(name, Value::Object(fields));
            self.save()?;
        }
        say("DONE.")?;
        Ok(ExitCode::SUCCESS)
    }
}

/// A JSON value as Python would concatenate it: strings without quotes.
fn plain(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

fn say(line: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{line}")?;
    stdout.flush()
}

/// Ask a question in two writes, then read one answer line.
fn prompt(text: &str) -> io::Result<String> {
    let middle = text
        .char_indices()
        .nth(text.chars().count() / 2)
        .map_or(text.len(), |(index, _)| index);
    let (first, second) = text.split_at(middle);
    for (index, part) in [first, second].into_iter().enumerate() {
        if index > 0 {
            thread::sleep(PROMPT_PAUSE);
        }
        let mut stdout = io::stdout().lock();
        stdout.write_all(part.as_bytes())?;
        stdout.flush()?;
    }
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    while answer.ends_with('\n') {
        answer.pop();
    }
    Ok(answer)
}
