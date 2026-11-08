//! Distribution, desktop, session, and sandbox detection.
//!
//! Detection never fails: unknown values map to `Unknown`/`Other` and the
//! app keeps working with generic guidance.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Read;
use std::iter::Peekable;
use std::path::Path;
use std::str::Chars;

const HOST_OS_RELEASE: &str = "/run/host/os-release";
const OS_RELEASE_PATHS: [&str; 2] = ["/etc/os-release", "/usr/lib/os-release"];
/// os-release files are a few hundred bytes; never read an unbounded file.
const OS_RELEASE_LIMIT: u64 = 64 * 1024;
const OSTREE_BOOTED: &str = "/run/ostree-booted";
/// openSUSE's package tool for read-only roots, relative to the root. The
/// `bin` path covers systems that merged `sbin` into `bin`.
const TRANSACTIONAL_UPDATE: [&str; 2] = [
    "usr/sbin/transactional-update",
    "usr/bin/transactional-update",
];
/// Used only when `PATH` is unset.
const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
const AUR_HELPERS: [&str; 2] = ["yay", "paru"];
/// RHEL and its rebuilds recognized by `ID` alone; Oracle Linux lists only
/// `fedora` in `ID_LIKE`.
const ENTERPRISE_IDS: [&str; 5] = ["rhel", "centos", "rocky", "almalinux", "ol"];
/// openSUSE's transactional desktops and server, under the IDs they have
/// used: Aeon (now `aeon`), Kalpa (now `kalpa-desktop`), and MicroOS.
/// Their derivatives list them in `ID_LIKE`. All of them are openSUSE.
const TRANSACTIONAL_SUSE_IDS: [&str; 7] = [
    "aeon",
    "kalpa",
    "kalpa-desktop",
    "microos",
    "opensuse-aeon",
    "opensuse-kalpa",
    "opensuse-microos",
];

/// Parsed `os-release(5)` fields used by the app.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OsRelease {
    /// Lowercase `ID`, e.g. `ubuntu`, `fedora`, `opensuse-tumbleweed`, `arch`.
    pub id: String,
    /// Lowercase `ID_LIKE` tokens, e.g. `["ubuntu", "debian"]`.
    pub id_like: Vec<String>,
    pub name: String,
    /// `PRETTY_NAME`, or `Linux` when missing.
    pub pretty_name: String,
    pub version_id: String,
    /// Lowercase `VARIANT_ID`, e.g. `silverblue`, `kinoite`.
    pub variant_id: String,
}

impl OsRelease {
    /// Parse os-release text (shell-like `KEY=value` lines, optional single
    /// or double quotes, `#` comments, backslash escapes in double quotes).
    /// Missing `ID`, `NAME`, and `PRETTY_NAME` take the os-release(5)
    /// defaults `linux`, `Linux`, and `Linux`.
    pub fn parse(text: &str) -> Self {
        let mut os = Self::default();
        for (key, value) in text.lines().filter_map(assignment) {
            match key {
                "ID" => os.id = value.trim().to_lowercase(),
                "ID_LIKE" => {
                    os.id_like = value.split_whitespace().map(str::to_lowercase).collect();
                }
                "NAME" => os.name = value,
                "PRETTY_NAME" => os.pretty_name = value,
                "VERSION_ID" => os.version_id = value,
                "VARIANT_ID" => os.variant_id = value.trim().to_lowercase(),
                _ => {}
            }
        }
        for (field, default) in [
            (&mut os.id, "linux"),
            (&mut os.name, "Linux"),
            (&mut os.pretty_name, "Linux"),
        ] {
            if field.trim().is_empty() {
                *field = default.to_owned();
            }
        }
        os
    }

    /// Read the host's os-release: `/etc/os-release`, then
    /// `/usr/lib/os-release`; inside Flatpak `/run/host/os-release` first.
    pub fn load(sandbox: Sandbox) -> Self {
        Self::parse(&read_os_release(sandbox))
    }

    /// Whether `ID` or any `ID_LIKE` token equals `id`.
    pub fn is_like(&self, id: &str) -> bool {
        self.ids().any(|known| known == id)
    }

    /// openSUSE or one of its editions (Tumbleweed, Leap, Slowroll, Aeon,
    /// Kalpa, MicroOS…), rather than SUSE Linux Enterprise. Aeon and Kalpa
    /// no longer carry `opensuse` in their `ID`, only in `ID_LIKE`.
    pub fn is_opensuse(&self) -> bool {
        self.ids().any(is_opensuse_id)
    }

    /// `ID`, then the `ID_LIKE` tokens.
    fn ids(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.id.as_str()).chain(self.id_like.iter().map(String::as_str))
    }
}

/// How FortiClient and other packages are installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DistroFamily {
    /// Debian, Ubuntu, Linux Mint, Pop!_OS, elementary OS, Zorin OS, KDE neon… (`apt`, `.deb`)
    Debian,
    /// Fedora, RHEL, CentOS Stream, AlmaLinux, Rocky Linux, Oracle Linux… (`dnf`, `.rpm`)
    Fedora,
    /// openSUSE (Tumbleweed, Leap, Slowroll, and the transactional Aeon,
    /// Kalpa, and MicroOS) and SUSE Linux Enterprise (`zypper` or
    /// `transactional-update`, `.rpm`)
    Suse,
    /// Arch Linux, Manjaro, EndeavourOS, CachyOS, Garuda… (`pacman`, AUR)
    Arch,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Desktop {
    Gnome,
    Kde,
    Cinnamon,
    Xfce,
    Mate,
    Budgie,
    Cosmic,
    Pantheon,
    Lxqt,
    Unity,
    /// A desktop or compositor not listed above (Sway, Hyprland, niri,
    /// GNOME Flashback…).
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionType {
    Wayland,
    X11,
    Unknown,
}

impl SessionType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wayland => "Wayland",
            Self::X11 => "X11",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sandbox {
    None,
    Flatpak,
}

impl Sandbox {
    /// `/.flatpak-info` exists.
    pub fn detect() -> Self {
        if std::path::Path::new("/.flatpak-info").exists() {
            Self::Flatpak
        } else {
            Self::None
        }
    }
}

/// The administrator command shown in copied install commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Elevation {
    Sudo,
    Doas,
    Run0,
    /// `su -c '…'` when no other tool is installed.
    Su,
}

impl Elevation {
    /// The first of `sudo`, `doas`, `run0` found on `PATH`, else `Su`.
    /// Inside a sandbox the host cannot be inspected, so `Sudo` is assumed.
    pub fn detect(sandbox: Sandbox) -> Self {
        match sandbox {
            Sandbox::Flatpak => Self::Sudo,
            Sandbox::None => Self::from_search_path(&search_path()),
        }
    }

    fn from_search_path(path: &OsStr) -> Self {
        [
            ("sudo", Self::Sudo),
            ("doas", Self::Doas),
            ("run0", Self::Run0),
        ]
        .into_iter()
        .find(|(program, _)| is_on_path(program, path))
        .map_or(Self::Su, |(_, elevation)| elevation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub os: OsRelease,
    pub family: DistroFamily,
    /// Ubuntu or a derivative (Mint, Pop!_OS, elementary, Zorin, neon…).
    /// GNOME on Ubuntu ships the Ubuntu AppIndicators extension, but Zorin
    /// OS replaces it with its own Zorin Appindicators (see
    /// [`crate::setup::tray_help`]).
    pub ubuntu_based: bool,
    /// RHEL or a rebuild (AlmaLinux, Rocky, CentOS Stream, Oracle Linux):
    /// some desktop extras come from EPEL.
    pub enterprise: bool,
    /// An image-based system, where installed packages take effect after a
    /// restart: OSTree systems (Fedora Atomic desktops, Bazzite, Bluefin…)
    /// layer them with `rpm-ostree`, and transactional openSUSE (Aeon,
    /// Kalpa, MicroOS) installs them with `transactional-update`.
    pub image_based: bool,
    pub desktop: Desktop,
    /// The sanitized `XDG_CURRENT_DESKTOP` value (matches
    /// `[A-Za-z0-9_:;-]{1,80}`), or `Unknown`.
    pub desktop_name: String,
    pub session: SessionType,
    pub sandbox: Sandbox,
    /// `std::env::consts::ARCH`, e.g. `x86_64`, `aarch64`.
    pub architecture: String,
    pub elevation: Elevation,
    /// An AUR helper found on `PATH` (`yay` or `paru`), Arch family only.
    pub aur_helper: Option<String>,
}

impl Platform {
    /// Inspect the running system.
    pub fn detect() -> Self {
        let sandbox = Sandbox::detect();
        let env = |name: &str| std::env::var(name).ok();
        let mut platform = Self::from_parts(
            &read_os_release(sandbox),
            &env,
            sandbox,
            Path::new(OSTREE_BOOTED).exists(),
            std::env::consts::ARCH,
            Elevation::detect(sandbox),
            None,
        );
        // The host's PATH and root file system are not visible from inside
        // a sandbox.
        if sandbox == Sandbox::None {
            match platform.family {
                DistroFamily::Arch => platform.aur_helper = aur_helper(&search_path()),
                DistroFamily::Suse => {
                    let root = Path::new("/");
                    platform.image_based |=
                        is_transactional_root(has_transactional_update(root), is_read_only(root));
                }
                _ => {}
            }
        }
        platform
    }

    /// Deterministic construction for tests and for [`Platform::detect`].
    /// `env` looks up `XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`,
    /// `WAYLAND_DISPLAY`, and `DISPLAY`. `image_based` is what the file
    /// system shows (`/run/ostree-booted`); systems whose `os_release`
    /// marks them as image-based count too.
    pub fn from_parts(
        os_release: &str,
        env: &dyn Fn(&str) -> Option<String>,
        sandbox: Sandbox,
        image_based: bool,
        architecture: &str,
        elevation: Elevation,
        aur_helper: Option<&str>,
    ) -> Self {
        let os = OsRelease::parse(os_release);
        let family = family_of(&os);
        let current_desktop = env("XDG_CURRENT_DESKTOP").filter(|value| !value.is_empty());
        Self {
            family,
            ubuntu_based: os.is_like("ubuntu"),
            enterprise: is_enterprise(&os, os_release),
            image_based: image_based || is_image_based_release(&os, family, os_release, sandbox),
            desktop: current_desktop
                .as_deref()
                .map_or(Desktop::Unknown, desktop_of),
            desktop_name: current_desktop
                .filter(|value| is_plain_desktop_name(value))
                .unwrap_or_else(|| "Unknown".to_owned()),
            session: session_of(env),
            sandbox,
            architecture: architecture.to_owned(),
            elevation,
            aur_helper: aur_helper
                .filter(|_| family == DistroFamily::Arch)
                .map(str::to_owned),
            os,
        }
    }

    /// What users call the place tray icons live: `top bar` on GNOME and
    /// Unity, `system tray` elsewhere.
    pub fn tray_term(&self) -> &'static str {
        match self.desktop {
            Desktop::Gnome | Desktop::Unity => "top bar",
            _ => "system tray",
        }
    }
}

/// One `KEY=value` line, or `None` for blank, comment, and malformed lines.
fn assignment(line: &str) -> Option<(&str, String)> {
    let line = line.trim_start();
    if line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    let valid_key = !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    valid_key.then(|| (key, shell_value(value)))
}

/// A shell word: quotes removed, backslash escapes applied, and unquoted
/// surrounding whitespace and a trailing ` # comment` dropped.
fn shell_value(raw: &str) -> String {
    let mut value = String::new();
    // The length of `value` without trailing unquoted whitespace.
    let mut end = 0;
    let mut after_blank = false;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => value.extend(chars.by_ref().take_while(|&c| c != '\'')),
            '"' => push_double_quoted(&mut chars, &mut value),
            '\\' => value.extend(chars.next()),
            '#' if after_blank => break,
            c if c.is_whitespace() => {
                after_blank = true;
                if !value.is_empty() {
                    value.push(c);
                }
                continue;
            }
            c => value.push(c),
        }
        after_blank = false;
        end = value.len();
    }
    value.truncate(end);
    value
}

/// Inside double quotes a backslash escapes only `$`, `` ` ``, `"`, and `\`.
fn push_double_quoted(chars: &mut Peekable<Chars<'_>>, value: &mut String) {
    while let Some(c) = chars.next() {
        match c {
            '"' => return,
            '\\' => match chars.next_if(|next| matches!(next, '$' | '`' | '"' | '\\')) {
                Some(escaped) => value.push(escaped),
                None => value.push('\\'),
            },
            c => value.push(c),
        }
    }
}

fn read_os_release(sandbox: Sandbox) -> String {
    let host = (sandbox == Sandbox::Flatpak).then_some(HOST_OS_RELEASE);
    host.into_iter()
        .chain(OS_RELEASE_PATHS)
        .find_map(read_small_file)
        .unwrap_or_default()
}

fn read_small_file(path: &str) -> Option<String> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(OS_RELEASE_LIMIT)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The family of `ID`, else of the first recognized `ID_LIKE` token, which
/// lists the closest relatives first.
fn family_of(os: &OsRelease) -> DistroFamily {
    std::iter::once(&os.id)
        .chain(&os.id_like)
        .find_map(|id| family_of_id(id))
        .unwrap_or(DistroFamily::Unknown)
}

fn family_of_id(id: &str) -> Option<DistroFamily> {
    match id {
        "debian" | "ubuntu" => Some(DistroFamily::Debian),
        "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "ol" => Some(DistroFamily::Fedora),
        "suse" | "sles" | "sled" => Some(DistroFamily::Suse),
        id if is_opensuse_id(id) => Some(DistroFamily::Suse),
        "arch" => Some(DistroFamily::Arch),
        _ => None,
    }
}

/// `opensuse`, an ID that starts with it (`opensuse-tumbleweed`,
/// `opensuse-leap`…), or a transactional edition under its own name.
fn is_opensuse_id(id: &str) -> bool {
    id.starts_with("opensuse") || TRANSACTIONAL_SUSE_IDS.contains(&id)
}

/// RHEL or a rebuild. Fedora remixes such as Nobara also list `rhel` in
/// `ID_LIKE`, so a relative counts only on an Enterprise Linux platform.
fn is_enterprise(os: &OsRelease, text: &str) -> bool {
    let enterprise_platform = os_release_value(text, "PLATFORM_ID")
        .is_some_and(|id| id.trim().to_ascii_lowercase().starts_with("platform:el"));
    ENTERPRISE_IDS.contains(&os.id.as_str())
        || (enterprise_platform && (os.is_like("rhel") || os.is_like("centos")))
}

/// Image-based systems that os-release itself identifies.
fn is_image_based_release(
    os: &OsRelease,
    family: DistroFamily,
    text: &str,
    sandbox: Sandbox,
) -> bool {
    // Flatpak hides `/run/ostree-booted`, but the host's os-release still
    // names its OSTree version.
    let ostree_host = sandbox == Sandbox::Flatpak
        && os_release_value(text, "OSTREE_VERSION")
            .is_some_and(|version| !version.trim().is_empty());
    ostree_host || (family == DistroFamily::Suse && is_transactional_suse(os))
}

fn is_transactional_suse(os: &OsRelease) -> bool {
    os.variant_id == "transactional" || TRANSACTIONAL_SUSE_IDS.iter().any(|id| os.is_like(id))
}

/// The last value assigned to `key`, which wins as it does in a shell.
fn os_release_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .rev()
        .filter_map(assignment)
        .find_map(|(name, value)| (name == key).then_some(value))
}

/// Map colon-separated `XDG_CURRENT_DESKTOP` tokens. Several desktops also
/// list GNOME (`Budgie:GNOME`, `GNOME-Flashback:GNOME`), so they are matched
/// first.
fn desktop_of(value: &str) -> Desktop {
    let tokens: Vec<String> = value
        .split(':')
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect();
    if tokens.is_empty() {
        return Desktop::Unknown;
    }
    if let Some(desktop) = tokens.iter().find_map(|token| specific_desktop(token)) {
        return desktop;
    }
    if tokens.iter().any(|token| token == "gnome") {
        Desktop::Gnome
    } else {
        Desktop::Other
    }
}

fn specific_desktop(token: &str) -> Option<Desktop> {
    Some(match token {
        "kde" => Desktop::Kde,
        "x-cinnamon" | "cinnamon" => Desktop::Cinnamon,
        "xfce" => Desktop::Xfce,
        "mate" => Desktop::Mate,
        "budgie" => Desktop::Budgie,
        "cosmic" => Desktop::Cosmic,
        "pantheon" => Desktop::Pantheon,
        "lxqt" => Desktop::Lxqt,
        "unity" => Desktop::Unity,
        // GNOME Flashback runs gnome-panel, not GNOME Shell, so GNOME Shell
        // extensions do not apply.
        "gnome-flashback" => Desktop::Other,
        _ => return None,
    })
}

/// `^[A-Za-z0-9_:;-]{1,80}$`: a desktop name, not arbitrary environment text.
fn is_plain_desktop_name(value: &str) -> bool {
    (1..=80).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b';' | b'-'))
}

fn session_of(env: &dyn Fn(&str) -> Option<String>) -> SessionType {
    let is_set = |name: &str| env(name).is_some_and(|value| !value.is_empty());
    match env("XDG_SESSION_TYPE")
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("wayland") => SessionType::Wayland,
        Some("x11") => SessionType::X11,
        _ if is_set("WAYLAND_DISPLAY") => SessionType::Wayland,
        _ if is_set("DISPLAY") => SessionType::X11,
        _ => SessionType::Unknown,
    }
}

fn search_path() -> OsString {
    std::env::var_os("PATH").unwrap_or_else(|| DEFAULT_PATH.into())
}

fn aur_helper(path: &OsStr) -> Option<String> {
    AUR_HELPERS
        .into_iter()
        .find(|helper| is_on_path(helper, path))
        .map(str::to_owned)
}

/// Whether a SUSE system is transactional (openSUSE Aeon, Kalpa, MicroOS,
/// or a transactional server whose os-release does not say so):
/// `transactional-update` is installed and the root is read-only. A
/// writable root only has the tool installed.
fn is_transactional_root(has_transactional_update: bool, read_only: bool) -> bool {
    has_transactional_update && read_only
}

fn has_transactional_update(root: &Path) -> bool {
    TRANSACTIONAL_UPDATE
        .iter()
        .any(|tool| root.join(tool).is_file())
}

fn is_read_only(path: &Path) -> bool {
    rustix::fs::statvfs(path)
        .is_ok_and(|stats| stats.f_flag.contains(rustix::fs::StatVfsMountFlags::RDONLY))
}

/// Whether `program` is an executable regular file in one of the absolute
/// directories of `path`.
fn is_on_path(program: &str, path: &OsStr) -> bool {
    std::env::split_paths(path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(program))
        .any(|candidate| {
            candidate.is_file()
                && rustix::fs::access(&candidate, rustix::fs::Access::EXEC_OK).is_ok()
        })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    const UBUNTU_24_04: &str = r#"PRETTY_NAME="Ubuntu 24.04.3 LTS"
NAME="Ubuntu"
VERSION_ID="24.04"
VERSION="24.04.3 LTS (Noble Numbat)"
VERSION_CODENAME=noble
ID=ubuntu
ID_LIKE=debian
HOME_URL="https://www.ubuntu.com/"
SUPPORT_URL="https://help.ubuntu.com/"
BUG_REPORT_URL="https://bugs.launchpad.net/ubuntu/"
PRIVACY_POLICY_URL="https://www.ubuntu.com/legal/terms-and-policies/privacy-policy"
UBUNTU_CODENAME=noble
LOGO=ubuntu-logo
"#;

    const UBUNTU_26_04: &str = r#"PRETTY_NAME="Ubuntu 26.04.1 LTS"
NAME="Ubuntu"
VERSION_ID="26.04"
VERSION="26.04.1 LTS (Resolute Raccoon)"
VERSION_CODENAME=resolute
ID=ubuntu
ID_LIKE=debian
HOME_URL="https://www.ubuntu.com/"
SUPPORT_URL="https://help.ubuntu.com/"
BUG_REPORT_URL="https://bugs.launchpad.net/ubuntu/"
PRIVACY_POLICY_URL="https://www.ubuntu.com/legal/terms-and-policies/privacy-policy"
UBUNTU_CODENAME=resolute
LOGO=ubuntu-logo
"#;

    const DEBIAN_13: &str = r#"PRETTY_NAME="Debian GNU/Linux 13 (trixie)"
NAME="Debian GNU/Linux"
VERSION_ID="13"
VERSION="13 (trixie)"
VERSION_CODENAME=trixie
DEBIAN_VERSION_FULL=13.1
ID=debian
HOME_URL="https://www.debian.org/"
SUPPORT_URL="https://www.debian.org/support"
BUG_REPORT_URL="https://bugs.debian.org/"
"#;

    const LINUX_MINT_22: &str = r#"NAME="Linux Mint"
VERSION="22 (Wilma)"
ID=linuxmint
ID_LIKE="ubuntu debian"
PRETTY_NAME="Linux Mint 22"
VERSION_ID="22"
HOME_URL="https://www.linuxmint.com/"
SUPPORT_URL="https://forums.linuxmint.com/"
BUG_REPORT_URL="http://linuxmint-troubleshooting-guide.readthedocs.io/en/latest/"
PRIVACY_POLICY_URL="https://www.linuxmint.com/"
VERSION_CODENAME=wilma
UBUNTU_CODENAME=noble
"#;

    const POP_OS_24_04: &str = r#"NAME="Pop!_OS"
VERSION="24.04 LTS"
ID=pop
ID_LIKE="ubuntu debian"
PRETTY_NAME="Pop!_OS 24.04 LTS"
VERSION_ID="24.04"
HOME_URL="https://pop.system76.com"
SUPPORT_URL="https://support.system76.com"
BUG_REPORT_URL="https://github.com/pop-os/pop/issues"
PRIVACY_POLICY_URL="https://system76.com/privacy"
VERSION_CODENAME=noble
UBUNTU_CODENAME=noble
LOGO=distributor-logo-pop-os
"#;

    const ELEMENTARY_OS_8: &str = r#"PRETTY_NAME="elementary OS 8 Circe"
NAME="elementary OS"
VERSION_ID="8"
VERSION="8 Circe"
VERSION_CODENAME=circe
ID=elementary
ID_LIKE=ubuntu
HOME_URL="https://elementary.io/"
DOCUMENTATION_URL="https://elementary.io/docs/learning-the-basics"
SUPPORT_URL="https://elementary.io/support"
BUG_REPORT_URL="https://github.com/elementary/os/issues/new"
PRIVACY_POLICY_URL="https://elementary.io/privacy-policy"
UBUNTU_CODENAME=noble
"#;

    const ZORIN_OS_18: &str = r#"PRETTY_NAME="Zorin OS 18"
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

    const KDE_NEON: &str = r#"PRETTY_NAME="KDE neon User Edition"
NAME="KDE neon"
VERSION_ID="24.04"
VERSION="24.04 LTS (Noble Numbat)"
VERSION_CODENAME=noble
ID=neon
ID_LIKE="ubuntu debian"
HOME_URL="https://neon.kde.org/"
SUPPORT_URL="https://neon.kde.org/"
BUG_REPORT_URL="https://bugs.kde.org/"
PRIVACY_POLICY_URL="https://kde.org/privacypolicy/"
UBUNTU_CODENAME=noble
LOGO=start-here-kde-neon
"#;

    const FEDORA_45_WORKSTATION: &str = r#"NAME="Fedora Linux"
VERSION="45 (Workstation Edition)"
RELEASE_TYPE=stable
ID=fedora
VERSION_ID=45
VERSION_CODENAME=""
PLATFORM_ID="platform:f45"
PRETTY_NAME="Fedora Linux 45 (Workstation Edition)"
ANSI_COLOR="0;38;2;60;110;180"
LOGO=fedora-logo-icon
CPE_NAME="cpe:/o:fedoraproject:fedora:45"
DEFAULT_HOSTNAME="fedora"
HOME_URL="https://fedoraproject.org/"
DOCUMENTATION_URL="https://docs.fedoraproject.org/en-US/fedora/f45/system-administrators-guide/"
SUPPORT_URL="https://ask.fedoraproject.org/"
BUG_REPORT_URL="https://bugzilla.redhat.com/"
REDHAT_BUGZILLA_PRODUCT="Fedora"
REDHAT_BUGZILLA_PRODUCT_VERSION=45
REDHAT_SUPPORT_PRODUCT="Fedora"
REDHAT_SUPPORT_PRODUCT_VERSION=45
SUPPORT_END=2027-12-08
VARIANT="Workstation Edition"
VARIANT_ID=workstation
"#;

    const FEDORA_45_SILVERBLUE: &str = r#"NAME="Fedora Linux"
VERSION="45.20261101.0 (Silverblue)"
RELEASE_TYPE=stable
ID=fedora
VERSION_ID=45
VERSION_CODENAME=""
PLATFORM_ID="platform:f45"
PRETTY_NAME="Fedora Linux 45.20261101.0 (Silverblue)"
ANSI_COLOR="0;38;2;60;110;180"
LOGO=fedora-logo-icon
CPE_NAME="cpe:/o:fedoraproject:fedora:45"
DEFAULT_HOSTNAME="fedora"
HOME_URL="https://silverblue.fedoraproject.org"
DOCUMENTATION_URL="https://docs.fedoraproject.org/en-US/fedora-silverblue/"
SUPPORT_URL="https://ask.fedoraproject.org/"
BUG_REPORT_URL="https://github.com/fedora-silverblue/issue-tracker/issues"
REDHAT_BUGZILLA_PRODUCT="Fedora"
REDHAT_BUGZILLA_PRODUCT_VERSION=45
REDHAT_SUPPORT_PRODUCT="Fedora"
REDHAT_SUPPORT_PRODUCT_VERSION=45
SUPPORT_END=2027-12-08
VARIANT="Silverblue"
VARIANT_ID=silverblue
OSTREE_VERSION='45.20261101.0'
"#;

    const BAZZITE: &str = r#"NAME="Bazzite"
VERSION="44.20261028.0 (Kinoite)"
RELEASE_TYPE=stable
ID=bazzite
ID_LIKE="fedora"
VERSION_ID=44
VERSION_CODENAME="Holographic"
PLATFORM_ID="platform:f44"
PRETTY_NAME="Bazzite 44 (FROM Fedora Kinoite)"
ANSI_COLOR="0;38;2;60;110;180"
LOGO=fedora-logo-icon
CPE_NAME="cpe:/o:universal-blue:bazzite:44"
DEFAULT_HOSTNAME="bazzite"
HOME_URL="https://bazzite.gg"
DOCUMENTATION_URL="https://docs.bazzite.gg"
SUPPORT_URL="https://discord.bazzite.gg"
BUG_REPORT_URL="https://github.com/ublue-os/bazzite/issues/"
VARIANT="Kinoite"
VARIANT_ID=bazzite
OSTREE_VERSION='44.20261028.0'
"#;

    const ALMALINUX_10: &str = r#"NAME="AlmaLinux"
VERSION="10.0 (Purple Lion)"
ID="almalinux"
ID_LIKE="rhel centos fedora"
VERSION_ID="10.0"
PLATFORM_ID="platform:el10"
PRETTY_NAME="AlmaLinux 10.0 (Purple Lion)"
ANSI_COLOR="0;34"
LOGO="fedora-logo-icon"
CPE_NAME="cpe:/o:almalinux:almalinux:10::baseos"
HOME_URL="https://almalinux.org/"
DOCUMENTATION_URL="https://wiki.almalinux.org/"
VENDOR_NAME="AlmaLinux"
VENDOR_URL="https://almalinux.org/"
BUG_REPORT_URL="https://bugs.almalinux.org/"

ALMALINUX_MANTISBT_PROJECT="AlmaLinux-10"
ALMALINUX_MANTISBT_PROJECT_VERSION="10.0"
REDHAT_SUPPORT_PRODUCT="AlmaLinux"
REDHAT_SUPPORT_PRODUCT_VERSION="10.0"
SUPPORT_END=2035-06-01
"#;

    const ROCKY_10: &str = r#"NAME="Rocky Linux"
VERSION="10.0 (Red Quartz)"
ID="rocky"
ID_LIKE="rhel centos fedora"
VERSION_ID="10.0"
PLATFORM_ID="platform:el10"
PRETTY_NAME="Rocky Linux 10.0 (Red Quartz)"
ANSI_COLOR="0;32"
LOGO="fedora-logo-icon"
CPE_NAME="cpe:/o:rocky:rocky:10::baseos"
HOME_URL="https://rockylinux.org/"
VENDOR_NAME="RESF"
VENDOR_URL="https://resf.org/"
BUG_REPORT_URL="https://bugs.rockylinux.org/"
SUPPORT_END="2035-05-31"
ROCKY_SUPPORT_PRODUCT="Rocky-Linux-10"
ROCKY_SUPPORT_PRODUCT_VERSION="10.0"
REDHAT_SUPPORT_PRODUCT="Rocky Linux"
REDHAT_SUPPORT_PRODUCT_VERSION="10.0"
"#;

    const CENTOS_STREAM_10: &str = r#"NAME="CentOS Stream"
VERSION="10 (Coughlan)"
ID="centos"
ID_LIKE="rhel fedora"
VERSION_ID="10"
PLATFORM_ID="platform:el10"
PRETTY_NAME="CentOS Stream 10 (Coughlan)"
ANSI_COLOR="0;31"
LOGO="fedora-logo-icon"
CPE_NAME="cpe:/o:centos:centos:10"
HOME_URL="https://centos.org/"
VENDOR_NAME="CentOS"
VENDOR_URL="https://centos.org/"
BUG_REPORT_URL="https://issues.redhat.com/"
REDHAT_SUPPORT_PRODUCT="Red Hat Enterprise Linux 10"
REDHAT_SUPPORT_PRODUCT_VERSION="CentOS Stream"
"#;

    /// A Fedora remix that lists `rhel` in `ID_LIKE`.
    const NOBARA_39: &str = r#"NAME="Nobara Linux"
VERSION="39 (GNOME Edition)"
ID=nobara
ID_LIKE="rhel centos fedora"
VERSION_ID=39
VERSION_CODENAME=""
PLATFORM_ID="platform:f39"
PRETTY_NAME="Nobara Linux 39 (GNOME Edition)"
ANSI_COLOR="0;38;2;60;110;180"
LOGO=nobara-logo-icon
CPE_NAME="cpe:/o:nobaraproject:nobara:39"
DEFAULT_HOSTNAME="nobara"
HOME_URL="https://nobaraproject.org/"
REDHAT_BUGZILLA_PRODUCT="Nobara"
REDHAT_BUGZILLA_PRODUCT_VERSION=39
REDHAT_SUPPORT_PRODUCT="Nobara"
REDHAT_SUPPORT_PRODUCT_VERSION=39
VARIANT="GNOME Edition"
VARIANT_ID=gnome
"#;

    const OPENSUSE_TUMBLEWEED: &str = r#"NAME="openSUSE Tumbleweed"
# VERSION="20261105"
ID="opensuse-tumbleweed"
ID_LIKE="opensuse suse"
VERSION_ID="20261105"
PRETTY_NAME="openSUSE Tumbleweed"
ANSI_COLOR="0;32"
# CPE 2.3 format, boo#1217921
CPE_NAME="cpe:2.3:o:opensuse:tumbleweed:20261105:*:*:*:*:*:*:*"
#CPE 2.2 format
#CPE_NAME="cpe:/o:opensuse:tumbleweed:20261105"
BUG_REPORT_URL="https://bugzilla.opensuse.org"
SUPPORT_URL="https://bugs.opensuse.org"
HOME_URL="https://www.opensuse.org"
DOCUMENTATION_URL="https://en.opensuse.org/Portal:Tumbleweed"
LOGO="distributor-logo-Tumbleweed"
"#;

    const OPENSUSE_LEAP_16: &str = r#"NAME="openSUSE Leap"
VERSION="16.0"
ID="opensuse-leap"
ID_LIKE="suse opensuse"
VERSION_ID="16.0"
PRETTY_NAME="openSUSE Leap 16.0"
ANSI_COLOR="0;32"
CPE_NAME="cpe:/o:opensuse:leap:16.0"
BUG_REPORT_URL="https://bugs.opensuse.org"
HOME_URL="https://www.opensuse.org/"
DOCUMENTATION_URL="https://en.opensuse.org/Portal:Leap"
LOGO="distributor-logo-Leap"
"#;

    /// As the `Aeon-release` package writes it.
    const OPENSUSE_AEON: &str = r#"NAME="Aeon"
# VERSION="20261102"
ID="aeon"
ID_LIKE="suse opensuse opensuse-tumbleweed opensuse-microos opensuse-aeon microos"
VERSION_ID="20261102"
PRETTY_NAME="Aeon"
ANSI_COLOR="0;32"
CPE_NAME="cpe:/o:opensuse:aeon:20261102"
BUG_REPORT_URL="https://aeondesktop.org/reportbug"
SUPPORT_URL="https://aeondesktop.org/bugs"
HOME_URL="https://aeondesktop.org/"
DOCUMENTATION_URL="https://aeondesktop.org/docs"
LOGO="distributor-logo-Aeon"
"#;

    /// As the `Kalpa-release` package writes it.
    const OPENSUSE_KALPA: &str = r#"NAME="Kalpa Desktop"
VERSION="20261102"
ID="kalpa-desktop"
ID_LIKE="suse opensuse opensuse-tumbleweed opensuse-microos opensuse-kalpa microos"
VERSION_ID="20261102"
PRETTY_NAME="Kalpa Desktop"
ANSI_COLOR="0;32"
CPE_NAME="cpe:/o:opensuse:kalpa:20261102"
BUG_REPORT_URL="https://kalpadesktop.org/bugreports"
SUPPORT_URL="https://kalpadesktop.org/forums"
HOME_URL="https://kalpadesktop.org/"
DOCUMENTATION_URL="https://kalpadesktop.org/documentation"
LOGO="distributor-logo-Kalpa"
"#;

    /// As the `MicroOS-release` package writes it.
    const OPENSUSE_MICROOS: &str = r#"NAME="openSUSE MicroOS"
# VERSION="20261102"
ID="opensuse-microos"
ID_LIKE="suse opensuse opensuse-tumbleweed microos sl-micro"
VERSION_ID="20261102"
PRETTY_NAME="openSUSE MicroOS"
SYSEXT_LEVEL="glibc-2.44"
ANSI_COLOR="0;32"
CPE_NAME="cpe:/o:opensuse:microos:20261102"
BUG_REPORT_URL="https://bugzilla.opensuse.org"
SUPPORT_URL="https://bugs.opensuse.org"
HOME_URL="https://www.opensuse.org/"
DOCUMENTATION_URL="https://en.opensuse.org/Portal:MicroOS"
LOGO="distributor-logo-MicroOS"
"#;

    /// Aeon's release candidates, before it dropped the `opensuse-` prefix.
    const OPENSUSE_AEON_RC: &str = r#"NAME="openSUSE Aeon"
ID="opensuse-aeon"
ID_LIKE="suse opensuse opensuse-tumbleweed opensuse-microos"
PRETTY_NAME="openSUSE Aeon"
"#;

    const ARCH: &str = r#"NAME="Arch Linux"
PRETTY_NAME="Arch Linux"
ID=arch
BUILD_ID=rolling
ANSI_COLOR="38;2;23;147;209"
HOME_URL="https://archlinux.org/"
DOCUMENTATION_URL="https://wiki.archlinux.org/"
SUPPORT_URL="https://bbs.archlinux.org/"
BUG_REPORT_URL="https://gitlab.archlinux.org/groups/archlinux/-/issues"
PRIVACY_POLICY_URL="https://terms.archlinux.org/docs/privacy-policy/"
LOGO=archlinux-logo
"#;

    const MANJARO: &str = r#"NAME="Manjaro Linux"
PRETTY_NAME="Manjaro Linux"
ID=manjaro
ID_LIKE=arch
BUILD_ID=rolling
ANSI_COLOR="32;1;24;144;200"
HOME_URL="https://manjaro.org/"
DOCUMENTATION_URL="https://wiki.manjaro.org/"
SUPPORT_URL="https://forum.manjaro.org/"
BUG_REPORT_URL="https://docs.manjaro.org/reporting-bugs/"
PRIVACY_POLICY_URL="https://manjaro.org/privacy-policy/"
LOGO=manjarolinux
"#;

    const ENDEAVOUR_OS: &str = r#"NAME='EndeavourOS'
PRETTY_NAME='EndeavourOS'
ID='endeavouros'
ID_LIKE='arch'
BUILD_ID='2025.03.19'
ANSI_COLOR='38;2;23;147;209'
HOME_URL='https://endeavouros.com'
DOCUMENTATION_URL='https://discovery.endeavouros.com'
SUPPORT_URL='https://forum.endeavouros.com'
BUG_REPORT_URL='https://forum.endeavouros.com/c/general-system/endeavouros-installation'
PRIVACY_POLICY_URL='https://endeavouros.com/privacy-policy-2'
LOGO='endeavouros'
"#;

    const CACHY_OS: &str = r#"NAME="CachyOS Linux"
PRETTY_NAME="CachyOS"
ID=cachyos
ID_LIKE=arch
BUILD_ID=rolling
ANSI_COLOR="38;2;23;147;209"
HOME_URL="https://cachyos.org/"
DOCUMENTATION_URL="https://wiki.cachyos.org/"
SUPPORT_URL="https://discuss.cachyos.org/"
BUG_REPORT_URL="https://github.com/cachyos"
PRIVACY_POLICY_URL="https://terms.archlinux.org/docs/privacy-policy/"
LOGO=cachyos
"#;

    const NIXOS: &str = r#"ANSI_COLOR="0;38;2;126;186;228"
BUG_REPORT_URL="https://github.com/NixOS/nixpkgs/issues"
BUILD_ID="25.05.20251101.0a1b2c3"
CPE_NAME="cpe:/o:nixos:nixos:25.05"
DEFAULT_HOSTNAME=nixos
DOCUMENTATION_URL="https://nixos.org/learn.html"
HOME_URL="https://nixos.org/"
ID=nixos
ID_LIKE=""
IMAGE_ID=""
IMAGE_VERSION=""
LOGO="nix-snowflake"
NAME=NixOS
PRETTY_NAME="NixOS 25.05 (Warbler)"
SUPPORT_END="2025-12-31"
SUPPORT_URL="https://nixos.org/community.html"
VARIANT=""
VARIANT_ID=""
VENDOR_NAME=NixOS
VENDOR_URL="https://nixos.org/"
VERSION="25.05 (Warbler)"
VERSION_CODENAME=warbler
VERSION_ID="25.05"
"#;

    const GENTOO: &str = r#"NAME=Gentoo
ID=gentoo
PRETTY_NAME="Gentoo Linux"
VERSION_ID="2.18"
HOME_URL="https://www.gentoo.org/"
SUPPORT_URL="https://www.gentoo.org/support/"
BUG_REPORT_URL="https://bugs.gentoo.org/"
ANSI_COLOR="1;32"
"#;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn platform(os_release: &str) -> Platform {
        Platform::from_parts(
            os_release,
            &no_env,
            Sandbox::None,
            false,
            "x86_64",
            Elevation::Sudo,
            None,
        )
    }

    fn platform_with_env(vars: &[(&str, &str)]) -> Platform {
        let env = |name: &str| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        };
        Platform::from_parts(
            UBUNTU_26_04,
            &env,
            Sandbox::None,
            false,
            "x86_64",
            Elevation::Sudo,
            None,
        )
    }

    #[test]
    fn distributions_map_to_families() {
        use DistroFamily::{Arch, Debian, Fedora, Suse, Unknown};
        // (os-release, family, ubuntu_based, enterprise, id, pretty name)
        let cases = [
            (
                UBUNTU_24_04,
                Debian,
                true,
                false,
                "ubuntu",
                "Ubuntu 24.04.3 LTS",
            ),
            (
                UBUNTU_26_04,
                Debian,
                true,
                false,
                "ubuntu",
                "Ubuntu 26.04.1 LTS",
            ),
            (
                DEBIAN_13,
                Debian,
                false,
                false,
                "debian",
                "Debian GNU/Linux 13 (trixie)",
            ),
            (
                LINUX_MINT_22,
                Debian,
                true,
                false,
                "linuxmint",
                "Linux Mint 22",
            ),
            (
                POP_OS_24_04,
                Debian,
                true,
                false,
                "pop",
                "Pop!_OS 24.04 LTS",
            ),
            (
                ELEMENTARY_OS_8,
                Debian,
                true,
                false,
                "elementary",
                "elementary OS 8 Circe",
            ),
            (ZORIN_OS_18, Debian, true, false, "zorin", "Zorin OS 18"),
            (
                KDE_NEON,
                Debian,
                true,
                false,
                "neon",
                "KDE neon User Edition",
            ),
            (
                FEDORA_45_WORKSTATION,
                Fedora,
                false,
                false,
                "fedora",
                "Fedora Linux 45 (Workstation Edition)",
            ),
            (
                FEDORA_45_SILVERBLUE,
                Fedora,
                false,
                false,
                "fedora",
                "Fedora Linux 45.20261101.0 (Silverblue)",
            ),
            (
                BAZZITE,
                Fedora,
                false,
                false,
                "bazzite",
                "Bazzite 44 (FROM Fedora Kinoite)",
            ),
            (
                ALMALINUX_10,
                Fedora,
                false,
                true,
                "almalinux",
                "AlmaLinux 10.0 (Purple Lion)",
            ),
            (
                ROCKY_10,
                Fedora,
                false,
                true,
                "rocky",
                "Rocky Linux 10.0 (Red Quartz)",
            ),
            (
                CENTOS_STREAM_10,
                Fedora,
                false,
                true,
                "centos",
                "CentOS Stream 10 (Coughlan)",
            ),
            (
                NOBARA_39,
                Fedora,
                false,
                false,
                "nobara",
                "Nobara Linux 39 (GNOME Edition)",
            ),
            (
                OPENSUSE_TUMBLEWEED,
                Suse,
                false,
                false,
                "opensuse-tumbleweed",
                "openSUSE Tumbleweed",
            ),
            (
                OPENSUSE_LEAP_16,
                Suse,
                false,
                false,
                "opensuse-leap",
                "openSUSE Leap 16.0",
            ),
            (OPENSUSE_AEON, Suse, false, false, "aeon", "Aeon"),
            (
                OPENSUSE_AEON_RC,
                Suse,
                false,
                false,
                "opensuse-aeon",
                "openSUSE Aeon",
            ),
            (
                OPENSUSE_KALPA,
                Suse,
                false,
                false,
                "kalpa-desktop",
                "Kalpa Desktop",
            ),
            (
                OPENSUSE_MICROOS,
                Suse,
                false,
                false,
                "opensuse-microos",
                "openSUSE MicroOS",
            ),
            (ARCH, Arch, false, false, "arch", "Arch Linux"),
            (MANJARO, Arch, false, false, "manjaro", "Manjaro Linux"),
            (
                ENDEAVOUR_OS,
                Arch,
                false,
                false,
                "endeavouros",
                "EndeavourOS",
            ),
            (CACHY_OS, Arch, false, false, "cachyos", "CachyOS"),
            (
                NIXOS,
                Unknown,
                false,
                false,
                "nixos",
                "NixOS 25.05 (Warbler)",
            ),
            (GENTOO, Unknown, false, false, "gentoo", "Gentoo Linux"),
        ];
        for (text, family, ubuntu_based, enterprise, id, pretty_name) in cases {
            let platform = platform(text);
            assert_eq!(platform.family, family, "{pretty_name}");
            assert_eq!(platform.ubuntu_based, ubuntu_based, "{pretty_name}");
            assert_eq!(platform.enterprise, enterprise, "{pretty_name}");
            assert_eq!(platform.os.id, id);
            assert_eq!(platform.os.pretty_name, pretty_name);
        }
    }

    #[test]
    fn other_rhel_and_suse_identifiers() {
        let family = |text: &str| platform(text).family;
        assert_eq!(
            family("ID=\"rhel\"\nID_LIKE=\"fedora\""),
            DistroFamily::Fedora
        );
        assert!(platform("ID=\"rhel\"\nID_LIKE=\"fedora\"").enterprise);
        assert!(platform("ID=\"ol\"\nID_LIKE=\"fedora\"").enterprise);
        assert_eq!(family("ID=\"sles\"\nID_LIKE=\"suse\""), DistroFamily::Suse);
        assert_eq!(family("ID=sled"), DistroFamily::Suse);
        assert_eq!(family("ID=opensuse-slowroll"), DistroFamily::Suse);
        assert_eq!(
            family("ID=\"mageia\"\nID_LIKE=\"mandriva fedora\""),
            DistroFamily::Fedora
        );
        assert_eq!(family("ID=debian\nID_LIKE=arch"), DistroFamily::Debian);
        assert_eq!(family(""), DistroFamily::Unknown);
    }

    #[test]
    fn enterprise_relatives_need_an_enterprise_platform() {
        let enterprise = |text: &str| platform(text).enterprise;
        // Explicit IDs count without a PLATFORM_ID.
        for id in ["rhel", "centos", "rocky", "almalinux", "ol"] {
            assert!(enterprise(&format!("ID={id}")), "{id}");
        }
        assert!(enterprise(
            "ID=eurolinux\nID_LIKE=\"rhel fedora centos\"\nPLATFORM_ID=\"platform:el9\""
        ));
        assert!(enterprise(
            "ID=example\nID_LIKE=centos\nPLATFORM_ID=Platform:EL10"
        ));
        assert!(!enterprise(NOBARA_39));
        assert!(!enterprise("ID=example\nID_LIKE=\"rhel fedora\""));
        assert!(!enterprise(
            "ID=example\nID_LIKE=fedora\nPLATFORM_ID=\"platform:el10\""
        ));
        // The last assignment wins, as in a shell.
        assert!(!enterprise(
            "ID=example\nID_LIKE=rhel\nPLATFORM_ID=platform:el10\nPLATFORM_ID=platform:f43"
        ));
    }

    #[test]
    fn image_based_systems() {
        // (os-release, sandbox, `/run/ostree-booted` exists, image-based)
        for (text, sandbox, ostree_booted, image_based) in [
            (FEDORA_45_SILVERBLUE, Sandbox::None, true, true),
            // Natively, `/run/ostree-booted` decides.
            (FEDORA_45_SILVERBLUE, Sandbox::None, false, false),
            // Flatpak hides it; the host's os-release decides.
            (FEDORA_45_SILVERBLUE, Sandbox::Flatpak, false, true),
            (BAZZITE, Sandbox::Flatpak, false, true),
            (FEDORA_45_WORKSTATION, Sandbox::Flatpak, false, false),
            (
                "ID=fedora\nOSTREE_VERSION=''",
                Sandbox::Flatpak,
                false,
                false,
            ),
            (
                "ID=fedora\n# OSTREE_VERSION=45",
                Sandbox::Flatpak,
                false,
                false,
            ),
            (OPENSUSE_AEON, Sandbox::None, false, true),
            (OPENSUSE_AEON_RC, Sandbox::None, false, true),
            (OPENSUSE_KALPA, Sandbox::Flatpak, false, true),
            (OPENSUSE_KALPA, Sandbox::None, false, true),
            (OPENSUSE_MICROOS, Sandbox::None, false, true),
            (OPENSUSE_TUMBLEWEED, Sandbox::None, false, false),
            (OPENSUSE_LEAP_16, Sandbox::Flatpak, false, false),
            (
                "ID=opensuse-tumbleweed\nVARIANT_ID=transactional",
                Sandbox::None,
                false,
                true,
            ),
            (
                "ID=example\nID_LIKE=\"opensuse-microos suse\"",
                Sandbox::None,
                false,
                true,
            ),
            (
                "ID=example\nID_LIKE=\"microos suse\"",
                Sandbox::None,
                false,
                true,
            ),
            // Only SUSE systems are transactional.
            (
                "ID=fedora\nVARIANT_ID=transactional",
                Sandbox::None,
                false,
                false,
            ),
        ] {
            let platform = Platform::from_parts(
                text,
                &no_env,
                sandbox,
                ostree_booted,
                "x86_64",
                Elevation::Sudo,
                None,
            );
            assert_eq!(platform.image_based, image_based, "{text:?} {sandbox:?}");
        }
    }

    #[test]
    fn transactional_roots_need_the_tool_and_a_read_only_root() {
        assert!(is_transactional_root(true, true));
        // A writable root is a regular system with the tool installed.
        assert!(!is_transactional_root(true, false));
        assert!(!is_transactional_root(false, true));
        assert!(!is_transactional_root(false, false));
    }

    #[test]
    fn root_facts_come_from_the_file_system() {
        let root = tempfile::tempdir().unwrap();
        assert!(!has_transactional_update(root.path()));
        std::fs::create_dir_all(root.path().join("usr/sbin/transactional-update")).unwrap();
        assert!(!has_transactional_update(root.path()), "a directory");
        std::fs::create_dir_all(root.path().join("usr/bin")).unwrap();
        std::fs::write(root.path().join("usr/bin/transactional-update"), "").unwrap();
        assert!(has_transactional_update(root.path()));
        assert!(!is_read_only(root.path()));
        assert!(!is_read_only(&root.path().join("missing")));
    }

    #[test]
    fn opensuse_is_told_from_suse_linux_enterprise() {
        for text in [
            OPENSUSE_TUMBLEWEED,
            OPENSUSE_LEAP_16,
            OPENSUSE_AEON,
            OPENSUSE_AEON_RC,
            OPENSUSE_KALPA,
            OPENSUSE_MICROOS,
            "ID=opensuse-slowroll",
            "ID=aeon",
            "ID=kalpa",
            "ID=example\nID_LIKE=opensuse-tumbleweed",
        ] {
            assert!(OsRelease::parse(text).is_opensuse(), "{text:?}");
            assert_eq!(platform(text).family, DistroFamily::Suse, "{text:?}");
        }
        for text in [
            "ID=\"sles\"\nID_LIKE=\"suse\"",
            "ID=sled",
            "ID=\"sl-micro\"\nID_LIKE=\"suse\"",
            FEDORA_45_WORKSTATION,
            ARCH,
            "",
        ] {
            assert!(!OsRelease::parse(text).is_opensuse(), "{text:?}");
        }
    }

    #[test]
    fn os_release_fields() {
        let silverblue = OsRelease::parse(FEDORA_45_SILVERBLUE);
        assert_eq!(silverblue.variant_id, "silverblue");
        assert_eq!(silverblue.version_id, "45");
        assert_eq!(silverblue.name, "Fedora Linux");
        let tumbleweed = OsRelease::parse(OPENSUSE_TUMBLEWEED);
        assert_eq!(tumbleweed.id_like, ["opensuse", "suse"]);
        assert_eq!(tumbleweed.version_id, "20261105");
        assert!(OsRelease::parse(NIXOS).id_like.is_empty());
        assert_eq!(OsRelease::parse(ENDEAVOUR_OS).name, "EndeavourOS");
        assert!(OsRelease::parse(ELEMENTARY_OS_8).is_like("ubuntu"));
        assert!(!OsRelease::parse(ELEMENTARY_OS_8).is_like("debian"));
        assert!(OsRelease::parse(ZORIN_OS_18).is_like("zorin"));
        let aeon = OsRelease::parse(OPENSUSE_AEON);
        assert!(aeon.is_like("opensuse-microos") && aeon.is_like("microos"));
    }

    #[test]
    fn quoting_escapes_and_comments() {
        let os = OsRelease::parse(concat!(
            "# A comment\n",
            "   # An indented comment\n",
            "NAME=\"Example \\\"Quoted\\\" \\$HOME \\\\ \\` \\n OS\"\n",
            "ID=Example\n",
            "ID_LIKE=\"Fedora  RHEL\"\t\n",
            "PRETTY_NAME='Example OS 1.0 (it'\\''s \"fine\")'\n",
            "VERSION_ID=1.0 # trailing comment\n",
            "VARIANT_ID=\"Atomic \" # quoted space kept\n",
            "BROKEN LINE\n",
            "=no key\n",
            "BAD KEY=value\n",
        ));
        assert_eq!(os.name, "Example \"Quoted\" $HOME \\ ` \\n OS");
        assert_eq!(os.id, "example");
        assert_eq!(os.id_like, ["fedora", "rhel"]);
        assert_eq!(os.pretty_name, "Example OS 1.0 (it's \"fine\")");
        assert_eq!(os.version_id, "1.0");
        assert_eq!(os.variant_id, "atomic");
        assert_eq!(shell_value("\"Atomic \""), "Atomic ");
        assert_eq!(shell_value("a#b"), "a#b");
        assert_eq!(shell_value(" #x"), "");
        assert_eq!(shell_value("plain\\ word"), "plain word");
        assert_eq!(shell_value("'unterminated"), "unterminated");
        assert_eq!(shell_value("\"unterminated \\"), "unterminated \\");
    }

    #[test]
    fn missing_fields_take_defaults() {
        let os = OsRelease::parse("VERSION_ID=1\nPRETTY_NAME=\"\"\n");
        assert_eq!(os.id, "linux");
        assert_eq!(os.name, "Linux");
        assert_eq!(os.pretty_name, "Linux");
        assert_eq!(OsRelease::parse("").pretty_name, "Linux");
    }

    #[test]
    fn desktops() {
        use Desktop::*;
        for (value, desktop) in [
            ("ubuntu:GNOME", Gnome),
            ("GNOME", Gnome),
            ("pop:GNOME", Gnome),
            ("GNOME-Classic:GNOME", Gnome),
            ("GNOME-Flashback:GNOME", Other),
            ("GNOME-Flashback:Unity", Other),
            ("zorin:GNOME", Gnome),
            ("Budgie:GNOME", Budgie),
            ("X-Cinnamon", Cinnamon),
            ("KDE", Kde),
            ("XFCE", Xfce),
            ("MATE", Mate),
            ("COSMIC", Cosmic),
            ("Pantheon", Pantheon),
            ("LXQt", Lxqt),
            ("Unity", Unity),
            ("Unity:Unity7:ubuntu", Unity),
            ("Hyprland", Other),
            ("sway", Other),
            ("niri", Other),
            ("Deepin", Other),
            (":", Unknown),
            ("", Unknown),
        ] {
            let platform = platform_with_env(&[("XDG_CURRENT_DESKTOP", value)]);
            assert_eq!(platform.desktop, desktop, "{value:?}");
        }
        assert_eq!(platform_with_env(&[]).desktop, Unknown);
    }

    #[test]
    fn desktop_name_is_sanitized() {
        let name = |value: &str| platform_with_env(&[("XDG_CURRENT_DESKTOP", value)]).desktop_name;
        assert_eq!(name("ubuntu:GNOME"), "ubuntu:GNOME");
        assert_eq!(name("X-Cinnamon"), "X-Cinnamon");
        assert_eq!(name("GNOME;KDE_x"), "GNOME;KDE_x");
        assert_eq!(name(&"A".repeat(80)), "A".repeat(80));
        assert_eq!(name(&"A".repeat(81)), "Unknown");
        for unsafe_value in [
            "GNOME KDE",
            "PASSWORD=secret",
            "GNOMÉ",
            "a/b",
            "line\nbreak",
        ] {
            assert_eq!(name(unsafe_value), "Unknown", "{unsafe_value:?}");
        }
        assert_eq!(platform_with_env(&[]).desktop_name, "Unknown");
    }

    #[test]
    fn sessions() {
        use SessionType::*;
        for (vars, session) in [
            (&[("XDG_SESSION_TYPE", "wayland")][..], Wayland),
            (
                &[
                    ("XDG_SESSION_TYPE", "x11"),
                    ("WAYLAND_DISPLAY", "wayland-0"),
                ],
                X11,
            ),
            (
                &[
                    ("XDG_SESSION_TYPE", "tty"),
                    ("WAYLAND_DISPLAY", "wayland-0"),
                ],
                Wayland,
            ),
            (
                &[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")],
                Wayland,
            ),
            (&[("WAYLAND_DISPLAY", ""), ("DISPLAY", ":0")], X11),
            (&[("XDG_SESSION_TYPE", "tty")], Unknown),
            (&[], Unknown),
        ] {
            assert_eq!(platform_with_env(vars).session, session, "{vars:?}");
        }
        assert_eq!(Wayland.as_str(), "Wayland");
    }

    #[test]
    fn parts_are_kept() {
        let platform = Platform::from_parts(
            FEDORA_45_SILVERBLUE,
            &no_env,
            Sandbox::Flatpak,
            true,
            "aarch64",
            Elevation::Run0,
            Some("yay"),
        );
        assert!(platform.image_based);
        assert_eq!(platform.sandbox, Sandbox::Flatpak);
        assert_eq!(platform.architecture, "aarch64");
        assert_eq!(platform.elevation, Elevation::Run0);
        // AUR helpers only apply to the Arch family.
        assert_eq!(platform.aur_helper, None);
        let arch = Platform::from_parts(
            ARCH,
            &no_env,
            Sandbox::None,
            false,
            "x86_64",
            Elevation::Sudo,
            Some("paru"),
        );
        assert_eq!(arch.aur_helper.as_deref(), Some("paru"));
    }

    #[test]
    fn tray_terms() {
        let term = |value: &str| platform_with_env(&[("XDG_CURRENT_DESKTOP", value)]).tray_term();
        assert_eq!(term("ubuntu:GNOME"), "top bar");
        assert_eq!(term("Unity"), "top bar");
        assert_eq!(term("KDE"), "system tray");
        assert_eq!(term("sway"), "system tray");
        assert_eq!(term("GNOME-Flashback:GNOME"), "system tray");
    }

    #[test]
    fn path_lookup_needs_an_executable_file() {
        let directory = tempfile::tempdir().unwrap();
        let file = |name: &str, mode: u32| {
            let path = directory.path().join(name);
            std::fs::write(&path, "").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        file("doas", 0o644);
        file("run0", 0o755);
        std::fs::create_dir(directory.path().join("sudo")).unwrap();
        let path = std::env::join_paths(["relative", directory.path().to_str().unwrap()]).unwrap();
        assert_eq!(Elevation::from_search_path(&path), Elevation::Run0);
        file("doas", 0o755);
        assert_eq!(Elevation::from_search_path(&path), Elevation::Doas);
        assert_eq!(Elevation::from_search_path(OsStr::new("")), Elevation::Su);
        assert_eq!(Elevation::detect(Sandbox::Flatpak), Elevation::Sudo);
        assert!(is_on_path("doas", &path));
        assert!(!is_on_path("yay", &path));
    }

    #[test]
    fn detection_reads_this_system() {
        let platform = Platform::detect();
        assert!(!platform.os.pretty_name.is_empty());
        assert_eq!(platform.architecture, std::env::consts::ARCH);
        assert!(is_plain_desktop_name(&platform.desktop_name));
        assert_eq!(OsRelease::load(platform.sandbox), platform.os);
    }
}
