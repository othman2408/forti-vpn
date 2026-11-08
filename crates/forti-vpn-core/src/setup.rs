//! Distribution-aware FortiClient installation and tray guidance.
//!
//! Administrator commands are only ever *shown and copied* for the user to
//! run in a terminal; the app never executes them.
use std::path::Path;

use crate::platform::{Desktop, DistroFamily, Elevation, Platform, Sandbox};

pub const DOWNLOAD_URL: &str = "https://www.fortinet.com/support/product-downloads";
pub const HELP_URL: &str = "https://docs.fortinet.com/document/forticlient/7.4.3/administration-guide/664703/installing-forticlient-on-computers";
pub const APPINDICATOR_EXTENSION_URL: &str =
    "https://extensions.gnome.org/extension/615/appindicator-support/";

/// The AUR package that repackages Fortinet's official `.deb`.
const AUR_PACKAGE: &str = "forticlient-vpn";
const APPINDICATOR_PACKAGE: &str = "gnome-shell-extension-appindicator";
const APPINDICATOR_NAME: &str = "“AppIndicator and KStatusNotifierItem Support”";
/// The extension Ubuntu and its GNOME derivatives ship.
const UBUNTU_APPINDICATORS_UUID: &str = "ubuntu-appindicators@ubuntu.com";
/// Zorin OS ships its own, "Zorin Appindicators", instead.
const ZORIN_APPINDICATORS_UUID: &str = "zorin-appindicator@zorinos.com";
const SERVICE_COMMAND: &str = "systemctl enable --now forticlient.service";
/// Exists while systemd manages the system (see `sd_booted(3)`).
const SYSTEMD_RUNTIME_DIRECTORY: &str = "/run/systemd/system";
const RESTART_TO_INSTALL: &str = "Restart to finish installing.";

/// A local package file a user can select in the setup dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackageFormat {
    Deb,
    Rpm,
    /// `.pkg.tar.zst` (or legacy `.pkg.tar.xz`) built from the AUR.
    Arch,
}

impl PackageFormat {
    /// e.g. `.deb`, `.rpm`, `.pkg.tar.zst`
    pub fn extension(self) -> &'static str {
        match self {
            Self::Deb => ".deb",
            Self::Rpm => ".rpm",
            Self::Arch => ".pkg.tar.zst",
        }
    }

    /// File chooser filter name, e.g. `Debian packages (.deb)`.
    pub fn filter_name(self) -> &'static str {
        match self {
            Self::Deb => "Debian packages (.deb)",
            Self::Rpm => "RPM packages (.rpm)",
            Self::Arch => "Arch packages (.pkg.tar.zst)",
        }
    }

    /// File chooser glob patterns (lower and upper case).
    pub fn patterns(self) -> &'static [&'static str] {
        match self {
            Self::Deb => &["*.deb", "*.DEB"],
            Self::Rpm => &["*.rpm", "*.RPM"],
            Self::Arch => &["*.pkg.tar.zst", "*.pkg.tar.xz"],
        }
    }

    /// Case-insensitive suffix check. Like Python's `Path.suffix`, a name
    /// that is only the extension (`.deb`) does not count.
    pub fn matches(self, path: &Path) -> bool {
        let Some(name) = path.file_name() else {
            return false;
        };
        let name = name.as_encoded_bytes();
        self.patterns()
            .iter()
            .filter_map(|pattern| pattern.strip_prefix('*'))
            .any(|suffix| {
                name.len() > suffix.len()
                    && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
            })
    }
}

/// What the setup dialog shows for installing FortiClient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallGuide {
    /// The package file type to select, or `None` when this distribution
    /// has no file-based install path.
    pub format: Option<PackageFormat>,
    /// e.g. `Linux x86-64 (amd64) .deb`, `Linux ARM64 .rpm`. Shown under
    /// "Download".
    pub download_description: String,
    /// Where to get FortiClient, e.g. "Download FortiClient for Linux from
    /// Fortinet." or "FortiClient is available from the AUR as
    /// forticlient-vpn."
    pub download_hint: String,
    /// A repository install command (e.g. an AUR helper), if one is the
    /// usual route on this distribution.
    pub repository_command: Option<String>,
    /// Extra caveats, e.g. "Restart to finish installing." on
    /// image-based systems.
    pub note: Option<String>,
}

pub fn install_guide(platform: &Platform) -> InstallGuide {
    let format = installer(platform).map(|(format, _)| format);
    let (download_description, download_hint, repository_command) = match format {
        Some(PackageFormat::Arch) => (
            format!("AUR package {AUR_PACKAGE} (x86-64)"),
            format!("FortiClient is available from the AUR as {AUR_PACKAGE}."),
            Some(aur_command(platform.aur_helper.as_deref())),
        ),
        Some(package) => (
            download_description(&platform.architecture, package.extension()),
            "Download FortiClient for Linux from Fortinet.".to_owned(),
            None,
        ),
        None => (
            download_description(&platform.architecture, ".deb or .rpm"),
            "Fortinet publishes FortiClient as .deb and .rpm packages. Use your distribution’s \
             instructions or your IT team’s package."
                .to_owned(),
            None,
        ),
    };
    InstallGuide {
        format,
        download_description,
        download_hint,
        repository_command,
        note: install_note(platform),
    }
}

/// Like the Python `package_description()`, for `.deb`, `.rpm`, or both.
fn download_description(architecture: &str, package: &str) -> String {
    match architecture.to_ascii_lowercase().as_str() {
        "x86_64" | "amd64" if package == ".deb" => "Linux x86-64 (amd64) .deb".to_owned(),
        "x86_64" | "amd64" => format!("Linux x86-64 {package}"),
        "aarch64" | "arm64" => format!("Linux ARM64 {package}"),
        _ => format!("Linux {package} for your computer"),
    }
}

fn install_note(platform: &Platform) -> Option<String> {
    let caveat = match platform.family {
        DistroFamily::Suse if platform.os.is_opensuse() => {
            Some("Fortinet doesn’t officially support openSUSE. Use the RPM package.")
        }
        DistroFamily::Suse => {
            Some("Fortinet doesn’t officially support SUSE Linux. Use the RPM package.")
        }
        DistroFamily::Fedora if platform.image_based => {
            Some("FortiClient writes to /opt, which image-based systems may not allow.")
        }
        _ => None,
    };
    // Only these families have an image-based install command (see
    // `installer`), so only there does a restart finish the job.
    let restart = (platform.image_based
        && matches!(platform.family, DistroFamily::Fedora | DistroFamily::Suse))
    .then_some(RESTART_TO_INSTALL);
    let sentences: Vec<&str> = caveat.into_iter().chain(restart).collect();
    (!sentences.is_empty()).then(|| sentences.join(" "))
}

fn aur_command(helper: Option<&str>) -> String {
    match helper {
        Some(helper) => format!("{} -S {AUR_PACKAGE}", shell_quote(helper)),
        None => format!(
            "git clone https://aur.archlinux.org/{AUR_PACKAGE}.git && cd {AUR_PACKAGE} && makepkg -si"
        ),
    }
}

/// The package format and the package-manager command that installs a
/// local package file.
fn installer(platform: &Platform) -> Option<(PackageFormat, &'static str)> {
    Some(match platform.family {
        DistroFamily::Debian => (PackageFormat::Deb, "apt install"),
        DistroFamily::Fedora if platform.image_based => (PackageFormat::Rpm, "rpm-ostree install"),
        DistroFamily::Fedora => (PackageFormat::Rpm, "dnf install"),
        DistroFamily::Suse if platform.image_based => {
            (PackageFormat::Rpm, "transactional-update pkg install")
        }
        DistroFamily::Suse => (PackageFormat::Rpm, "zypper install"),
        DistroFamily::Arch => (PackageFormat::Arch, "pacman -U"),
        DistroFamily::Unknown => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// Not an absolute path.
    Relative(PackageFormat),
    /// The file does not exist.
    Missing(PackageFormat),
    /// The file is not a package of the expected format.
    WrongFormat(PackageFormat),
    /// A Flatpak document-portal path (`/run/user/<uid>/doc/…`), which the
    /// administrator cannot read on the host.
    SandboxPath,
    /// This distribution has no file-based install command.
    Unsupported,
}

impl InstallError {
    /// User-facing message, e.g. "Choose the downloaded FortiClient .deb
    /// file from this computer."
    pub fn message(&self) -> String {
        match self {
            Self::Relative(format) | Self::Missing(format) | Self::WrongFormat(format) => format!(
                "Choose the downloaded FortiClient {} file from this computer.",
                format.extension()
            ),
            Self::SandboxPath => "Move the installer to your Downloads folder, then choose it \
                                  again."
                .to_owned(),
            Self::Unsupported => "This distribution has no package-file installer for \
                                  FortiClient. Use your distribution’s instructions or your IT \
                                  team’s package."
                .to_owned(),
        }
    }
}

/// The command that installs a downloaded FortiClient package, quoted for
/// POSIX shells, e.g. `sudo apt install '/home/me/Downloads/forticlient.deb'`,
/// `sudo dnf install …`, `sudo zypper install …`, `sudo pacman -U …`,
/// `sudo rpm-ostree install …` (image-based Fedora),
/// `sudo transactional-update pkg install …` (transactional openSUSE), or
/// `su -c '…'`.
pub fn install_command(platform: &Platform, path: &Path) -> Result<String, InstallError> {
    let (format, tool) = installer(platform).ok_or(InstallError::Unsupported)?;
    if !path.is_absolute() {
        return Err(InstallError::Relative(format));
    }
    if is_document_portal_path(path) {
        return Err(InstallError::SandboxPath);
    }
    if !path.is_file() {
        return Err(InstallError::Missing(format));
    }
    // A name that is not UTF-8 cannot be shown as a copyable command.
    let Some(package) = path.to_str() else {
        return Err(InstallError::Missing(format));
    };
    if !format.matches(path) {
        return Err(InstallError::WrongFormat(format));
    }
    Ok(administrator_command(
        platform.elevation,
        &format!("{tool} {}", shell_quote(package)),
    ))
}

/// Files chosen through the Flatpak document portal live in a per-user FUSE
/// mount (`/run/user/<uid>/doc/…` or `/run/flatpak/doc/…`) that other users,
/// including root, cannot read.
fn is_document_portal_path(path: &Path) -> bool {
    let mut parts = path
        .components()
        .skip(1)
        .map(|part| part.as_os_str().to_string_lossy());
    match (
        parts.next().as_deref(),
        parts.next().as_deref(),
        parts.next(),
    ) {
        (Some("run"), Some("user"), Some(_)) => parts.next().as_deref() == Some("doc"),
        (Some("run"), Some("flatpak"), Some(third)) => third == "doc",
        _ => false,
    }
}

/// The command that starts FortiClient's system service now and at every
/// boot, when the system runs systemd (assumed inside a sandbox, where the
/// host cannot be inspected). Never executed by the app.
pub fn service_command(platform: &Platform) -> Option<String> {
    let systemd_runs = Path::new(SYSTEMD_RUNTIME_DIRECTORY).is_dir();
    service_command_for(platform, systemd_runs)
}

/// [`service_command`], given whether this system's `/run/systemd/system`
/// exists (inside a sandbox it is the sandbox's, so it says nothing about
/// the host).
fn service_command_for(platform: &Platform, systemd_runs: bool) -> Option<String> {
    let systemd = platform.sandbox == Sandbox::Flatpak || systemd_runs;
    systemd.then(|| administrator_command(platform.elevation, SERVICE_COMMAND))
}

/// Run `command` as the administrator; `su` takes it as one argument.
fn administrator_command(elevation: Elevation, command: &str) -> String {
    match elevation {
        Elevation::Sudo => format!("sudo {command}"),
        Elevation::Doas => format!("doas {command}"),
        Elevation::Run0 => format!("run0 {command}"),
        Elevation::Su => format!("su -c {}", shell_quote(command)),
    }
}

/// POSIX shell quoting equivalent to Python's `shlex.quote`.
pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    let safe = |byte: u8| byte.is_ascii_alphanumeric() || b"@%+=:,./-_".contains(&byte);
    if value.bytes().all(safe) {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}

/// How to get a tray (StatusNotifierItem host) on this desktop, in words
/// that do not need that term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayHelp {
    /// A few short sentences, e.g. "Add the System Tray widget to your
    /// panel." or "Install the extension with this command, sign out and
    /// back in, then turn on … in the Extensions app."
    pub summary: String,
    /// One copyable line the user runs in a terminal, when one applies.
    /// Only steps that need the administrator are elevated.
    pub command: Option<String>,
    /// A web page with more help, when one applies.
    pub link: Option<&'static str>,
}

pub fn tray_help(platform: &Platform) -> TrayHelp {
    match platform.desktop {
        Desktop::Gnome => gnome_tray_help(platform),
        Desktop::Kde => summary_only("Add the System Tray widget to your panel."),
        Desktop::Cinnamon => summary_only("Add the XApp Status applet to your panel."),
        Desktop::Xfce => summary_only("Add the Status Tray plugin to your panel."),
        Desktop::Budgie => summary_only("Add the System Tray applet to your panel."),
        Desktop::Cosmic => {
            summary_only("Add the Notifications Tray applet to your panel in Settings.")
        }
        Desktop::Lxqt => summary_only("Add the Status Notifier widget to your panel."),
        Desktop::Unity => unity_tray_help(platform),
        Desktop::Mate => summary_only("Add the Notification Area or Indicator Applet to a panel."),
        Desktop::Pantheon => summary_only(
            "The Pantheon panel needs the Ayatana indicators plugin to show tray icons.",
        ),
        // GNOME Flashback runs gnome-panel instead of GNOME Shell.
        Desktop::Other if runs_gnome_flashback(platform) => {
            summary_only("Add the Notification Area applet to your panel.")
        }
        Desktop::Other | Desktop::Unknown => {
            summary_only("Use a panel that shows tray icons, such as Waybar’s tray module.")
        }
    }
}

/// Whether `XDG_CURRENT_DESKTOP` names GNOME Flashback
/// (`GNOME-Flashback:GNOME`, `GNOME-Flashback:Unity`…).
fn runs_gnome_flashback(platform: &Platform) -> bool {
    platform
        .desktop_name
        .split(':')
        .any(|token| token.eq_ignore_ascii_case("GNOME-Flashback"))
}

fn summary_only(summary: &str) -> TrayHelp {
    TrayHelp {
        summary: summary.to_owned(),
        command: None,
        link: None,
    }
}

fn gnome_tray_help(platform: &Platform) -> TrayHelp {
    // Zorin OS and Ubuntu ship an AppIndicator extension of their own, so
    // GNOME Shell has already loaded it and `gnome-extensions enable` can
    // turn it on right away. Zorin OS is Ubuntu-based but replaces
    // Ubuntu's extension.
    if platform.os.is_like("zorin") {
        return shipped_extension("Zorin Appindicators", ZORIN_APPINDICATORS_UUID);
    }
    if platform.ubuntu_based {
        return shipped_extension("Ubuntu AppIndicators", UBUNTU_APPINDICATORS_UUID);
    }
    // The Extensions app is a separate package that GNOME does not install
    // by default on these systems. Debian's package installs the extension
    // under Ubuntu's name.
    let (install, extensions_app, name) = match platform.family {
        DistroFamily::Debian => (
            "apt install",
            "gnome-shell-extension-prefs",
            "“Ubuntu AppIndicators”",
        ),
        DistroFamily::Fedora if platform.image_based => (
            "rpm-ostree install",
            "gnome-extensions-app",
            APPINDICATOR_NAME,
        ),
        DistroFamily::Fedora => ("dnf install", "gnome-extensions-app", APPINDICATOR_NAME),
        DistroFamily::Arch => ("pacman -S", "gnome-extensions-app", APPINDICATOR_NAME),
        DistroFamily::Suse | DistroFamily::Unknown => {
            return TrayHelp {
                summary: format!("Install {APPINDICATOR_NAME} from GNOME Extensions."),
                command: None,
                link: Some(APPINDICATOR_EXTENSION_URL),
            };
        }
    };
    // GNOME Shell finds new extensions only when it starts, and
    // `gnome-extensions enable` refuses one it has not loaded. So the
    // command only installs the extension, and the user turns it on once
    // GNOME Shell has restarted: after signing back in, or after a restart
    // on image-based systems, where new packages take effect only then.
    let reload = if platform.image_based {
        "restart"
    } else {
        "sign out and back in"
    };
    let summary = format!(
        "Install the extension and the Extensions app with this command, {reload}, then turn \
         on {name} in Extensions."
    );
    TrayHelp {
        summary: if platform.enterprise {
            let epel = administrator_command(platform.elevation, "dnf install epel-release");
            format!(
                "The extension comes from EPEL: enable it first (on AlmaLinux or Rocky Linux, {epel}). {summary}"
            )
        } else {
            summary
        },
        command: Some(administrator_command(
            platform.elevation,
            &format!("{install} {APPINDICATOR_PACKAGE} {extensions_app}"),
        )),
        link: None,
    }
}

/// An AppIndicator extension the distribution ships: turned on in the
/// Extensions app, or by a command the user runs (it changes only their own
/// GNOME settings, so it is not elevated).
fn shipped_extension(name: &str, uuid: &str) -> TrayHelp {
    TrayHelp {
        summary: format!("Turn on {name} in the Extensions app, or run this command."),
        command: Some(format!("gnome-extensions enable {uuid}")),
        link: None,
    }
}

/// Unity's top bar shows status icons through indicator-application.
fn unity_tray_help(platform: &Platform) -> TrayHelp {
    TrayHelp {
        summary: "Install indicator-application, then sign out and back in.".to_owned(),
        command: (platform.family == DistroFamily::Debian).then(|| {
            administrator_command(platform.elevation, "apt install indicator-application")
        }),
        link: None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use super::*;

    const UBUNTU: &str = "ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=\"Ubuntu 26.04 LTS\"";
    const DEBIAN: &str = "ID=debian\nPRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"";
    const FEDORA: &str = "ID=fedora\nPRETTY_NAME=\"Fedora Linux 45 (Workstation Edition)\"";
    const ALMALINUX: &str = "ID=\"almalinux\"\nID_LIKE=\"rhel centos fedora\"";
    const TUMBLEWEED: &str = "ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"";
    /// Transactional, so image-based from its os-release alone.
    const AEON: &str = "NAME=\"Aeon\"\nID=\"aeon\"\n\
        ID_LIKE=\"suse opensuse opensuse-tumbleweed opensuse-microos opensuse-aeon microos\"\n\
        PRETTY_NAME=\"Aeon\"";
    const KALPA: &str = "NAME=\"Kalpa Desktop\"\nID=\"kalpa-desktop\"\n\
        ID_LIKE=\"suse opensuse opensuse-tumbleweed opensuse-microos opensuse-kalpa microos\"\n\
        PRETTY_NAME=\"Kalpa Desktop\"";
    const MICROOS: &str = "ID=\"opensuse-microos\"\n\
        ID_LIKE=\"suse opensuse opensuse-tumbleweed microos sl-micro\"";
    const SLES: &str = "ID=\"sles\"\nID_LIKE=\"suse\"";
    const ZORIN_18: &str = r#"PRETTY_NAME="Zorin OS 18"
NAME="Zorin OS"
VERSION_ID="18"
VERSION="18"
VERSION_CODENAME=noble
ID=zorin
ID_LIKE=ubuntu
HOME_URL="https://zorin.com/os/"
SUPPORT_URL="https://help.zorin.com/"
BUG_REPORT_URL="https://zorin.com/os/feedback/"
PRIVACY_POLICY_URL="https://zorin.com/legal/privacy/"
UBUNTU_CODENAME=noble
"#;
    const ARCH: &str = "ID=arch\nPRETTY_NAME=\"Arch Linux\"";
    const NIXOS: &str = "ID=nixos\nPRETTY_NAME=\"NixOS 25.05 (Warbler)\"";

    struct Options {
        desktop: &'static str,
        sandbox: Sandbox,
        image_based: bool,
        architecture: &'static str,
        elevation: Elevation,
        aur_helper: Option<&'static str>,
    }

    const DEFAULTS: Options = Options {
        desktop: "GNOME",
        sandbox: Sandbox::None,
        image_based: false,
        architecture: "x86_64",
        elevation: Elevation::Sudo,
        aur_helper: None,
    };

    fn platform_with(os_release: &str, options: Options) -> Platform {
        let env = |name: &str| (name == "XDG_CURRENT_DESKTOP").then(|| options.desktop.to_owned());
        Platform::from_parts(
            os_release,
            &env,
            options.sandbox,
            options.image_based,
            options.architecture,
            options.elevation,
            options.aur_helper,
        )
    }

    fn platform(os_release: &str) -> Platform {
        platform_with(os_release, DEFAULTS)
    }

    /// Split `command` into words with the system shell, in `directory`, so
    /// any unquoted command substitution would leave a file behind.
    fn shell_words(command: &str, directory: &Path) -> Vec<String> {
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"eval "set -- $1"; printf '%s\0' "$@""#)
            .arg("sh")
            .arg(command)
            .current_dir(directory)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .split_terminator('\0')
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn install_command_quotes_selected_package_without_execution() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("FortiClient ' $(touch unwanted).deb");
        std::fs::write(&path, "").unwrap();
        let command = install_command(&platform(UBUNTU), &path).unwrap();
        assert_eq!(
            shell_words(&command, directory.path()),
            ["sudo", "apt", "install", path.to_str().unwrap()]
        );
        assert!(!directory.path().join("unwanted").exists());
        assert_eq!(
            install_command(&platform(UBUNTU), Path::new("relative.deb")),
            Err(InstallError::Relative(PackageFormat::Deb))
        );
        assert_eq!(
            install_command(&platform(UBUNTU), &directory.path().join("missing.deb")),
            Err(InstallError::Missing(PackageFormat::Deb))
        );
        let other = directory.path().join("installer.sh");
        std::fs::write(&other, "").unwrap();
        assert_eq!(
            install_command(&platform(UBUNTU), &other),
            Err(InstallError::WrongFormat(PackageFormat::Deb))
        );
        assert_eq!(
            install_command(&platform(UBUNTU), directory.path()),
            Err(InstallError::Missing(PackageFormat::Deb))
        );
        for portal in [
            "/run/user/1000/doc/0a1b2c3d/forticlient.deb",
            "/run/flatpak/doc/0a1b2c3d/forticlient.deb",
        ] {
            assert_eq!(
                install_command(&platform(UBUNTU), Path::new(portal)),
                Err(InstallError::SandboxPath)
            );
        }
        assert!(!is_document_portal_path(Path::new("/run/user/1000/bus")));
        assert!(!is_document_portal_path(Path::new(
            "/home/me/doc/forticlient.deb"
        )));
    }

    #[test]
    fn install_commands_per_distribution() {
        let directory = tempfile::tempdir().unwrap();
        let file = |name: &str| {
            let path = directory.path().join(name);
            std::fs::write(&path, "").unwrap();
            path
        };
        let deb = file("forticlient_7.4.3.5411_amd64.DEB");
        let rpm = file("forticlient_7.4.3.5411_x86_64.rpm");
        let pkg = file("forticlient-vpn-7.4.3.5411-1-x86_64.pkg.tar.zst");
        let quoted = |path: &PathBuf| shell_quote(path.to_str().unwrap());
        let silverblue = Options {
            image_based: true,
            ..DEFAULTS
        };
        for (platform, path, expected) in [
            (
                platform(UBUNTU),
                &deb,
                format!("sudo apt install {}", quoted(&deb)),
            ),
            (
                platform(DEBIAN),
                &deb,
                format!("sudo apt install {}", quoted(&deb)),
            ),
            (
                platform(FEDORA),
                &rpm,
                format!("sudo dnf install {}", quoted(&rpm)),
            ),
            (
                platform(ALMALINUX),
                &rpm,
                format!("sudo dnf install {}", quoted(&rpm)),
            ),
            (
                platform_with(FEDORA, silverblue),
                &rpm,
                format!("sudo rpm-ostree install {}", quoted(&rpm)),
            ),
            (
                platform(TUMBLEWEED),
                &rpm,
                format!("sudo zypper install {}", quoted(&rpm)),
            ),
            (
                platform(AEON),
                &rpm,
                format!("sudo transactional-update pkg install {}", quoted(&rpm)),
            ),
            // Detected from the root file system, as on a transactional
            // Tumbleweed server.
            (
                platform_with(
                    TUMBLEWEED,
                    Options {
                        image_based: true,
                        ..DEFAULTS
                    },
                ),
                &rpm,
                format!("sudo transactional-update pkg install {}", quoted(&rpm)),
            ),
            (
                platform(ARCH),
                &pkg,
                format!("sudo pacman -U {}", quoted(&pkg)),
            ),
        ] {
            assert_eq!(install_command(&platform, path), Ok(expected));
        }
        assert_eq!(
            install_command(&platform(AEON), &deb),
            Err(InstallError::WrongFormat(PackageFormat::Rpm))
        );
        assert_eq!(
            install_command(&platform(FEDORA), &deb),
            Err(InstallError::WrongFormat(PackageFormat::Rpm))
        );
        assert_eq!(
            install_command(&platform(ARCH), &rpm),
            Err(InstallError::WrongFormat(PackageFormat::Arch))
        );
        assert_eq!(
            install_command(&platform(NIXOS), &deb),
            Err(InstallError::Unsupported)
        );
    }

    #[test]
    fn elevation_tools() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("it's FortiClient.deb");
        std::fs::write(&path, "").unwrap();
        let command = |elevation| {
            let options = Options {
                elevation,
                ..DEFAULTS
            };
            install_command(&platform_with(DEBIAN, options), &path).unwrap()
        };
        let package = path.to_str().unwrap();
        assert_eq!(
            shell_words(&command(Elevation::Doas), directory.path()),
            ["doas", "apt", "install", package]
        );
        assert_eq!(
            shell_words(&command(Elevation::Run0), directory.path()),
            ["run0", "apt", "install", package]
        );
        let su = shell_words(&command(Elevation::Su), directory.path());
        assert_eq!(su[..2], ["su", "-c"]);
        assert_eq!(su.len(), 3);
        assert_eq!(
            shell_words(&su[2], directory.path()),
            ["apt", "install", package]
        );

        let rpm = directory.path().join("it's FortiClient.rpm");
        std::fs::write(&rpm, "").unwrap();
        let options = Options {
            elevation: Elevation::Su,
            ..DEFAULTS
        };
        let su = shell_words(
            &install_command(&platform_with(AEON, options), &rpm).unwrap(),
            directory.path(),
        );
        assert_eq!(su[..2], ["su", "-c"]);
        assert_eq!(
            shell_words(&su[2], directory.path()),
            [
                "transactional-update",
                "pkg",
                "install",
                rpm.to_str().unwrap()
            ]
        );
    }

    #[test]
    fn service_commands() {
        let native = |elevation| {
            platform_with(
                DEBIAN,
                Options {
                    elevation,
                    ..DEFAULTS
                },
            )
        };
        let flatpak = |elevation| {
            platform_with(
                DEBIAN,
                Options {
                    sandbox: Sandbox::Flatpak,
                    elevation,
                    ..DEFAULTS
                },
            )
        };
        for (elevation, expected) in [
            (
                Elevation::Sudo,
                "sudo systemctl enable --now forticlient.service",
            ),
            (
                Elevation::Doas,
                "doas systemctl enable --now forticlient.service",
            ),
            (
                Elevation::Run0,
                "run0 systemctl enable --now forticlient.service",
            ),
            (
                Elevation::Su,
                "su -c 'systemctl enable --now forticlient.service'",
            ),
        ] {
            assert_eq!(
                service_command_for(&native(elevation), true).as_deref(),
                Some(expected),
                "{elevation:?}"
            );
            // Without systemd there is no service to start.
            assert_eq!(service_command_for(&native(elevation), false), None);
            // The host cannot be inspected from Flatpak, so systemd is
            // assumed whatever the sandbox shows.
            for systemd_runs in [true, false] {
                assert_eq!(
                    service_command_for(&flatpak(elevation), systemd_runs).as_deref(),
                    Some(expected),
                    "{elevation:?}"
                );
            }
        }
        // This system decides natively.
        let systemd_runs = Path::new(SYSTEMD_RUNTIME_DIRECTORY).is_dir();
        assert_eq!(
            service_command(&native(Elevation::Sudo)),
            service_command_for(&native(Elevation::Sudo), systemd_runs)
        );
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            shell_words(
                "su -c 'systemctl enable --now forticlient.service'",
                directory.path()
            ),
            ["su", "-c", "systemctl enable --now forticlient.service"]
        );
    }

    #[test]
    fn shell_quote_matches_python_shlex() {
        for (value, expected) in [
            ("", "''"),
            ("abc", "abc"),
            ("a b", "'a b'"),
            ("it's", r#"'it'"'"'s'"#),
            (
                "/home/me/Downloads/forticlient.deb",
                "/home/me/Downloads/forticlient.deb",
            ),
            ("ü", "'ü'"),
            ("@%+=:,./-_", "@%+=:,./-_"),
            ("$HOME", "'$HOME'"),
            ("a\nb", "'a\nb'"),
            ("'", r#"''"'"''"#),
            ("\"", "'\"'"),
            ("*", "'*'"),
            ("~", "'~'"),
            ("#", "'#'"),
        ] {
            assert_eq!(shell_quote(value), expected, "{value:?}");
        }
    }

    #[test]
    fn package_formats() {
        assert_eq!(PackageFormat::Deb.extension(), ".deb");
        assert_eq!(PackageFormat::Rpm.extension(), ".rpm");
        assert_eq!(PackageFormat::Arch.extension(), ".pkg.tar.zst");
        assert_eq!(PackageFormat::Deb.filter_name(), "Debian packages (.deb)");
        assert_eq!(PackageFormat::Rpm.filter_name(), "RPM packages (.rpm)");
        assert_eq!(
            PackageFormat::Arch.filter_name(),
            "Arch packages (.pkg.tar.zst)"
        );
        assert_eq!(PackageFormat::Deb.patterns(), ["*.deb", "*.DEB"]);
        assert_eq!(PackageFormat::Rpm.patterns(), ["*.rpm", "*.RPM"]);
        assert_eq!(
            PackageFormat::Arch.patterns(),
            ["*.pkg.tar.zst", "*.pkg.tar.xz"]
        );
        for (format, name, matches) in [
            (PackageFormat::Deb, "/tmp/a.deb", true),
            (PackageFormat::Deb, "/tmp/A.Deb", true),
            (PackageFormat::Deb, "/tmp/.deb", false),
            (PackageFormat::Deb, "/tmp/a.deb.part", false),
            (PackageFormat::Deb, "/tmp/ü.deb", true),
            (PackageFormat::Rpm, "/tmp/a.RPM", true),
            (PackageFormat::Rpm, "/tmp/a.deb", false),
            (PackageFormat::Arch, "/tmp/a-1-x86_64.pkg.tar.zst", true),
            (PackageFormat::Arch, "/tmp/a-1-x86_64.PKG.TAR.XZ", true),
            (PackageFormat::Arch, "/tmp/a.tar.zst", false),
            (PackageFormat::Arch, "/", false),
        ] {
            assert_eq!(format.matches(Path::new(name)), matches, "{name}");
        }
    }

    #[test]
    fn error_messages() {
        let python = "Choose the downloaded FortiClient .deb file from this computer.";
        assert_eq!(
            InstallError::WrongFormat(PackageFormat::Deb).message(),
            python
        );
        assert_eq!(
            InstallError::WrongFormat(PackageFormat::Arch).message(),
            "Choose the downloaded FortiClient .pkg.tar.zst file from this computer."
        );
        for error in [
            InstallError::Relative(PackageFormat::Deb),
            InstallError::Missing(PackageFormat::Deb),
        ] {
            assert_eq!(error.message(), python);
        }
        assert_eq!(
            InstallError::Unsupported.message(),
            "This distribution has no package-file installer for FortiClient. Use your \
             distribution’s instructions or your IT team’s package."
        );
        assert_eq!(
            InstallError::SandboxPath.message(),
            "Move the installer to your Downloads folder, then choose it again."
        );
    }

    #[test]
    fn download_guidance_matches_architecture() {
        let description = |os_release: &str, architecture: &'static str| {
            let options = Options {
                architecture,
                ..DEFAULTS
            };
            install_guide(&platform_with(os_release, options)).download_description
        };
        assert_eq!(description(UBUNTU, "x86_64"), "Linux x86-64 (amd64) .deb");
        assert_eq!(description(UBUNTU, "aarch64"), "Linux ARM64 .deb");
        assert_eq!(
            description(UBUNTU, "riscv64"),
            "Linux .deb for your computer"
        );
        assert_eq!(description(FEDORA, "x86_64"), "Linux x86-64 .rpm");
        assert_eq!(description(TUMBLEWEED, "aarch64"), "Linux ARM64 .rpm");
        assert_eq!(
            description(FEDORA, "powerpc64"),
            "Linux .rpm for your computer"
        );
        assert_eq!(
            description(ARCH, "x86_64"),
            "AUR package forticlient-vpn (x86-64)"
        );
        assert_eq!(description(NIXOS, "x86_64"), "Linux x86-64 .deb or .rpm");
    }

    #[test]
    fn install_guides() {
        let ubuntu = install_guide(&platform(UBUNTU));
        assert_eq!(ubuntu.format, Some(PackageFormat::Deb));
        assert_eq!(
            ubuntu.download_hint,
            "Download FortiClient for Linux from Fortinet."
        );
        assert_eq!((ubuntu.repository_command, ubuntu.note), (None, None));

        let fedora = install_guide(&platform(FEDORA));
        assert_eq!(fedora.format, Some(PackageFormat::Rpm));
        assert_eq!(fedora.note, None);
        let silverblue = Options {
            image_based: true,
            ..DEFAULTS
        };
        let silverblue = install_guide(&platform_with(FEDORA, silverblue));
        assert_eq!(
            silverblue.note.as_deref(),
            Some(
                "FortiClient writes to /opt, which image-based systems may not allow. Restart to \
                 finish installing."
            )
        );

        let tumbleweed = install_guide(&platform(TUMBLEWEED));
        assert_eq!(tumbleweed.format, Some(PackageFormat::Rpm));
        assert_eq!(
            tumbleweed.note.as_deref(),
            Some("Fortinet doesn’t officially support openSUSE. Use the RPM package.")
        );
        // Every transactional openSUSE edition, whatever its ID.
        for os_release in [AEON, KALPA, MICROOS] {
            let guide = install_guide(&platform(os_release));
            assert_eq!(guide.format, Some(PackageFormat::Rpm));
            assert_eq!(
                guide.note.as_deref(),
                Some(
                    "Fortinet doesn’t officially support openSUSE. Use the RPM package. Restart \
                     to finish installing."
                ),
                "{os_release}"
            );
        }
        assert_eq!(
            install_guide(&platform(SLES)).note.as_deref(),
            Some("Fortinet doesn’t officially support SUSE Linux. Use the RPM package.")
        );
        let sle_micro = Options {
            image_based: true,
            ..DEFAULTS
        };
        assert!(
            install_guide(&platform_with(SLES, sle_micro))
                .note
                .unwrap()
                .ends_with(" Restart to finish installing.")
        );

        let arch = install_guide(&platform(ARCH));
        assert_eq!(arch.format, Some(PackageFormat::Arch));
        assert_eq!(
            arch.download_hint,
            "FortiClient is available from the AUR as forticlient-vpn."
        );
        assert_eq!(
            arch.repository_command.as_deref(),
            Some(
                "git clone https://aur.archlinux.org/forticlient-vpn.git && cd forticlient-vpn && makepkg -si"
            )
        );
        let helper = Options {
            aur_helper: Some("paru"),
            ..DEFAULTS
        };
        assert_eq!(
            install_guide(&platform_with(ARCH, helper))
                .repository_command
                .as_deref(),
            Some("paru -S forticlient-vpn")
        );

        let nixos = install_guide(&platform(NIXOS));
        assert_eq!(nixos.format, None);
        assert_eq!((nixos.repository_command, nixos.note), (None, None));
        assert!(nixos.download_hint.contains("your IT team’s package"));
    }

    #[test]
    fn gnome_tray_help_per_distribution() {
        let help =
            |os_release: &str, options: Options| tray_help(&platform_with(os_release, options));
        let ubuntu = help(
            UBUNTU,
            Options {
                desktop: "ubuntu:GNOME",
                ..DEFAULTS
            },
        );
        assert_eq!(
            ubuntu.summary,
            "Turn on Ubuntu AppIndicators in the Extensions app, or run this command."
        );
        // Runs as the user: it changes the user's own GNOME settings.
        assert_eq!(
            ubuntu.command.as_deref(),
            Some("gnome-extensions enable ubuntu-appindicators@ubuntu.com")
        );
        assert_eq!(ubuntu.link, None);

        // Zorin OS is Ubuntu-based, but ships its own extension.
        let zorin = help(
            ZORIN_18,
            Options {
                desktop: "zorin:GNOME",
                ..DEFAULTS
            },
        );
        assert_eq!(
            zorin.summary,
            "Turn on Zorin Appindicators in the Extensions app, or run this command."
        );
        assert_eq!(
            zorin.command.as_deref(),
            Some("gnome-extensions enable zorin-appindicator@zorinos.com")
        );
        assert_eq!(zorin.link, None);

        // GNOME Shell cannot turn on an extension installed while it runs,
        // so the command only installs it.
        let summary = "Install the extension and the Extensions app with this command, sign out \
            and back in, then turn on “AppIndicator and KStatusNotifierItem Support” in Extensions.";
        for (os_release, command) in [
            (
                FEDORA,
                "sudo dnf install gnome-shell-extension-appindicator gnome-extensions-app",
            ),
            (
                ARCH,
                "sudo pacman -S gnome-shell-extension-appindicator gnome-extensions-app",
            ),
        ] {
            let help = help(os_release, DEFAULTS);
            assert_eq!(help.summary, summary);
            assert_eq!(help.command.as_deref(), Some(command));
            assert_eq!(help.link, None);
        }

        // Debian's package installs the extension under Ubuntu's name.
        let debian = help(DEBIAN, DEFAULTS);
        assert_eq!(
            debian.summary,
            "Install the extension and the Extensions app with this command, sign out and back \
             in, then turn on “Ubuntu AppIndicators” in Extensions."
        );
        assert_eq!(
            debian.command.as_deref(),
            Some("sudo apt install gnome-shell-extension-appindicator gnome-shell-extension-prefs")
        );

        let almalinux = help(ALMALINUX, DEFAULTS);
        assert_eq!(
            almalinux.summary,
            format!(
                "The extension comes from EPEL: enable it first (on AlmaLinux or Rocky Linux, \
                 sudo dnf install epel-release). {summary}"
            )
        );
        assert_eq!(
            almalinux.command.as_deref(),
            Some("sudo dnf install gnome-shell-extension-appindicator gnome-extensions-app")
        );

        let silverblue = help(
            FEDORA,
            Options {
                image_based: true,
                ..DEFAULTS
            },
        );
        assert_eq!(
            silverblue.summary,
            "Install the extension and the Extensions app with this command, restart, then turn \
             on “AppIndicator and KStatusNotifierItem Support” in Extensions."
        );
        assert_eq!(
            silverblue.command.as_deref(),
            Some("sudo rpm-ostree install gnome-shell-extension-appindicator gnome-extensions-app")
        );

        let su = help(
            DEBIAN,
            Options {
                elevation: Elevation::Su,
                ..DEFAULTS
            },
        );
        assert_eq!(
            su.command.as_deref(),
            Some(
                "su -c 'apt install gnome-shell-extension-appindicator gnome-shell-extension-prefs'"
            )
        );

        for os_release in [TUMBLEWEED, AEON, KALPA, NIXOS] {
            let help = help(os_release, DEFAULTS);
            assert_eq!(
                help.summary,
                "Install “AppIndicator and KStatusNotifierItem Support” from GNOME Extensions."
            );
            assert_eq!(help.command, None);
            assert_eq!(help.link, Some(APPINDICATOR_EXTENSION_URL));
        }
    }

    #[test]
    fn tray_help_for_other_desktops() {
        let summary = |desktop: &'static str| {
            let help = tray_help(&platform_with(
                FEDORA,
                Options {
                    desktop,
                    ..DEFAULTS
                },
            ));
            assert_eq!(help.link, None);
            help.summary
        };
        assert_eq!(summary("KDE"), "Add the System Tray widget to your panel.");
        assert_eq!(
            summary("X-Cinnamon"),
            "Add the XApp Status applet to your panel."
        );
        assert_eq!(summary("XFCE"), "Add the Status Tray plugin to your panel.");
        assert_eq!(
            summary("Budgie:GNOME"),
            "Add the System Tray applet to your panel."
        );
        assert_eq!(
            summary("COSMIC"),
            "Add the Notifications Tray applet to your panel in Settings."
        );
        assert_eq!(
            summary("LXQt"),
            "Add the Status Notifier widget to your panel."
        );
        assert_eq!(
            summary("MATE"),
            "Add the Notification Area or Indicator Applet to a panel."
        );
        assert!(summary("Pantheon").contains("Ayatana indicators plugin"));
        // GNOME Flashback has no GNOME Shell, so no extension advice.
        for flashback in [
            "GNOME-Flashback:GNOME",
            "GNOME-Flashback:Unity",
            "gnome-flashback",
        ] {
            assert_eq!(
                summary(flashback),
                "Add the Notification Area applet to your panel.",
                "{flashback}"
            );
        }
        for other in ["sway", "Hyprland", "GNOME-Classic-Flashback", ""] {
            assert_eq!(
                summary(other),
                "Use a panel that shows tray icons, such as Waybar’s tray module.",
                "{other:?}"
            );
        }
        let unity = tray_help(&platform_with(
            UBUNTU,
            Options {
                desktop: "Unity:Unity7:ubuntu",
                ..DEFAULTS
            },
        ));
        assert_eq!(
            unity.command.as_deref(),
            Some("sudo apt install indicator-application")
        );
    }

    /// Every text the setup guidance can show, on every distribution and
    /// desktop the tests know.
    fn every_guidance_text() -> Vec<String> {
        let mut texts: Vec<String> = [
            InstallError::Relative(PackageFormat::Deb),
            InstallError::Missing(PackageFormat::Rpm),
            InstallError::WrongFormat(PackageFormat::Arch),
            InstallError::SandboxPath,
            InstallError::Unsupported,
        ]
        .iter()
        .map(InstallError::message)
        .collect();
        for os_release in [
            UBUNTU, DEBIAN, FEDORA, ALMALINUX, TUMBLEWEED, AEON, KALPA, MICROOS, SLES, ARCH, NIXOS,
            ZORIN_18,
        ] {
            for desktop in [
                "GNOME",
                "ubuntu:GNOME",
                "zorin:GNOME",
                "GNOME-Flashback:GNOME",
                "KDE",
                "X-Cinnamon",
                "XFCE",
                "MATE",
                "Budgie:GNOME",
                "COSMIC",
                "Pantheon",
                "LXQt",
                "Unity",
                "sway",
                "",
            ] {
                for image_based in [false, true] {
                    let platform = platform_with(
                        os_release,
                        Options {
                            desktop,
                            image_based,
                            ..DEFAULTS
                        },
                    );
                    let guide = install_guide(&platform);
                    texts.extend([guide.download_description, guide.download_hint]);
                    texts.extend(guide.note);
                    texts.push(tray_help(&platform).summary);
                }
            }
        }
        texts
    }

    #[test]
    fn guidance_avoids_jargon_and_wrong_names() {
        for text in every_guidance_text() {
            // Only the extension's own name may contain the protocol's.
            let plain = text.replace(APPINDICATOR_NAME, "");
            assert!(!plain.contains("StatusNotifierItem"), "{text}");
            assert!(!text.contains("layering"), "{text}");
        }
        // openSUSE is never called SUSE Linux.
        for os_release in [TUMBLEWEED, AEON, KALPA, MICROOS] {
            for image_based in [false, true] {
                let options = Options {
                    image_based,
                    ..DEFAULTS
                };
                let note = install_guide(&platform_with(os_release, options)).note;
                assert!(note.is_some_and(|note| !note.contains("SUSE Linux")));
            }
        }
    }

    #[test]
    fn guidance_uses_typographic_apostrophes() {
        for text in every_guidance_text() {
            assert!(!text.contains('\''), "{text}");
        }
    }
}
