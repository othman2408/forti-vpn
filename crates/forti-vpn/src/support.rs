//! Version information for Settings and About: a port of the Python
//! `forti_vpn/support.py`. The report itself is an allowlist built by
//! [`forti_vpn_core::report`].
use std::rc::Rc;

use forti_vpn_core::model::ClientInfo;
use forti_vpn_core::report::{ReportInputs, troubleshooting_report as report};
use gtk::{gio, glib};

use crate::window::MainWindow;

/// Read FortiClient's version on a worker thread and hand it to `deliver`
/// on the main thread, unless the window closed meanwhile. One bounded read
/// per dialog opening; no polling or VPN operations.
pub(crate) fn probe_client(window: &Rc<MainWindow>, deliver: impl FnOnce(ClientInfo) + 'static) {
    let backend = window.backend();
    let window = Rc::downgrade(window);
    glib::spawn_future_local(async move {
        let info = gio::spawn_blocking(move || backend.information())
            .await
            // A panicking backend is reported like a missing client.
            .unwrap_or_else(|_| ClientInfo::unavailable());
        if window.upgrade().is_some_and(|window| !window.is_closed()) {
            deliver(info);
        }
    });
}

/// The copyable troubleshooting report, with the GTK and libadwaita
/// versions this process runs with.
pub(crate) fn troubleshooting_report(window: &MainWindow, client: &ClientInfo) -> String {
    let gtk_version = format!(
        "{}.{}.{}",
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version()
    );
    let adwaita_version = format!(
        "{}.{}.{}",
        adw::major_version(),
        adw::minor_version(),
        adw::micro_version()
    );
    let tray_available = window.indicator_available();
    window.with_state(|state| {
        report(&ReportInputs {
            platform: window.platform(),
            gtk_version: &gtk_version,
            adwaita_version: &adwaita_version,
            client,
            status: state.status.state,
            tray_available,
            diagnostics: &state.diagnostics,
        })
    })
}
