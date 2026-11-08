//! Domain types for FortiClient profiles, connection status, and prompts.
use zeroize::Zeroizing;

use crate::error::VpnError;

/// A password, MFA code, or other secret typed by the user. The buffer is
/// wiped when dropped.
pub type Secret = Zeroizing<String>;

/// A FortiClient VPN profile.
///
/// `fields` keeps FortiClient's own order (as printed by `forticlient vpn
/// view`) and never contains password, secret, or token values. The one
/// exception is the `Save Password` toggle (`Enabled`/`Disabled`), which
/// says whether FortiClient has a saved password, never the password.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Profile {
    pub name: String,
    pub fields: Vec<(String, String)>,
    /// Company-managed (EMS) profiles cannot be edited or removed.
    pub managed: bool,
}

impl Profile {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    pub fn with_fields<K: Into<String>, V: Into<String>>(
        name: impl Into<String>,
        fields: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        Self {
            name: name.into(),
            fields: fields
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
            managed: false,
        }
    }

    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    pub fn username(&self) -> &str {
        self.field("Username").unwrap_or("")
    }

    pub fn gateway(&self) -> &str {
        self.field("Remote Gateway").unwrap_or("")
    }
}

/// Connection state as reported by `forticlient vpn status`, plus
/// `Unavailable` when FortiClient cannot be queried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ConnectionState {
    #[default]
    Unavailable,
    Disconnected,
    Connecting,
    AwaitingAuthentication,
    Connected,
    Disconnecting,
}

impl ConnectionState {
    /// Stable lowercase name used in diagnostics and reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::AwaitingAuthentication => "awaiting authentication",
            Self::Connected => "connected",
            Self::Disconnecting => "disconnecting",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    pub state: ConnectionState,
    /// The active profile name, when FortiClient reports one.
    pub profile: Option<String>,
}

impl Status {
    pub fn new(state: ConnectionState) -> Self {
        Self {
            state,
            profile: None,
        }
    }

    pub fn with_profile(state: ConnectionState, profile: impl Into<String>) -> Self {
        Self {
            state,
            profile: Some(profile.into()),
        }
    }

    pub fn unavailable() -> Self {
        Self::new(ConnectionState::Unavailable)
    }
}

/// The profile editor's authentication choice. `Keep` preserves the CLI's
/// current value when editing an existing profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthenticationMode {
    #[default]
    Keep,
    Prompt,
    Save,
    Disabled,
}

impl AuthenticationMode {
    /// The answer typed into FortiClient's
    /// `Authentication (1.prompt / 2.save / 3.disable)` prompt.
    pub fn cli_value(self) -> &'static str {
        match self {
            Self::Keep => "",
            Self::Prompt => "1",
            Self::Save => "2",
            Self::Disabled => "3",
        }
    }
}

/// The profile editor's client-certificate choice. `Keep` preserves the
/// CLI's current value when editing an existing profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CertificateKind {
    #[default]
    Keep,
    /// A local PKCS#12 (.p12/.pfx) file.
    Local,
    /// A PKCS#11 smartcard library.
    Smartcard,
    Disabled,
}

impl CertificateKind {
    /// The answer typed into FortiClient's
    /// `Certificate Type (1.local (pkcs12) / 2.smartcard (pkcs11) / 3.disable)` prompt.
    pub fn cli_value(self) -> &'static str {
        match self {
            Self::Keep => "",
            Self::Local => "1",
            Self::Smartcard => "2",
            Self::Disabled => "3",
        }
    }

    pub fn needs_path(self) -> bool {
        matches!(self, Self::Local | Self::Smartcard)
    }
}

/// Values the profile editor submits to `forticlient vpn edit`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfileSettings {
    pub name: String,
    /// A FortiClient gateway value such as `vpn.example.org`,
    /// `vpn.example.org:10443`, or `[2001:db8::1]:8443`.
    pub gateway: String,
    pub authentication: AuthenticationMode,
    pub username: String,
    pub certificate_kind: CertificateKind,
    /// A certificate path or PKCS#11 library, when `certificate_kind` needs one.
    pub certificate: String,
}

impl ProfileSettings {
    /// Reject values FortiClient's line-based dialogue cannot accept safely.
    pub fn validate(&self) -> Result<(), VpnError> {
        let single_line = [&self.name, &self.gateway, &self.username, &self.certificate]
            .iter()
            .all(|value| !value.contains(['\n', '\r', '\0']));
        if !single_line {
            return Err(VpnError::failed(
                "Profile fields must contain a single line.",
            ));
        }
        if self.name.trim().is_empty() || self.name.starts_with('-') {
            return Err(VpnError::failed(
                "Enter a profile name that does not start with a dash.",
            ));
        }
        if self.gateway.trim().is_empty() {
            return Err(VpnError::failed("Enter a remote gateway."));
        }
        if self.authentication == AuthenticationMode::Save && self.username.trim().is_empty() {
            return Err(VpnError::failed(
                "Enter a username for saved authentication.",
            ));
        }
        if self.certificate_kind.needs_path() && self.certificate.trim().is_empty() {
            return Err(VpnError::failed(
                "Enter a certificate path or PKCS#11 library.",
            ));
        }
        Ok(())
    }
}

/// A question FortiClient asks on its terminal during `connect` or `edit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PromptKind {
    Gateway,
    Authentication,
    Username,
    CertificateType,
    CertificatePassword,
    VerifyPassword,
    Certificate,
    Password,
    Mfa,
}

impl PromptKind {
    /// Stable snake_case identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Authentication => "authentication",
            Self::Username => "username",
            Self::CertificateType => "certificate_type",
            Self::CertificatePassword => "certificate_password",
            Self::VerifyPassword => "verify_password",
            Self::Certificate => "certificate",
            Self::Password => "password",
            Self::Mfa => "mfa",
        }
    }

    /// Words used in diagnostics: "FortiClient requested certificate password."
    pub fn description(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Authentication => "authentication",
            Self::Username => "username",
            Self::CertificateType => "certificate type",
            Self::CertificatePassword => "certificate password",
            Self::VerifyPassword => "verify password",
            Self::Certificate => "certificate",
            Self::Password => "password",
            Self::Mfa => "mfa",
        }
    }

    /// Whether the answer must be hidden while typing.
    pub fn is_secret(self) -> bool {
        matches!(
            self,
            Self::Password | Self::Mfa | Self::CertificatePassword | Self::VerifyPassword
        )
    }
}

/// Public FortiClient version information. Serial numbers and device
/// identifiers are never included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    /// A dotted version such as `7.4.3.5411`, or `Could not determine` /
    /// `Unavailable`.
    pub version: String,
    /// The executable path, with the home directory shortened to `~`, or
    /// `Not detected`.
    pub path: String,
}

impl ClientInfo {
    pub fn unavailable() -> Self {
        Self {
            version: "Unavailable".into(),
            path: "Not detected".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_rejects_unsafe_values() {
        let base = ProfileSettings {
            name: "name".into(),
            gateway: "host".into(),
            ..Default::default()
        };
        for settings in [
            ProfileSettings {
                name: "-option".into(),
                ..base.clone()
            },
            ProfileSettings {
                name: "name\nInjected".into(),
                ..base.clone()
            },
            ProfileSettings {
                gateway: String::new(),
                ..base.clone()
            },
            ProfileSettings {
                authentication: AuthenticationMode::Save,
                ..base.clone()
            },
            ProfileSettings {
                certificate_kind: CertificateKind::Local,
                ..base.clone()
            },
        ] {
            assert!(settings.validate().is_err(), "{settings:?}");
        }
        assert!(base.validate().is_ok());
    }

    #[test]
    fn profile_fields_keep_order() {
        let profile = Profile::with_fields(
            "Office",
            [("Remote Gateway", "vpn:443"), ("Username", "worker")],
        );
        assert_eq!(profile.gateway(), "vpn:443");
        assert_eq!(profile.username(), "worker");
        assert_eq!(profile.fields[0].0, "Remote Gateway");
        assert_eq!(Profile::new("Empty").username(), "");
    }
}
