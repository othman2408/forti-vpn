//! Parsers and message tables for FortiClient CLI output.
//!
//! The parsing functions, prompt tables, and message tables started as a
//! port of the Python 0.1.0 `forti_vpn/backend.py`; the stopped-service
//! message and the package-database version are additions. Patterns are
//! compiled once (`std::sync::LazyLock`) and are case-insensitive like the
//! originals.
use std::sync::LazyLock;

use regex::Regex;

use crate::error::VpnError;
use crate::model::{ConnectionState, Profile, PromptKind, Status};

// Desktop-neutral: GNOME uses its login keyring, KDE uses KWallet, and
// Fedora 45 uses oo7.
pub const STORAGE_WARNING: &str = "FortiClient could not access its password storage, so the password was not saved. Unlock your keyring or wallet, then use Manage Profile → Update Saved Password.";
pub const LOGIN_BLOCKED_MESSAGE: &str = "Too many bad login attempts. The VPN server temporarily blocked login. Wait a few minutes or ask IT to clear the block.";
pub const PROFILE_LOAD_FAILED: &str =
    "FortiClient could not load this VPN profile. Check FortiClient’s logs for the profile error.";
/// FortiClient's system service is stopped (the CLI prints "Failed to get
/// response from confighandler" or "client port not found").
pub const SERVICE_STOPPED: &str =
    "FortiClient’s service is not running. Use Set Up FortiClient to start it, then refresh.";
pub const GENERIC_FAILURE: &str =
    "FortiClient did not complete the request. Open Settings → Diagnostics for the CLI result.";

const UNEXPECTED_LIST: &str = "FortiClient returned an unexpected profile list.";

/// The status and error characters of dpkg's three-letter abbreviated
/// status (`${db:Status-Abbrev}`) for an installed package without errors.
/// The first letter, the wanted state, may be anything: a held package
/// (`hi `) is still installed.
const DPKG_INSTALLED: &str = "i ";

/// The `vpn view` field that tells `connect` whether FortiClient already has
/// a saved password. Its value is `Enabled` or `Disabled`, never a secret.
pub(crate) const SAVE_PASSWORD_FIELD: &str = "Save Password";

/// While `forticlient.service` is stopped, `vpn` commands print the first
/// message and `version` reports the second (FortiClient 7.4.3).
const SERVICE_STOPPED_PATTERN: &str =
    r"(?:Failed to get response from confighandler|client port not found)";

// The certificate prompts are identical in both dialogues.
const CERTIFICATE_PASSWORD_PROMPT: &str = r"(?:^|\n)[ \t]*Client Certificate password:\s*$";
const VERIFY_PASSWORD_PROMPT: &str = r"(?:^|\n)[ \t]*Verify Password:\s*$";

// The editor expressions are based on the installed 7.4.3 dialogue.
static EDIT_PROMPTS: LazyLock<Vec<(PromptKind, Regex)>> = LazyLock::new(|| {
    prompt_table(&[
        (
            PromptKind::Gateway,
            r"Remote Gateway(?: \[(?:current|default)=[^\r\n]*\])?:\s*$",
        ),
        (
            PromptKind::Authentication,
            r"Authentication \(1\.prompt / 2\.save / 3\.disable\) \[(?:current|default)=[^\r\n]*\]:\s*$",
        ),
        (
            PromptKind::Username,
            r"Username:\s*(?:\[(?:current|default)=[^\r\n]*\])?\s*$",
        ),
        (
            PromptKind::CertificateType,
            r"Certificate Type \(1\.local \(pkcs12\) / 2\.smartcard \(pkcs11\) / 3\.disable\) \[(?:current|default)=[^\r\n]*\]:\s*$",
        ),
        (PromptKind::CertificatePassword, CERTIFICATE_PASSWORD_PROMPT),
        (PromptKind::VerifyPassword, VERIFY_PASSWORD_PROMPT),
        (
            PromptKind::Certificate,
            r"(?:Client Certificate path \(\.p12 file URL\)|PKCS#11 library \(library name or file URL\)) \[(?:current|default)=[^\r\n]*\]:\s*$",
        ),
    ])
});

static AUTH_PROMPTS: LazyLock<Vec<(PromptKind, Regex)>> = LazyLock::new(|| {
    prompt_table(&[
        (PromptKind::CertificatePassword, CERTIFICATE_PASSWORD_PROMPT),
        (PromptKind::VerifyPassword, VERIFY_PASSWORD_PROMPT),
        (
            PromptKind::Password,
            r"(?:^|\n)[ \t]*(?:Please input password\.|(?:Enter |Input |Please enter )?(?:VPN )?password\s*:)\s*$",
        ),
        (
            PromptKind::Username,
            r"(?:^|\n)[ \t]*(?:Please input username\.|(?:Enter |Input )?(?:user ?name|username)\s*:)\s*$",
        ),
        (
            PromptKind::Mfa,
            r"(?:^|\n)[ \t]*(?:Please input (?:token|verification code|OTP)\.|(?:Enter |Input |Please enter )?(?:FortiToken|token|verification code|one[- ]time (?:password|code)|OTP|two[- ]factor (?:code|token))\s*:)\s*$",
        ),
    ])
});

// Only fixed descriptions are exposed. Never include raw CLI output: a
// service response can contain a password, token, gateway, or account
// information.
static CLI_MESSAGES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    message_table(&[
        (r"Too many bad login attempts", LOGIN_BLOCKED_MESSAGE),
        (SERVICE_STOPPED_PATTERN, SERVICE_STOPPED),
        (
            r"(?:Load VPN profile was failed|Failed to load (?:the )?VPN profile)",
            PROFILE_LOAD_FAILED,
        ),
        (
            r"(?:Failed to save credential to key store|Unable to use system.s key store|Failed to set credentials to GNOME Keyring)",
            STORAGE_WARNING,
        ),
        (
            r"(?:Internal communication failed|Failed to communicate with VPN service|Failed sending data to VPN service|Failed to connect with the websocket server)",
            "FortiClient could not communicate with its VPN service. Open FortiClient and retry.",
        ),
        (
            r"Service rejected our request \(code:\s*\d+\)",
            "The FortiClient service rejected the connection request.",
        ),
        (
            r"(?:Incorrect password|Invalid password|Authentication failed|Login failed|Access denied)",
            "The VPN server rejected authentication. Check the username, password, and MFA requirements.",
        ),
        (
            r"(?:certificate verify failed|certificate verification failed|untrusted certificate)",
            "FortiClient could not verify the VPN certificate. Ask your IT team to check it.",
        ),
        (
            r"VPN tunnel does not exist",
            "FortiClient could not find the selected VPN profile. Refresh profiles.",
        ),
        (
            r"(?:SSLVPN|IPsec VPN) is disabled while registered to EMS",
            "Your organization has disabled this VPN type in FortiClient. Contact your IT team.",
        ),
        (
            r"(?:Failed to convert ip address form or resolve FQDN|Could not resolve|Unable to resolve)",
            "FortiClient could not resolve the VPN server address. Check your network and profile.",
        ),
        (
            r"(?:Request failed\.|Connection failed)",
            "FortiClient reported a connection failure. Check the VPN server and service availability.",
        ),
    ])
});

static VPN_LOG_MESSAGES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    message_table(&[
        (r"Too many bad login attempts", LOGIN_BLOCKED_MESSAGE),
        (
            r"DBUS session bus address not found",
            "FortiClient could not find your desktop session bus. Launch it from your desktop session and retry.",
        ),
    ])
});

static LIST_HEADING: LazyLock<Regex> = LazyLock::new(|| compile(r"(?m)^VPNs:"));
static LIST_SECTION: LazyLock<Regex> = LazyLock::new(|| compile(r"^  \S.*:\s*$"));
static LIST_PROFILE: LazyLock<Regex> = LazyLock::new(|| compile(r"^    \S"));
static SECRET_FIELD: LazyLock<Regex> = LazyLock::new(|| compile(r"(?i)password|secret|token"));
static STATUS_LINE: LazyLock<Regex> = LazyLock::new(|| compile(r"(?m)^\s*Status:\s*(.*?)\s*$"));
static ACTIVE_PROFILE: LazyLock<Regex> = LazyLock::new(|| {
    compile(r"(?mi)^\s*(?:VPN(?: name)?|VPN Name|Name|Profile|VPN tunnel):\s*(.+?)\s*$")
});
static KEYRING_WARNING_LINE: LazyLock<Regex> = LazyLock::new(|| {
    compile(
        r"(?i)^\s*(?:Failed to save credential to key store\.|Failed to set credentials to GNOME Keyring\.)\s*$",
    )
});
static FAILURE_WORD: LazyLock<Regex> = LazyLock::new(|| {
    compile(r"(?i)\b(?:error|failed|failure|invalid|denied|unable|incorrect|unrecognized)\b")
});
static SERVICE_STOPPED_OUTPUT: LazyLock<Regex> =
    LazyLock::new(|| compile(&format!("(?i){SERVICE_STOPPED_PATTERN}")));
// Matched after `normalize_newlines`, so no line ends in `\r`.
static VERSION_LINE: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?m)^FortiClient Version:[ \t]*([0-9]+(?:\.[0-9]+){1,3})[ \t]*$"));
// A version that is part of a longer run of digits and dots is not one.
static PACKAGE_VERSION: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?:^|[^0-9.])([0-9]+(?:\.[0-9]+){1,3})(?:[^0-9.]|$)"));

fn compile(pattern: &str) -> Regex {
    // Every pattern is a constant exercised by the tests below, so failing
    // here is a programming error rather than a runtime condition.
    Regex::new(pattern).expect("built-in patterns are valid")
}

fn prompt_table(entries: &[(PromptKind, &str)]) -> Vec<(PromptKind, Regex)> {
    entries
        .iter()
        .map(|&(kind, pattern)| (kind, compile(&format!("(?i){pattern}"))))
        .collect()
}

fn message_table(entries: &[(&str, &'static str)]) -> Vec<(Regex, &'static str)> {
    entries
        .iter()
        .map(|&(pattern, description)| (compile(&format!("(?i){pattern}")), description))
        .collect()
}

fn matching(table: &[(Regex, &'static str)], output: &str) -> Vec<&'static str> {
    table
        .iter()
        .filter(|(pattern, _)| pattern.is_match(output))
        .map(|&(_, description)| description)
        .collect()
}

/// `text` as Python's `text=True` pipes deliver it: `\r\n` and a lone `\r`
/// both end a line with `\n`.
pub(crate) fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The lines of `text` as Python's `str.splitlines()` produces them.
fn split_lines(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let Some(index) = rest.find(is_line_break) else {
            return Some(std::mem::take(&mut rest));
        };
        let line = &rest[..index];
        let after = &rest[index..];
        let width = if after.starts_with("\r\n") {
            2
        } else {
            after.chars().next().map_or(1, char::len_utf8)
        };
        rest = &after[width..];
        Some(line)
    })
}

fn is_line_break(character: char) -> bool {
    matches!(
        character,
        '\n' | '\r'
            | '\u{0b}'
            | '\u{0c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// Parse `forticlient vpn list`. Profiles under a section whose heading
/// does not contain "personal" are company-managed.
pub fn parse_profiles(output: &str) -> Result<Vec<Profile>, VpnError> {
    if !LIST_HEADING.is_match(output) {
        return Err(VpnError::failed(UNEXPECTED_LIST));
    }
    let mut profiles = Vec::new();
    // Whether the current section is company-managed; `None` before the
    // first section heading.
    let mut managed = None;
    for line in split_lines(output).skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        if LIST_SECTION.is_match(line) {
            managed = Some(!line.to_lowercase().contains("personal"));
        } else if let Some(managed) = managed.filter(|_| LIST_PROFILE.is_match(line)) {
            profiles.push(Profile {
                name: line.trim().to_owned(),
                fields: Vec::new(),
                managed,
            });
        } else {
            return Err(VpnError::failed(UNEXPECTED_LIST));
        }
    }
    Ok(profiles)
}

/// Parse `forticlient vpn view <name>`, dropping password/secret/token
/// fields but keeping the `Save Password` setting.
pub fn parse_profile(name: &str, output: &str, managed: bool) -> Result<Profile, VpnError> {
    if !output.starts_with(&format!("VPN: {name}\n")) {
        return Err(VpnError::failed("FortiClient could not read this profile."));
    }
    let mut profile = Profile {
        name: name.to_owned(),
        fields: Vec::new(),
        managed,
    };
    for line in split_lines(output).skip(1) {
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let value = value.trim();
        if !value.is_empty() && !is_secret_field(key, value) {
            set_field(&mut profile.fields, key, value);
        }
    }
    Ok(profile)
}

/// Whether a `vpn view` field may hold a secret. `Save Password` only
/// names a setting, so it is kept while its value is a plain toggle.
fn is_secret_field(key: &str, value: &str) -> bool {
    let save_password_toggle =
        key == SAVE_PASSWORD_FIELD && matches!(value, "Enabled" | "Disabled");
    SECRET_FIELD.is_match(key) && !save_password_toggle
}

/// Like a Python dict: a repeated key keeps its first position and takes
/// the last value.
fn set_field(fields: &mut Vec<(String, String)>, key: &str, value: &str) {
    match fields.iter_mut().find(|(existing, _)| existing == key) {
        Some((_, current)) => value.clone_into(current),
        None => fields.push((key.to_owned(), value.to_owned())),
    }
}

/// Parse `forticlient vpn status`.
pub fn parse_status(output: &str) -> Result<Status, VpnError> {
    let value = STATUS_LINE
        .captures(output)
        .and_then(|captures| captures.get(1))
        .ok_or_else(|| {
            VpnError::failed(
                "FortiClient returned an unexpected status. Open diagnostics for details.",
            )
        })?
        .as_str()
        .to_lowercase();
    let state = match value.strip_suffix("...").unwrap_or(&value) {
        "not running" | "disconnected" => ConnectionState::Disconnected,
        "connected" => ConnectionState::Connected,
        "connecting" | "re-connecting" => ConnectionState::Connecting,
        "disconnecting" => ConnectionState::Disconnecting,
        "xauth" => ConnectionState::AwaitingAuthentication,
        _ => {
            return Err(VpnError::failed(
                "Unsupported FortiClient status. Refresh status or check the original CLI.",
            ));
        }
    };
    let profile = ACTIVE_PROFILE
        .captures(output)
        .and_then(|captures| captures.get(1))
        .map(|name| name.as_str().to_owned());
    Ok(Status { state, profile })
}

/// Prompts of the 7.4.3 `vpn edit` dialogue, in matching order.
pub fn edit_prompts() -> &'static [(PromptKind, Regex)] {
    &EDIT_PROMPTS
}

/// Prompts of `vpn connect`, in matching order.
pub fn auth_prompts() -> &'static [(PromptKind, Regex)] {
    &AUTH_PROMPTS
}

/// Fixed descriptions for recognised CLI output, in table order.
pub fn cli_messages(output: &str) -> Vec<&'static str> {
    matching(&CLI_MESSAGES, output)
}

/// Whether CLI output says FortiClient's system service is not running.
pub(crate) fn service_stopped(output: &str) -> bool {
    SERVICE_STOPPED_OUTPUT.is_match(output)
}

/// Whether CLI output and exit code mean failure, and the description to
/// show. Credential-storage warnings alone are not a failure.
pub fn cli_failure(output: &str, code: i32) -> Option<&'static str> {
    // Credential storage warnings do not establish a failed VPN connection.
    // A successful CLI exit still requires status verification by connect().
    let fatal_output = split_lines(output)
        .filter(|line| !KEYRING_WARNING_LINE.is_match(line))
        .collect::<Vec<_>>()
        .join("\n");
    if code == 0 && !FAILURE_WORD.is_match(&fatal_output) {
        return None;
    }
    if let Some(&description) = cli_messages(&fatal_output).first() {
        return Some(description);
    }
    if code != 0
        && let Some(&description) = cli_messages(output).first()
    {
        return Some(description);
    }
    Some(GENERIC_FAILURE)
}

/// Fixed descriptions for recognised lines in FortiClient's VPN log.
pub fn vpn_log_messages(output: &str) -> Vec<&'static str> {
    matching(&VPN_LOG_MESSAGES, output)
}

/// The dotted version from `forticlient version`
/// (`FortiClient Version: 7.4.3.5411`), ignoring serials and UIDs. Lines
/// may end in `\n`, `\r\n`, or a lone `\r`.
pub fn parse_version(output: &str) -> Option<String> {
    VERSION_LINE
        .captures(&normalize_newlines(output))
        .and_then(|captures| captures.get(1))
        .map(|version| version.as_str().to_owned())
}

/// The dotted version in a package database's answer, such as `7.4.3.5411`
/// from dpkg's `1:7.4.3.5411-1` or pacman's `forticlient-vpn 7.4.3.5411-1`.
/// Nothing else of the output is kept.
pub(crate) fn parse_package_version(output: &str) -> Option<String> {
    PACKAGE_VERSION
        .captures(output)
        .and_then(|captures| captures.get(1))
        .map(|version| version.as_str().to_owned())
}

/// The version in dpkg's `${db:Status-Abbrev}${Version}` answer, such as
/// `ii 7.4.3.5411`, only when the status says installed (`ii `). A package
/// that was removed with its configuration kept (`rc `) or is only half
/// installed still reports the version it had.
pub(crate) fn parse_dpkg_version(output: &str) -> Option<String> {
    let mut status = output.chars();
    let wanted = status.next().filter(char::is_ascii_lowercase)?;
    output[wanted.len_utf8()..]
        .strip_prefix(DPKG_INSTALLED)
        .and_then(parse_package_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_match(prompts: &[(PromptKind, Regex)], text: &str) -> Option<PromptKind> {
        prompts
            .iter()
            .find(|(_, pattern)| pattern.is_match(text))
            .map(|&(kind, _)| kind)
    }

    /// The prompt matches as `kind`, and no partial read of it is mistaken
    /// for a different question.
    fn assert_prompt(prompts: &[(PromptKind, Regex)], text: &str, kind: PromptKind) {
        assert_eq!(first_match(prompts, text), Some(kind), "{text:?}");
        for (index, _) in text.char_indices().skip(1) {
            let partial = &text[..index];
            let found = first_match(prompts, partial);
            assert!(
                found.is_none() || found == Some(kind),
                "{partial:?} matched {found:?}"
            );
        }
    }

    /// The first half, as the fake CLI writes it before pausing.
    fn first_half(text: &str) -> &str {
        let middle = text.chars().count() / 2;
        &text[..text
            .char_indices()
            .nth(middle)
            .map_or(text.len(), |(index, _)| index)]
    }

    #[test]
    fn list_spaces_and_managed() {
        let profiles =
            parse_profiles("VPNs:\n  Personal VPNs:\n    Office VPN\n  EMS VPNs:\n    Company\n")
                .unwrap();
        assert_eq!(profiles[0].name, "Office VPN");
        assert!(!profiles[0].managed);
        assert_eq!(profiles[1].name, "Company");
        assert!(profiles[1].managed);
        assert!(profiles.iter().all(|profile| profile.fields.is_empty()));
    }

    #[test]
    fn empty_lists_and_blank_lines() {
        assert_eq!(parse_profiles("VPNs:\n").unwrap(), Vec::new());
        let profiles = parse_profiles("VPNs:\n\n  Personal VPNs:\n   \n    Home\n").unwrap();
        assert_eq!(profiles, vec![Profile::new("Home")]);
    }

    #[test]
    fn malformed_list() {
        for text in [
            "error",
            "VPNs:\nprofile",
            "VPNs:\n  strange output",
            "VPNs:\n    Orphan profile\n",
            "",
        ] {
            let error = parse_profiles(text).unwrap_err();
            assert_eq!(error, VpnError::failed(UNEXPECTED_LIST), "{text:?}");
        }
    }

    #[test]
    fn status() {
        assert_eq!(
            parse_status("Status: Not Running\n").unwrap(),
            Status::new(ConnectionState::Disconnected)
        );
        assert_eq!(
            parse_status("Status: Connected\nVPN Name: Office VPN\n").unwrap(),
            Status::with_profile(ConnectionState::Connected, "Office VPN")
        );
        for text in ["Status: Connecting...", "Status: Re-Connecting"] {
            assert_eq!(
                parse_status(text).unwrap().state,
                ConnectionState::Connecting
            );
        }
        assert_eq!(
            parse_status("Status: XAuth").unwrap().state,
            ConnectionState::AwaitingAuthentication
        );
        assert_eq!(
            parse_status("  Status:  Disconnecting...  \n")
                .unwrap()
                .state,
            ConnectionState::Disconnecting
        );
        assert_eq!(
            parse_status("Status: Disconnected\r\n").unwrap().state,
            ConnectionState::Disconnected
        );
        let error = parse_status("Status: mystery").unwrap_err();
        assert!(
            error
                .message()
                .starts_with("Unsupported FortiClient status.")
        );
        let error = parse_status("nothing here").unwrap_err();
        assert!(
            error
                .message()
                .starts_with("FortiClient returned an unexpected status.")
        );
    }

    #[test]
    fn status_profile_labels() {
        for label in [
            "VPN",
            "VPN name",
            "VPN Name",
            "Name",
            "Profile",
            "VPN tunnel",
            "vpn NAME",
        ] {
            let status = parse_status(&format!("Status: Connected\n{label}:  Lab  \n")).unwrap();
            assert_eq!(status.profile.as_deref(), Some("Lab"), "{label}");
        }
        assert_eq!(
            parse_status("Status: Connected\nGateway: vpn\n")
                .unwrap()
                .profile,
            None
        );
    }

    #[test]
    fn profile_excludes_secret_fields() {
        let profile = parse_profile(
            "Office VPN",
            "VPN: Office VPN\n  Username: worker\n  Save Password: Enabled\n  Password: SECRET\n  Token: SECRET\n  Client Certificate Password: SECRET\n  Shared Secret: SECRET\n",
            false,
        )
        .unwrap();
        assert_eq!(profile.username(), "worker");
        // Whether a password is saved decides how connect() asks for one.
        assert_eq!(profile.field("Save Password"), Some("Enabled"));
        assert_eq!(profile.field("Password"), None);
        assert_eq!(profile.field("Token"), None);
        assert_eq!(
            profile.fields,
            [
                ("Username".to_owned(), "worker".to_owned()),
                ("Save Password".to_owned(), "Enabled".to_owned())
            ]
        );
    }

    #[test]
    fn only_the_save_password_toggle_is_kept() {
        let view = |line: &str| parse_profile("Lab", &format!("VPN: Lab\n  {line}\n"), false);
        assert_eq!(
            view("Save Password: Disabled").unwrap().fields,
            [("Save Password".to_owned(), "Disabled".to_owned())]
        );
        // Anything else under a password-like key is treated as a secret.
        for line in [
            "Save Password: SECRET",
            "save password: Enabled",
            "Saved Password: Enabled",
            "Password Saved: Enabled",
        ] {
            assert!(view(line).unwrap().fields.is_empty(), "{line:?}");
        }
    }

    #[test]
    fn profile_fields_keep_order_and_values() {
        let output = "VPN: Lab\n  Remote Gateway: [2001:db8::1]:8443\n  Empty:\n  no separator\n  Username: a\n  Username: b\n";
        let profile = parse_profile("Lab", output, true).unwrap();
        assert!(profile.managed);
        assert_eq!(
            profile.fields,
            vec![
                ("Remote Gateway".to_owned(), "[2001:db8::1]:8443".to_owned()),
                ("Username".to_owned(), "b".to_owned())
            ]
        );
    }

    #[test]
    fn profile_requires_matching_header() {
        for output in [
            "VPN: Other\n  Username: worker\n",
            "VPN: Lab",
            "Username: worker\n",
        ] {
            let error = parse_profile("Lab", output, false).unwrap_err();
            assert_eq!(error.message(), "FortiClient could not read this profile.");
        }
    }

    #[test]
    fn auth_prompts_are_exact() {
        let prompts = auth_prompts();
        assert_eq!(first_match(prompts, "FortiToken:"), Some(PromptKind::Mfa));
        assert_eq!(
            first_match(prompts, "  Client Certificate password:"),
            Some(PromptKind::CertificatePassword)
        );
        assert_eq!(
            first_match(prompts, "Untrusted server asks for password:"),
            None
        );
    }

    #[test]
    fn auth_prompt_variants() {
        let prompts = auth_prompts();
        let cases = [
            ("Please input password.\n", PromptKind::Password),
            ("Password:", PromptKind::Password),
            ("Enter password: ", PromptKind::Password),
            ("Input VPN password :", PromptKind::Password),
            ("Please enter password:", PromptKind::Password),
            ("Connecting...\nPASSWORD:", PromptKind::Password),
            ("Please input username.", PromptKind::Username),
            ("Username:", PromptKind::Username),
            ("Enter user name:", PromptKind::Username),
            ("Input username: ", PromptKind::Username),
            ("\t username :", PromptKind::Username),
            ("Enter verification code: ", PromptKind::Mfa),
            ("Please input token.", PromptKind::Mfa),
            ("Please input verification code.", PromptKind::Mfa),
            ("Please input OTP.", PromptKind::Mfa),
            ("Token:", PromptKind::Mfa),
            ("One-time password:", PromptKind::Mfa),
            ("Enter one time code:", PromptKind::Mfa),
            ("OTP:", PromptKind::Mfa),
            ("Please enter two-factor token:", PromptKind::Mfa),
            ("Two factor code:", PromptKind::Mfa),
            (
                "Client Certificate password: ",
                PromptKind::CertificatePassword,
            ),
            ("  Verify Password:", PromptKind::VerifyPassword),
        ];
        for (text, kind) in cases {
            assert_prompt(prompts, text, kind);
        }
    }

    #[test]
    fn auth_prompts_reject_other_output() {
        let prompts = auth_prompts();
        for text in [
            "Untrusted server asks for password:",
            "Accept untrusted certificate? [y/n] ",
            "Password: SECRET",
            "Invalid code; try again\n",
            "Please input password",
            "Enter verifi",
            "DONE.\n",
            "",
        ] {
            assert_eq!(first_match(prompts, text), None, "{text:?}");
        }
    }

    #[test]
    fn edit_prompts_follow_the_installed_dialogue() {
        let prompts = edit_prompts();
        let cases = [
            (
                "Remote Gateway [current=localhost:443]: ",
                PromptKind::Gateway,
            ),
            ("Remote Gateway:", PromptKind::Gateway),
            ("Remote Gateway [default=]:", PromptKind::Gateway),
            (
                "Authentication (1.prompt / 2.save / 3.disable) [current=save]: ",
                PromptKind::Authentication,
            ),
            (
                "Authentication (1.prompt / 2.save / 3.disable) [default=1]:",
                PromptKind::Authentication,
            ),
            ("  Username: [current=worker]", PromptKind::Username),
            ("  Username: [current=]", PromptKind::Username),
            ("Username:", PromptKind::Username),
            (
                "Certificate Type (1.local (pkcs12) / 2.smartcard (pkcs11) / 3.disable) [current=disable]: ",
                PromptKind::CertificateType,
            ),
            (
                "Client Certificate password:",
                PromptKind::CertificatePassword,
            ),
            ("Verify Password: ", PromptKind::VerifyPassword),
            (
                "Client Certificate path (.p12 file URL) [default=None]: ",
                PromptKind::Certificate,
            ),
            (
                "PKCS#11 library (library name or file URL) [current=/usr/lib/opensc.so]:",
                PromptKind::Certificate,
            ),
        ];
        for (text, kind) in cases {
            assert_prompt(prompts, text, kind);
        }
        assert_eq!(
            first_match(prompts, "Authentication (1.prompt / 2.save / 3.disable):"),
            None
        );
        assert_eq!(first_match(prompts, "DONE.\n"), None);
    }

    #[test]
    fn split_prompts_match_only_once_complete() {
        let auth = auth_prompts();
        let edit = edit_prompts();
        for (prompts, text) in [
            (auth, "Please input password.\n"),
            (auth, "Enter verification code: "),
            (edit, "Remote Gateway [current=localhost:443]: "),
            (
                edit,
                "Authentication (1.prompt / 2.save / 3.disable) [current=save]: ",
            ),
            (edit, "  Username: [current=worker]"),
            (
                edit,
                "Certificate Type (1.local (pkcs12) / 2.smartcard (pkcs11) / 3.disable) [current=disable]: ",
            ),
            (
                edit,
                "Client Certificate path (.p12 file URL) [default=None]: ",
            ),
        ] {
            assert_eq!(first_match(prompts, first_half(text)), None, "{text:?}");
            assert!(first_match(prompts, text).is_some(), "{text:?}");
        }
    }

    #[test]
    fn prompts_are_case_insensitive() {
        assert_eq!(
            first_match(auth_prompts(), "please INPUT password."),
            Some(PromptKind::Password)
        );
        assert_eq!(
            first_match(edit_prompts(), "remote gateway:"),
            Some(PromptKind::Gateway)
        );
    }

    #[test]
    fn failure_messages_do_not_expose_raw_cli_credentials() {
        let output = "ERROR: Internal communication failed SUPERSECRET 123SECRET";
        let message = cli_failure(output, 1).unwrap();
        assert!(message.contains("VPN service"));
        assert!(!message.contains("SUPERSECRET"));
        assert!(!cli_messages(output).join("\n").contains("123SECRET"));
        assert!(
            cli_failure("Incorrect password", 1)
                .unwrap()
                .contains("authentication")
        );
        assert!(
            cli_failure("Login failed: Too many bad login attempts.", 0)
                .unwrap()
                .contains("temporarily blocked login")
        );
        assert_eq!(
            cli_failure("Failed to save credential to key store.\nDONE.", 0),
            None
        );
        assert!(
            cli_failure("Failed to save credential to key store.", 1)
                .unwrap()
                .contains("password storage")
        );
    }

    #[test]
    fn failure_precedence() {
        assert_eq!(cli_failure("DONE.\n", 0), None);
        assert_eq!(cli_failure("", 0), None);
        assert_eq!(cli_failure("DONE.\n", 2), Some(GENERIC_FAILURE));
        assert_eq!(cli_failure("Something failed.", 0), Some(GENERIC_FAILURE));
        // A failure word that is part of another word is not a failure.
        assert_eq!(cli_failure("Errors: 0\nFailedOver: no\n", 0), None);
        // A storage warning alongside a real failure: the failure wins.
        let output =
            "Failed to set credentials to GNOME Keyring.\nVPN tunnel does not exist\nerror";
        assert_eq!(
            cli_failure(output, 0),
            Some("FortiClient could not find the selected VPN profile. Refresh profiles.")
        );
        // Table order decides between several recognised failures.
        assert_eq!(
            cli_failure("Connection failed\nToo many bad login attempts", 1),
            Some(LOGIN_BLOCKED_MESSAGE)
        );
        // A warning that is not on its own line still counts as output.
        assert_eq!(
            cli_failure("Note: Failed to save credential to key store. Retry", 0),
            Some(STORAGE_WARNING)
        );
        assert_eq!(
            cli_failure("  failed to save credential to KEY STORE.  \r\nDONE.", 0),
            None
        );
    }

    #[test]
    fn messages_follow_table_order() {
        assert_eq!(
            cli_messages(
                "Connection failed. Access denied. Failed to save credential to key store"
            ),
            vec![
                STORAGE_WARNING,
                "The VPN server rejected authentication. Check the username, password, and MFA requirements.",
                "FortiClient reported a connection failure. Check the VPN server and service availability.",
            ]
        );
        assert_eq!(
            cli_messages("Unable to use system’s key store"),
            vec![STORAGE_WARNING]
        );
        assert_eq!(
            cli_messages("Failed to set credentials to GNOME Keyring."),
            vec![STORAGE_WARNING]
        );
        assert_eq!(
            cli_messages("Service rejected our request (code: 12)"),
            vec!["The FortiClient service rejected the connection request."]
        );
        assert!(cli_messages("DONE.").is_empty());
    }

    #[test]
    fn storage_warning_names_no_particular_desktop() {
        assert!(STORAGE_WARNING.contains("keyring or wallet"));
        assert!(!STORAGE_WARNING.contains("login keyring"));
    }

    #[test]
    fn stopped_service_is_recognised() {
        let vpn_output = "Failed to get response from confighandler\n";
        let version_output = "Unable to display version info: unable to retrieve version info: client port not found: open /var/run/forticlient/service_port: no such file or directory\n";
        for output in [
            vpn_output,
            version_output,
            "FAILED TO GET RESPONSE FROM CONFIGHANDLER",
        ] {
            assert!(service_stopped(output), "{output:?}");
            assert_eq!(cli_messages(output), [SERVICE_STOPPED], "{output:?}");
        }
        assert_eq!(cli_failure(vpn_output, 1), Some(SERVICE_STOPPED));
        assert_eq!(cli_failure(vpn_output, 0), Some(SERVICE_STOPPED));
        // Only a login block comes first.
        assert_eq!(
            cli_failure(&format!("Too many bad login attempts\n{vpn_output}"), 1),
            Some(LOGIN_BLOCKED_MESSAGE)
        );
        assert_eq!(
            cli_failure(&format!("{vpn_output}Load VPN profile was failed"), 1),
            Some(SERVICE_STOPPED)
        );
        for output in ["Status: Not Running", "confighandler", "client port", ""] {
            assert!(!service_stopped(output), "{output:?}");
        }
    }

    #[test]
    fn vpn_log_messages_are_fixed_descriptions() {
        let log = "Login failed: too many BAD login attempts.\npassword SUPERSECRET\nDBUS session bus address not found\n";
        let messages = vpn_log_messages(log);
        assert_eq!(messages[0], LOGIN_BLOCKED_MESSAGE);
        assert!(messages[1].contains("desktop session bus"));
        assert!(!messages.join("\n").contains("SUPERSECRET"));
        assert!(vpn_log_messages("Tunnel up").is_empty());
    }

    #[test]
    fn version_drops_device_identifiers() {
        let output = "FortiClient Version: 7.4.3.5411\nFortiClient Serial: SECRET-SERIAL\nFortiClient UID: SECRET-UID\n";
        assert_eq!(parse_version(output).as_deref(), Some("7.4.3.5411"));
        assert_eq!(
            parse_version("FortiClient Version:\t7.4\r\n").as_deref(),
            Some("7.4")
        );
        assert_eq!(
            parse_version("Banner\nFortiClient Version: 7.0.1\n").as_deref(),
            Some("7.0.1")
        );
        // A lone carriage return ends a line, as with Python's text=True.
        assert_eq!(
            parse_version("Banner\rFortiClient Version: 7.4.3.5411 \rFortiClient UID: SECRET\r")
                .as_deref(),
            Some("7.4.3.5411")
        );
        for output in [
            "FortiClient Version: 7",
            "FortiClient Version: 7.4.3.5411.1",
            "FortiClient Version: 7.4.3\u{0b}",
            "Version: 7.4.3",
            "SECRET",
            "",
        ] {
            assert_eq!(parse_version(output), None, "{output:?}");
        }
    }

    #[test]
    fn newlines_are_normalised_like_python_text_mode() {
        assert_eq!(normalize_newlines("a\r\nb\rc\nd\r\r\n"), "a\nb\nc\nd\n\n");
        assert_eq!(normalize_newlines("\r"), "\n");
        assert_eq!(normalize_newlines("plain"), "plain");
    }

    #[test]
    fn package_versions_keep_only_the_dotted_version() {
        for (output, version) in [
            ("7.4.3.5411", "7.4.3.5411"),
            ("1:7.4.3.5411-1ubuntu1\n", "7.4.3.5411"),
            ("forticlient-vpn 7.4.0.1636-1\n", "7.4.0.1636"),
            ("forticlient 7.2.4.0850-1", "7.2.4.0850"),
            ("7.4", "7.4"),
        ] {
            assert_eq!(
                parse_package_version(output).as_deref(),
                Some(version),
                "{output:?}"
            );
        }
        for output in [
            "",
            "SECRET",
            "7",
            "1.2.3.4.5",
            "7.4.3.5411.",
            "package forticlient is not installed",
            "forticlient\t\n",
        ] {
            assert_eq!(parse_package_version(output), None, "{output:?}");
        }
    }

    #[test]
    fn dpkg_versions_count_only_for_installed_packages() {
        for (output, version) in [
            ("ii 7.4.3.5411", "7.4.3.5411"),
            ("ii 1:7.4.3.5411-1ubuntu1", "7.4.3.5411"),
            ("ii 7.4.3.5411ii 7.4.3.5411", "7.4.3.5411"),
            // Held with `apt-mark hold`, still installed.
            ("hi 7.4.3.5411", "7.4.3.5411"),
        ] {
            assert_eq!(
                parse_dpkg_version(output).as_deref(),
                Some(version),
                "{output:?}"
            );
        }
        for output in [
            // Removed with its configuration kept, half installed or
            // configured, never installed, or broken.
            "rc 7.4.3.5411",
            "iU 7.4.3.5411",
            "iF 7.4.3.5411",
            "un ",
            "iiR7.4.3.5411",
            "hiR7.4.3.5411",
            "Ii 7.4.3.5411",
            // Without a status, or without a version.
            "7.4.3.5411",
            " ii 7.4.3.5411",
            "ii ",
            "ii SECRET",
            "",
        ] {
            assert_eq!(parse_dpkg_version(output), None, "{output:?}");
        }
    }

    #[test]
    fn lines_split_like_python() {
        let lines = |text| split_lines(text).collect::<Vec<_>>();
        assert_eq!(
            lines("a\nb\r\nc\rd\u{0b}e\u{2028}f\n"),
            ["a", "b", "c", "d", "e", "f"]
        );
        assert_eq!(lines("a\n\nb"), ["a", "", "b"]);
        assert_eq!(lines("\n"), [""]);
        assert!(lines("").is_empty());
    }
}
