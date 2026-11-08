//! The About dialog: a port of `show_about()` in the Python
//! `forti_vpn/support.py`.
use std::rc::Rc;

use adw::prelude::*;
use forti_vpn_core::model::ClientInfo;
use forti_vpn_core::setup::DOWNLOAD_URL;
use forti_vpn_core::{APP_ID, APP_NAME, REPOSITORY, VERSION};
use gtk::glib;

use crate::support;
use crate::window::MainWindow;

const COMMENTS: &str = "An unofficial native GUI for FortiClient VPN. Not affiliated with \
                        Fortinet.\n\nWorks with FortiClient 7.4 on Linux with GTK 4.14 and \
                        libadwaita 1.5 or newer. FortiClient must be installed separately.";

pub(crate) fn show_about(window: &Rc<MainWindow>) {
    let checking = ClientInfo {
        version: "Checking…".into(),
        path: "Checking…".into(),
    };
    let dialog = adw::AboutDialog::builder()
        .application_name(APP_NAME)
        .application_icon(APP_ID)
        .developer_name("othman2408")
        .version(VERSION)
        .website(REPOSITORY)
        .copyright("Copyright © 2026 othman2408")
        .license_type(gtk::License::MitX11)
        .issue_url(format!("{REPOSITORY}/issues"))
        .comments(COMMENTS)
        .debug_info(support::troubleshooting_report(window, &checking))
        .debug_info_filename("forti-vpn-troubleshooting.txt")
        .build();
    dialog.add_link("Fortinet downloads", DOWNLOAD_URL);
    dialog.present(Some(&window.widgets.window));
    let about = dialog.downgrade();
    let owner = Rc::downgrade(window);
    support::probe_client(window, move |info| {
        let (Some(dialog), Some(window)) = (about.upgrade(), owner.upgrade()) else {
            return;
        };
        // Still open: a closed dialog has no root.
        if dialog.root().is_none() {
            return;
        }
        // The comments are Pango markup; the path is not.
        dialog.set_comments(&format!(
            "{COMMENTS}\n\nInstalled FortiClient: {}\nCLI: {}",
            glib::markup_escape_text(&info.version),
            glib::markup_escape_text(&info.path)
        ));
        dialog.set_debug_info(&support::troubleshooting_report(&window, &info));
    });
}
