//! Forti VPN: a native GTK 4 and libadwaita front end for FortiClient VPN.
//!
//! The binary in `main.rs` only calls [`run`]. Modules are in a library so
//! integration tests can exercise them.
mod about;
mod app;
mod dialogs;
mod settings;
mod setup_dialog;
mod support;
pub mod tray;
mod widgets;
mod window;

/// Start the application. Returns the process exit code.
pub fn run() -> gtk::glib::ExitCode {
    app::run()
}

/// What the GTK widget tests in `tests/ui.rs` drive. Not a stable API.
#[doc(hidden)]
pub mod test_support {
    pub use crate::app::{App, Config};
    pub use crate::dialogs::{AuthenticationDialog, ContentDialog, ProfileEditor};
    pub use crate::settings::SettingsDialog;
    pub use crate::setup_dialog::SetupDialog;
    pub use crate::window::{MainWindow, Services, Widgets};

    /// Register the bundled stylesheet and icons, as [`crate::run`] does.
    pub fn register_resources() {
        crate::app::register_resources();
    }
}
