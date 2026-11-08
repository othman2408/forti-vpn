//! FortiClient CLI integration and toolkit-independent logic for Forti-VPN.
//!
//! Nothing in this crate depends on GTK or GLib, so all behaviour here is
//! testable headlessly with `cargo test -p forti-vpn-core`. The GTK app in
//! `crates/forti-vpn` renders [`presentation::MainView`] and forwards user
//! actions to [`workflow::Workflow`].
pub mod backend;
pub mod cancel;
pub mod cli;
pub mod demo;
pub mod error;
pub mod gateway;
pub mod host;
pub mod model;
pub mod parse;
pub mod platform;
pub mod preferences;
pub mod presentation;
mod punycode;
pub mod report;
pub mod setup;
pub mod workflow;

pub use error::VpnError;

/// The application version, from the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_ID: &str = "io.github.fortivpn.Utility";
pub const APP_NAME: &str = "Forti VPN";
pub const REPOSITORY: &str = "https://github.com/othman2408/forti-vpn";

/// Whether the process runs with root privileges. The app refuses to: it
/// must use the desktop user's session bus, keyring, and FortiClient
/// profiles.
pub fn running_as_root() -> bool {
    rustix::process::geteuid().is_root()
}
