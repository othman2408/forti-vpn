//! Validated, non-secret application preferences.
//!
//! The file format is the Python version's JSON object, so upgrades keep
//! settings: `{"last_profile": str, "close_behavior": "hide"|"quit",
//! "appearance": "system"|"light"|"dark", "remember_password": bool}`.
//! Unknown keys and invalid values are ignored one by one; a missing or
//! malformed file yields defaults. Writes are atomic (temporary file +
//! rename), the file is `0600`, and its directory is created `0700`.
//! Passwords are never stored.
use std::ffi::OsString;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CloseBehavior {
    /// Keep running in the tray (only when a tray is available).
    #[default]
    Hide,
    /// Disconnect the VPN and quit.
    Quit,
}

impl CloseBehavior {
    fn as_str(self) -> &'static str {
        match self {
            Self::Hide => "hide",
            Self::Quit => "quit",
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        match value.as_str()? {
            "hide" => Some(Self::Hide),
            "quit" => Some(Self::Quit),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        match value.as_str()? {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PreferenceValues {
    pub last_profile: String,
    pub close_behavior: CloseBehavior,
    pub appearance: Appearance,
    pub remember_password: bool,
}

impl PreferenceValues {
    /// Take each valid known key from a JSON object and ignore the rest.
    fn from_json(text: &str) -> Self {
        let mut values = Self::default();
        let Ok(Value::Object(object)) = serde_json::from_str(text) else {
            return values;
        };
        if let Some(name) = object.get("last_profile").and_then(Value::as_str) {
            values.last_profile = name.to_owned();
        }
        if let Some(behavior) = object
            .get("close_behavior")
            .and_then(CloseBehavior::from_json)
        {
            values.close_behavior = behavior;
        }
        if let Some(appearance) = object.get("appearance").and_then(Appearance::from_json) {
            values.appearance = appearance;
        }
        if let Some(remember) = object.get("remember_password").and_then(Value::as_bool) {
            values.remember_password = remember;
        }
        values
    }

    /// The same keys, order, and separators as Python's `json.dump`.
    fn to_json(&self) -> String {
        format!(
            r#"{{"last_profile": {}, "close_behavior": "{}", "appearance": "{}", "remember_password": {}}}"#,
            Value::from(self.last_profile.as_str()),
            self.close_behavior.as_str(),
            self.appearance.as_str(),
            self.remember_password,
        )
    }
}

#[derive(Debug, Clone)]
pub struct Preferences {
    path: PathBuf,
    values: PreferenceValues,
}

impl Preferences {
    /// `$XDG_CONFIG_HOME/forti-vpn/preferences.json`, falling back to
    /// `~/.config/forti-vpn/preferences.json`.
    pub fn default_path() -> PathBuf {
        config_home(std::env::var_os("XDG_CONFIG_HOME"), std::env::home_dir())
            .join("forti-vpn")
            .join("preferences.json")
    }

    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let values = fs::read_to_string(&path)
            .map(|text| PreferenceValues::from_json(&text))
            .unwrap_or_default();
        Self { path, values }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn values(&self) -> &PreferenceValues {
        &self.values
    }

    /// Each setter persists all values; on error nothing changes in memory.
    pub fn set_close_behavior(&mut self, value: CloseBehavior) -> io::Result<()> {
        self.update(|values| values.close_behavior = value)
    }

    pub fn set_appearance(&mut self, value: Appearance) -> io::Result<()> {
        self.update(|values| values.appearance = value)
    }

    pub fn set_remember_password(&mut self, value: bool) -> io::Result<()> {
        self.update(|values| values.remember_password = value)
    }

    pub fn select_profile(&mut self, name: &str) -> io::Result<()> {
        self.update(|values| values.last_profile = name.to_owned())
    }

    fn update(&mut self, change: impl FnOnce(&mut PreferenceValues)) -> io::Result<()> {
        let mut values = self.values.clone();
        change(&mut values);
        write_atomically(&self.path, values.to_json().as_bytes())?;
        self.values = values;
        Ok(())
    }
}

/// The XDG base directory specification ignores an empty or relative
/// `XDG_CONFIG_HOME`.
fn config_home(xdg_config_home: Option<OsString>, home: Option<PathBuf>) -> PathBuf {
    xdg_config_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.unwrap_or_default().join(".config"))
}

/// Replace `path` with `contents` so readers see the old or the new file,
/// never a partial one.
fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let Some(name) = path.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the preferences path has no file name",
        ));
    };
    if let Some(directory) = path.parent() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
    }
    let mut temporary_name = name.to_os_string();
    temporary_name.push(".tmp");
    let temporary = path.with_file_name(temporary_name);
    // A file left by an interrupted write may have other permissions.
    match fs::remove_file(&temporary) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let written =
        write_new_private_file(&temporary, contents).and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

fn write_new_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn migration_and_settings_preserved_on_selection() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("prefs.json");
        fs::write(
            &path,
            r#"{"last_profile": "Old", "password": "secret", "appearance": "invalid"}"#,
        )
        .unwrap();
        let mut preferences = Preferences::load(&path);
        assert_eq!(preferences.values().appearance, Appearance::System);
        assert_eq!(preferences.values().last_profile, "Old");
        preferences.set_close_behavior(CloseBehavior::Quit).unwrap();
        preferences.set_appearance(Appearance::Dark).unwrap();
        preferences.set_remember_password(true).unwrap();
        preferences.select_profile("New").unwrap();
        let loaded = Preferences::load(&path);
        assert_eq!(
            loaded.values(),
            &PreferenceValues {
                last_profile: "New".into(),
                close_behavior: CloseBehavior::Quit,
                appearance: Appearance::Dark,
                remember_password: true,
            }
        );
        assert!(!fs::read_to_string(&path).unwrap().contains("secret"));
    }

    #[test]
    fn only_profile_persisted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let mut preferences = Preferences::load(&path);
        preferences.select_profile("Office VPN").unwrap();
        let saved: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["last_profile"], "Office VPN");
        assert!(saved.get("password").is_none());
        assert_eq!(Preferences::load(&path).values().last_profile, "Office VPN");
        assert_eq!(mode(&path), 0o600);
        fs::write(&path, "bad json").unwrap();
        assert_eq!(Preferences::load(&path).values().last_profile, "");
    }

    #[test]
    fn file_matches_the_python_format() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let mut preferences = Preferences::load(&path);
        preferences.select_profile("Office VPN").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            r#"{"last_profile": "Office VPN", "close_behavior": "hide", "appearance": "system", "remember_password": false}"#
        );
    }

    #[test]
    fn python_files_load() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        fs::write(
            &path,
            r#"{"last_profile": "Büro 😀", "close_behavior": "quit", "appearance": "light", "remember_password": true}"#,
        )
        .unwrap();
        assert_eq!(
            Preferences::load(&path).values(),
            &PreferenceValues {
                last_profile: "Büro 😀".into(),
                close_behavior: CloseBehavior::Quit,
                appearance: Appearance::Light,
                remember_password: true,
            }
        );
    }

    #[test]
    fn invalid_values_are_ignored_one_by_one() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        fs::write(
            &path,
            r#"{"last_profile": 7, "close_behavior": "quit", "appearance": "DARK", "remember_password": 1, "extra": true}"#,
        )
        .unwrap();
        assert_eq!(
            Preferences::load(&path).values(),
            &PreferenceValues {
                close_behavior: CloseBehavior::Quit,
                ..PreferenceValues::default()
            }
        );
        for text in ["[\"quit\"]", "\"quit\"", "null", "", "\u{FEFF}{}"] {
            fs::write(&path, text).unwrap();
            assert_eq!(
                Preferences::load(&path).values(),
                &PreferenceValues::default()
            );
        }
        fs::write(&path, [b'{', 0xFF, b'}']).unwrap();
        assert_eq!(
            Preferences::load(&path).values(),
            &PreferenceValues::default()
        );
        let missing = Preferences::load(directory.path().join("missing.json"));
        assert_eq!(missing.values(), &PreferenceValues::default());
        assert_eq!(missing.path(), directory.path().join("missing.json"));
    }

    #[test]
    fn unusual_profile_names_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let name = "Quote \" backslash \\ tab \t newline \n ünïcode";
        Preferences::load(&path).select_profile(name).unwrap();
        assert_eq!(Preferences::load(&path).values().last_profile, name);
    }

    #[test]
    fn directories_are_private_and_temporary_files_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config");
        let path = config.join("forti-vpn").join("preferences.json");
        let mut preferences = Preferences::load(&path);
        preferences.set_appearance(Appearance::Light).unwrap();
        assert_eq!(mode(&config.join("forti-vpn")), 0o700);
        assert_eq!(mode(&config), 0o700);

        let temporary = config.join("forti-vpn").join("preferences.json.tmp");
        fs::write(&temporary, "stale").unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        preferences.set_appearance(Appearance::Dark).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert!(!temporary.exists());
        assert_eq!(
            Preferences::load(&path).values().appearance,
            Appearance::Dark
        );
    }

    #[test]
    fn failed_write_keeps_values() {
        let directory = tempfile::tempdir().unwrap();
        let blocker = directory.path().join("file");
        fs::write(&blocker, "").unwrap();
        let mut preferences = Preferences::load(blocker.join("preferences.json"));
        assert!(preferences.set_close_behavior(CloseBehavior::Quit).is_err());
        assert!(preferences.select_profile("Office VPN").is_err());
        assert_eq!(preferences.values(), &PreferenceValues::default());

        let target = directory.path().join("taken");
        fs::create_dir(&target).unwrap();
        let mut preferences = Preferences::load(&target);
        assert!(preferences.set_remember_password(true).is_err());
        assert!(!preferences.values().remember_password);
        assert!(!directory.path().join("taken.tmp").exists());
    }

    #[test]
    fn default_location_follows_xdg() {
        let home = Some(PathBuf::from("/home/me"));
        assert_eq!(
            config_home(Some("/xdg/config".into()), home.clone()),
            Path::new("/xdg/config")
        );
        for ignored in [None, Some(OsString::new()), Some("relative/config".into())] {
            assert_eq!(
                config_home(ignored, home.clone()),
                Path::new("/home/me/.config")
            );
        }
        assert!(Preferences::default_path().ends_with("forti-vpn/preferences.json"));
    }
}
