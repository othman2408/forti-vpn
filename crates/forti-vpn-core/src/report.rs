//! The copyable troubleshooting report: an allowlist of facts, never profile
//! names, gateways, credentials, or device identifiers.
use crate::model::{ClientInfo, ConnectionState};
use crate::platform::{Platform, Sandbox};

/// What this release supports, shown in Settings and the report.
pub const COMPATIBILITY: &str =
    "FortiClient 7.4 VPN CLI on Linux with GTK 4.14 and libadwaita 1.5 or newer";

#[derive(Debug, Clone, Copy)]
pub struct ReportInputs<'a> {
    pub platform: &'a Platform,
    /// e.g. `4.22.4`
    pub gtk_version: &'a str,
    /// e.g. `1.9.1`
    pub adwaita_version: &'a str,
    pub client: &'a ClientInfo,
    pub status: ConnectionState,
    pub tray_available: bool,
    /// Already-sanitized session events.
    pub diagnostics: &'a [String],
}

/// Lines, in order: `Forti-VPN <version>`, `Unofficial FortiClient GUI`,
/// `System: <PRETTY_NAME>`, `Architecture: <arch>`, `Desktop: <desktop>`,
/// `Session: <Wayland|X11|Unknown>`, `Installation: <Native|Flatpak>`,
/// `GTK: …`, `libadwaita: …`, `FortiClient: …`, `CLI: …`,
/// `Supported: <COMPATIBILITY>`, `VPN status: <state>`,
/// `Tray icon: <Available|Unavailable>`, blank,
/// `Sanitized session events (no profiles, credentials, or device identifiers):`,
/// the events, blank, `Support: <repository>/issues`.
pub fn troubleshooting_report(inputs: &ReportInputs<'_>) -> String {
    let platform = inputs.platform;
    let installation = match platform.sandbox {
        Sandbox::None => "Native",
        Sandbox::Flatpak => "Flatpak",
    };
    let tray = if inputs.tray_available {
        "Available"
    } else {
        "Unavailable"
    };
    let mut lines = vec![
        format!("Forti-VPN {}", crate::VERSION),
        "Unofficial FortiClient GUI".to_owned(),
        format!("System: {}", platform.os.pretty_name),
        format!("Architecture: {}", platform.architecture),
        format!("Desktop: {}", platform.desktop_name),
        format!("Session: {}", platform.session.as_str()),
        format!("Installation: {installation}"),
        format!("GTK: {}", inputs.gtk_version),
        format!("libadwaita: {}", inputs.adwaita_version),
        format!("FortiClient: {}", inputs.client.version),
        format!("CLI: {}", inputs.client.path),
        format!("Supported: {COMPATIBILITY}"),
        format!("VPN status: {}", inputs.status.as_str()),
        format!("Tray icon: {tray}"),
        String::new(),
        "Sanitized session events (no profiles, credentials, or device identifiers):".to_owned(),
    ];
    lines.extend(inputs.diagnostics.iter().cloned());
    lines.push(String::new());
    lines.push(format!("Support: {}/issues", crate::REPOSITORY));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Elevation;

    const UBUNTU: &str = "ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=\"Ubuntu 26.04 LTS\"\n";

    fn platform(desktop: &str, sandbox: Sandbox) -> Platform {
        let env = |name: &str| match name {
            "XDG_CURRENT_DESKTOP" => Some(desktop.to_owned()),
            "XDG_SESSION_TYPE" => Some("wayland".to_owned()),
            _ => None,
        };
        Platform::from_parts(
            UBUNTU,
            &env,
            sandbox,
            false,
            "x86_64",
            Elevation::Sudo,
            None,
        )
    }

    fn report(
        platform: &Platform,
        client: &ClientInfo,
        tray_available: bool,
        diagnostics: &[String],
    ) -> String {
        troubleshooting_report(&ReportInputs {
            platform,
            gtk_version: "4.22.4",
            adwaita_version: "1.9.1",
            client,
            status: ConnectionState::Connected,
            tray_available,
            diagnostics,
        })
    }

    #[test]
    fn version_output_drops_device_identifiers() {
        // The values `cli::parse_version` keeps from `forticlient version`
        // output that also listed `SECRET-SERIAL` and `SECRET-UID`.
        let client = ClientInfo {
            version: "7.4.3.5411".into(),
            path: "/opt/forticlient/forticlient-cli".into(),
        };
        let diagnostics = ["12:00:00 FortiClient requested password.".to_owned()];
        let text = report(
            &platform("ubuntu:GNOME", Sandbox::None),
            &client,
            true,
            &diagnostics,
        );
        assert!(text.contains("VPN status: connected"));
        assert!(!text.contains("PRIVATE-PROFILE"));
        assert!(!text.contains("SECRET"));
    }

    #[test]
    fn lines_in_order() {
        let client = ClientInfo::unavailable();
        let diagnostics = [
            "12:00:00 Started FortiClient connect.".to_owned(),
            "12:00:01 FortiClient requested mfa.".to_owned(),
        ];
        let text = report(
            &platform("ubuntu:GNOME", Sandbox::Flatpak),
            &client,
            false,
            &diagnostics,
        );
        let expected = [
            format!("Forti-VPN {}", crate::VERSION).as_str(),
            "Unofficial FortiClient GUI",
            "System: Ubuntu 26.04 LTS",
            "Architecture: x86_64",
            "Desktop: ubuntu:GNOME",
            "Session: Wayland",
            "Installation: Flatpak",
            "GTK: 4.22.4",
            "libadwaita: 1.9.1",
            "FortiClient: Unavailable",
            "CLI: Not detected",
            "Supported: FortiClient 7.4 VPN CLI on Linux with GTK 4.14 and libadwaita 1.5 or newer",
            "VPN status: connected",
            "Tray icon: Unavailable",
            "",
            "Sanitized session events (no profiles, credentials, or device identifiers):",
            "12:00:00 Started FortiClient connect.",
            "12:00:01 FortiClient requested mfa.",
            "",
            "Support: https://github.com/othman2408/forti-vpn/issues",
        ]
        .join("\n");
        assert_eq!(text, expected);
    }

    #[test]
    fn desktop_is_the_sanitized_name() {
        let client = ClientInfo::unavailable();
        let text = report(
            &platform("GNOME TOKEN=secret", Sandbox::None),
            &client,
            true,
            &[],
        );
        assert!(text.contains("\nDesktop: Unknown\n"));
        assert!(!text.contains("secret"));
        assert!(text.contains("\nInstallation: Native\n"));
        assert!(text.contains("\nTray icon: Available\n"));
    }
}
