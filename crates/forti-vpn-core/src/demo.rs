//! A simulated FortiClient for demos, screenshots, and UI development.
//!
//! Enabled in the app with `FORTI_VPN_DEMO=1`. Nothing here touches the
//! system; every value is invented.
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crate::backend::{Backend, OperationContext};
use crate::error::{CANCELLED, VpnError};
use crate::model::{
    AuthenticationMode, CertificateKind, ClientInfo, ConnectionState, Profile, ProfileSettings,
    PromptKind, Status,
};

/// Environment variable that switches the app to [`DemoBackend`].
pub const DEMO_VARIABLE: &str = "FORTI_VPN_DEMO";

const STEP: Duration = Duration::from_millis(700);

struct DemoState {
    status: Status,
    profiles: Vec<Profile>,
}

pub struct DemoBackend {
    state: Mutex<DemoState>,
}

fn sample_profile(name: &str, gateway: &str, username: &str) -> Profile {
    Profile::with_fields(
        name,
        [
            ("Remote Gateway", gateway),
            ("Username", username),
            ("Authentication", "Prompt"),
            ("Client Certificate", "None"),
            ("Single Sign On (SSO) for VPN Tunnel", "Disabled"),
            ("Auto Connect", "Disabled"),
            ("Always Up", "Disabled"),
            ("Save Password", "Disabled"),
        ],
    )
}

impl Default for DemoBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl DemoBackend {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(DemoState {
                status: Status::new(ConnectionState::Disconnected),
                profiles: vec![
                    sample_profile("Office VPN", "vpn.example.com:10443", "alex"),
                    sample_profile("Lab Network", "lab.example.org", "alex.dev"),
                ],
            }),
        }
    }

    fn with_state<T>(&self, action: impl FnOnce(&mut DemoState) -> T) -> T {
        action(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn pause(context: &OperationContext<'_>) -> Result<(), VpnError> {
        if context.cancel.wait_timeout(STEP) {
            return Err(VpnError::cancelled(CANCELLED));
        }
        Ok(())
    }

    fn ask(context: &OperationContext<'_>, kind: PromptKind) -> Result<(), VpnError> {
        (context.log)(&format!("FortiClient requested {}.", kind.description()));
        match (context.ask)(kind)? {
            Some(_) => Ok(()),
            None => Err(VpnError::cancelled(CANCELLED)),
        }
    }
}

impl Backend for DemoBackend {
    fn interactive_timeout(&self) -> Duration {
        Duration::from_secs(120)
    }

    fn status(&self) -> Result<Status, VpnError> {
        Ok(self.with_state(|state| state.status.clone()))
    }

    fn list_profiles(&self) -> Result<Vec<Profile>, VpnError> {
        Ok(self.with_state(|state| {
            state
                .profiles
                .iter()
                .map(|profile| Profile::new(profile.name.clone()))
                .collect()
        }))
    }

    fn view(&self, profile: &Profile) -> Result<Profile, VpnError> {
        self.with_state(|state| {
            state
                .profiles
                .iter()
                .find(|item| item.name == profile.name)
                .cloned()
        })
        .ok_or_else(|| VpnError::failed("FortiClient could not read this profile."))
    }

    fn connect(
        &self,
        profile: &Profile,
        _username: &str,
        _remember: bool,
        context: &OperationContext<'_>,
    ) -> Result<Status, VpnError> {
        (context.log)("Started FortiClient connect.");
        Self::ask(context, PromptKind::Password)?;
        Self::pause(context)?;
        Self::ask(context, PromptKind::Mfa)?;
        Self::pause(context)?;
        (context.log)("FortiClient finished connect; verifying the result.");
        Ok(self.with_state(|state| {
            state.status = Status::with_profile(ConnectionState::Connected, profile.name.clone());
            state.status.clone()
        }))
    }

    fn disconnect(&self) -> Result<Status, VpnError> {
        std::thread::sleep(STEP);
        Ok(self.with_state(|state| {
            state.status = Status::new(ConnectionState::Disconnected);
            state.status.clone()
        }))
    }

    fn save_profile(
        &self,
        settings: &ProfileSettings,
        existing: Option<&Profile>,
        context: &OperationContext<'_>,
    ) -> Result<Profile, VpnError> {
        settings.validate()?;
        (context.log)("Started FortiClient edit.");
        Self::pause(context)?;
        self.with_state(|state| {
            let exists = state
                .profiles
                .iter()
                .any(|profile| profile.name == settings.name);
            if existing.is_none() && exists {
                return Err(VpnError::failed("A profile with this name already exists."));
            }
            let mut profile = existing.cloned().unwrap_or_else(|| {
                sample_profile(&settings.name, &settings.gateway, &settings.username)
            });
            let mut set = |key: &str, value: &str| match profile
                .fields
                .iter_mut()
                .find(|(name, _)| name == key)
            {
                Some((_, current)) => *current = value.to_owned(),
                None => profile.fields.push((key.to_owned(), value.to_owned())),
            };
            set("Remote Gateway", &settings.gateway);
            if settings.authentication == AuthenticationMode::Save {
                set("Username", &settings.username);
            }
            if settings.certificate_kind.needs_path() {
                set("Client Certificate", &settings.certificate);
            } else if settings.certificate_kind == CertificateKind::Disabled {
                set("Client Certificate", "None");
            }
            state.profiles.retain(|item| item.name != profile.name);
            state.profiles.push(profile.clone());
            Ok(profile)
        })
    }

    fn remove(&self, profile: &Profile) -> Result<(), VpnError> {
        self.with_state(|state| {
            if state.status.state != ConnectionState::Disconnected
                && state
                    .status
                    .profile
                    .as_deref()
                    .is_none_or(|name| name == profile.name)
            {
                return Err(VpnError::failed(
                    "Disconnect the active VPN before changing this profile.",
                ));
            }
            state.profiles.retain(|item| item.name != profile.name);
            Ok(())
        })
    }

    fn information(&self) -> ClientInfo {
        ClientInfo {
            version: "7.4.3 (demo)".into(),
            path: "Demo mode — no FortiClient".into(),
        }
    }
}
