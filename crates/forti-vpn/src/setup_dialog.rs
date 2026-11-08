//! Guided FortiClient setup; administrator commands are copied, never
//! executed. A port of the Python `forti_vpn/setup.py`, with
//! distribution-aware guidance from [`forti_vpn_core::setup`].
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use forti_vpn_core::model::ConnectionState;
use forti_vpn_core::parse::SERVICE_STOPPED;
use forti_vpn_core::setup::{
    DOWNLOAD_URL, HELP_URL, InstallError, InstallGuide, PackageFormat, install_command,
    install_guide, service_command,
};
use forti_vpn_core::workflow::{State, Workflow};
use gtk::{AccessibleAnnouncementPriority, gio, glib};

use crate::dialogs::ContentDialog;
use crate::widgets;
use crate::window::MainWindow;

const READY: &str = "ready";
const RECOVERY: &str = "recovery";
const INSTALL: &str = "install";
const CHECKING: &str = "Checking FortiClient…";
const RUN_SERVICE_COMMAND: &str = "Run this command in Terminal, then check again.";
/// What to do about a stopped service where no command can start it.
const START_SERVICE: &str = "Start FortiClient’s service, or ask your IT team to start it, then \
                             check again.";

pub struct SetupDialog {
    window: Weak<MainWindow>,
    guide: InstallGuide,
    command: RefCell<Option<String>>,
    showing_help: Cell<bool>,
    pub content: ContentDialog,
    pub pages: gtk::Stack,
    recovery_page: gtk::Box,
    pub recovery_title: gtk::Label,
    pub recovery_details: gtk::Label,
    recovery_help: gtk::Button,
    /// How to start FortiClient's service, when the system runs systemd.
    pub service: Option<ServiceSection>,
    pub install_title: gtk::Label,
    pub download_section: gtk::Box,
    /// Copies the repository install command (the AUR), when there is one.
    pub repository_copy: Option<gtk::Button>,
    pub select_button: gtk::Button,
    pub command_panel: gtk::Box,
    pub copy_button: gtk::Button,
    command_label: gtk::Label,
    pub check_status: gtk::Label,
    pub check: gtk::Button,
    pub close_button: gtk::Button,
}

/// What the dialog shows, as read from the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Situation {
    refreshing: bool,
    busy: bool,
    setup_required: bool,
    /// FortiClient answers: its CLI exists and its service runs.
    ready: bool,
    /// The last operation found FortiClient's service stopped.
    service_stopped: bool,
}

impl Situation {
    fn new(state: &State, refreshing: bool) -> Self {
        Self {
            refreshing,
            busy: state.busy.is_some(),
            setup_required: state.setup_required,
            ready: !state.setup_required && state.status.state != ConnectionState::Unavailable,
            service_stopped: state.error.contains(SERVICE_STOPPED),
        }
    }

    fn page(self, showing_help: bool) -> &'static str {
        if showing_help || self.setup_required {
            INSTALL
        } else if self.ready {
            READY
        } else {
            RECOVERY
        }
    }

    /// The footer's status line. `has_command` is whether the dialog shows
    /// the command that starts FortiClient's service.
    fn message(self, has_command: bool) -> &'static str {
        if self.refreshing {
            CHECKING
        } else if self.setup_required {
            "FortiClient CLI has not been found yet."
        } else if self.ready {
            "FortiClient is ready."
        } else if self.service_stopped {
            stopped_service_advice(has_command)
        } else {
            "FortiClient is not responding. Open it to finish setup, then check again."
        }
    }

    /// The recovery page's title and details (empty to hide them).
    fn recovery_text(self, has_command: bool) -> (&'static str, &'static str) {
        if self.refreshing {
            (CHECKING, "Checking the CLI and VPN service.")
        } else if self.service_stopped {
            // The command section, first on the page, says what to do.
            let details = if has_command { "" } else { START_SERVICE };
            ("FortiClient’s service is stopped", details)
        } else {
            (
                "FortiClient needs attention",
                "The CLI was found, but the VPN is not ready. Open FortiClient to finish setup, \
                 or ask your IT team to check its service.",
            )
        }
    }
}

fn stopped_service_advice(has_command: bool) -> &'static str {
    if has_command {
        RUN_SERVICE_COMMAND
    } else {
        START_SERVICE
    }
}

/// The widgets of the page shown when FortiClient is installed but not
/// ready.
struct RecoveryPage {
    page: gtk::Box,
    title: gtk::Label,
    details: gtk::Label,
    help: gtk::Button,
    service: Option<ServiceSection>,
}

/// The command that starts FortiClient's service, with what it is for.
pub struct ServiceSection {
    pub container: gtk::Box,
    pub description: gtk::Label,
    pub copy: gtk::Button,
}

/// The widgets of the installation guide page.
struct InstallPage {
    page: gtk::Box,
    title: gtk::Label,
    download_section: gtk::Box,
    repository_copy: Option<gtk::Button>,
    select_button: gtk::Button,
    command_panel: gtk::Box,
    copy_button: gtk::Button,
    command_label: gtk::Label,
}

impl SetupDialog {
    pub(crate) fn new(window: &Rc<MainWindow>) -> Rc<Self> {
        let guide = install_guide(window.platform());
        let content = ContentDialog::new("Set Up FortiClient", 440);
        let pages = gtk::Stack::builder().vhomogeneous(false).build();
        content.body.append(&pages);
        let (ready, ready_help) = ready_page();
        pages.add_named(&ready, Some(READY));
        let recovery = RecoveryPage::build(service_command(window.platform()));
        pages.add_named(&recovery.page, Some(RECOVERY));
        let install = InstallPage::build(&guide);
        pages.add_named(&install.page, Some(INSTALL));
        let check_status = widgets::message_label(None);
        content
            .footer
            .insert_child_after(&check_status, Some(&content.error));
        let check = widgets::button("Check Again", Some("suggested-action"));
        let close_button = widgets::button("Close", None);
        content.set_actions(&[&close_button, &check]);
        content.actions.set_homogeneous(false);
        close_button.set_hexpand(true);
        check.set_hexpand(true);
        let dialog = Rc::new(Self {
            window: Rc::downgrade(window),
            guide,
            command: RefCell::default(),
            showing_help: Cell::new(false),
            content,
            pages,
            recovery_page: recovery.page,
            recovery_title: recovery.title,
            recovery_details: recovery.details,
            recovery_help: recovery.help,
            service: recovery.service,
            install_title: install.title,
            download_section: install.download_section,
            repository_copy: install.repository_copy,
            select_button: install.select_button,
            command_panel: install.command_panel,
            copy_button: install.copy_button,
            command_label: install.command_label,
            check_status,
            check,
            close_button,
        });
        dialog.connect_signals(&ready_help);
        dialog.connect_closed(window);
        dialog.update();
        dialog
            .content
            .dialog
            .set_focus(Some(dialog.initial_focus()));
        dialog
    }

    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.content.dialog.present(Some(parent));
        // libadwaita 1.5.0 can drop the dialog's initial focus.
        self.initial_focus().grab_focus();
    }

    pub fn close(&self) {
        self.content.dialog.close();
    }

    /// The first control of the visible page that works right away, else
    /// Close. Never Check Again: the check the dialog starts with makes it
    /// insensitive.
    pub fn initial_focus(&self) -> &gtk::Button {
        let page = self.pages.visible_child_name();
        let candidates = match page.as_deref() {
            Some(INSTALL) => [self.repository_copy.as_ref(), Some(&self.select_button)],
            Some(RECOVERY) => [self.service.as_ref().map(|service| &service.copy), None],
            _ => [None, None],
        };
        candidates
            .into_iter()
            .flatten()
            // Visible with all its parents, and sensitive.
            .find(|button| button.is_visible() && button.is_sensitive())
            .unwrap_or(&self.close_button)
    }

    fn connect_signals(self: &Rc<Self>, ready_help: &gtk::Button) {
        self.on_click(ready_help, Self::show_help);
        self.on_click(&self.recovery_help, Self::show_help);
        self.on_click(&self.select_button, Self::choose_package);
        self.on_click(&self.copy_button, Self::copy_command);
        self.on_click(&self.check, Self::check_again);
        let dialog = self.content.dialog.downgrade();
        self.close_button.connect_clicked(move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        });
    }

    fn on_click(self: &Rc<Self>, button: &gtk::Button, action: fn(&Rc<Self>)) {
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(dialog) = weak.upgrade() {
                action(&dialog);
            }
        });
    }

    fn connect_closed(&self, window: &Rc<MainWindow>) {
        let window = Rc::downgrade(window);
        self.content.dialog.connect_closed(move |dialog| {
            if let Some(window) = window.upgrade() {
                window.setup_closed(dialog);
            }
        });
    }

    /// Show the installation guide, even when FortiClient is ready.
    pub fn show_help(self: &Rc<Self>) {
        self.showing_help.set(true);
        self.update();
    }

    fn check_again(self: &Rc<Self>) {
        self.showing_help.set(false);
        if let Some(window) = self.window.upgrade() {
            window.operate(Workflow::refresh);
        }
    }

    /// Show the page and messages that match FortiClient's current state.
    pub fn update(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let refreshing = window.is_refreshing();
        let situation = window.with_state(|state| Situation::new(state, refreshing));
        let page = situation.page(self.showing_help.get());
        self.pages.set_visible_child_name(page);
        self.show_recovery(situation);
        let ready_page = page == READY;
        self.check.set_visible(!ready_page || refreshing);
        self.enable_check(!situation.busy && !refreshing);
        widgets::set_css_class(&self.close_button, "suggested-action", ready_page);
        self.check_status.set_visible(!ready_page || refreshing);
        self.show_status(situation.message(self.service.is_some()));
        // Recalculate the height after switching between the compact and
        // installation pages.
        self.content.dialog.set_content_height(-1);
    }

    /// Check Again works only while nothing runs. Turning insensitive while
    /// it checks can leave nothing focused, so then the focus returns to it
    /// once it works again.
    fn enable_check(&self, sensitive: bool) {
        let enabled = sensitive && !self.check.get_sensitive();
        self.check.set_sensitive(sensitive);
        if enabled && self.check.is_visible() && self.content.dialog.focus().is_none() {
            self.check.grab_focus();
        }
    }

    fn show_status(&self, message: &str) {
        if self.check_status.text() != message {
            self.check_status.set_text(message);
            self.check_status
                .announce(message, AccessibleAnnouncementPriority::Medium);
        }
    }

    fn show_recovery(&self, situation: Situation) {
        let (title, details) = situation.recovery_text(self.service.is_some());
        self.recovery_title.set_text(title);
        self.recovery_details.set_text(details);
        self.recovery_details.set_visible(!details.is_empty());
        self.recovery_help.set_visible(!situation.refreshing);
        if let Some(service) = &self.service {
            service.container.set_visible(!situation.refreshing);
            service.show(situation.service_stopped);
            // A stopped service is the problem itself: its command comes
            // first.
            let previous: &gtk::Widget = if situation.service_stopped {
                self.recovery_title.upcast_ref()
            } else {
                self.recovery_details.upcast_ref()
            };
            self.recovery_page
                .reorder_child_after(&service.container, Some(previous));
        }
    }

    fn choose_package(self: &Rc<Self>) {
        let Some(format) = self.guide.format else {
            return;
        };
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(format.filter_name()));
        for pattern in format.patterns() {
            filter.add_pattern(pattern);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let chooser = gtk::FileDialog::builder()
            .title("Choose FortiClient installer")
            .filters(&filters)
            .default_filter(&filter)
            .build();
        let parent = self
            .window
            .upgrade()
            .map(|window| window.widgets.window.clone());
        let weak = Rc::downgrade(self);
        chooser.open(parent.as_ref(), gio::Cancellable::NONE, move |result| {
            if let Some(dialog) = weak.upgrade() {
                dialog.package_chosen(format, result);
            }
        });
    }

    fn package_chosen(
        self: &Rc<Self>,
        format: PackageFormat,
        result: Result<gio::File, glib::Error>,
    ) {
        match result {
            Ok(file) => match file.path() {
                Some(path) => self.set_package(&path),
                // Not a local file, so no administrator could install it.
                None => self
                    .content
                    .show_error(&InstallError::Missing(format).message()),
            },
            Err(error) if error.matches(gtk::DialogError::Dismissed) => {}
            Err(_) => self
                .content
                .show_error("Could not select the installer. Try again."),
        }
    }

    /// Show the command that installs the package at `path`.
    pub fn set_package(self: &Rc<Self>, path: &Path) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let command = match install_command(window.platform(), path) {
            Ok(command) => command,
            Err(error) => return self.content.show_error(&error.message()),
        };
        self.command_label.set_text(&command);
        self.command.replace(Some(command));
        self.showing_help.set(true);
        self.command_panel.set_visible(true);
        self.download_section.set_visible(false);
        self.install_title.set_text("Installer selected");
        self.select_button.set_label("Change Installer…");
        self.update();
        self.copy_button.set_sensitive(true);
        self.copy_button.set_label("Copy Install Command");
        self.content.show_error("");
    }

    fn copy_command(self: &Rc<Self>) {
        // Cloned, so no borrow is held while the clipboard and the button
        // emit their signals.
        let command = self.command.borrow().clone();
        if let Some(command) = command {
            widgets::copy_with_feedback(&self.copy_button, &command, "Install command copied");
        }
    }
}

fn ready_page() -> (gtk::Box, gtk::Button) {
    let page = widgets::vertical_box(12);
    page.set_margin_top(12);
    page.set_margin_bottom(12);
    let icon = gtk::Image::builder()
        .icon_name("emblem-ok-symbolic")
        .pixel_size(48)
        .css_classes(["success"])
        .build();
    page.append(&icon);
    page.append(&widgets::centered_label(
        "FortiClient is ready",
        Some("title-2"),
    ));
    page.append(&widgets::centered_label(
        "You can now manage your VPN profiles and connect.",
        None,
    ));
    let help = widgets::button("Installation Help", Some("flat"));
    help.set_halign(gtk::Align::Center);
    page.append(&help);
    (page, help)
}

impl RecoveryPage {
    fn build(service_command: Option<String>) -> Self {
        let page = widgets::vertical_box(12);
        page.set_margin_top(12);
        page.set_margin_bottom(12);
        let title = widgets::styled_label("FortiClient needs attention", "title-3");
        let details = widgets::message_label(None);
        page.append(&title);
        page.append(&details);
        let service = service_command.map(|command| ServiceSection::build(&command));
        if let Some(service) = &service {
            page.append(&service.container);
        }
        let help = widgets::button("Installation Help", Some("flat"));
        page.append(&help);
        Self {
            page,
            title,
            details,
            help,
            service,
        }
    }
}

impl ServiceSection {
    fn build(command: &str) -> Self {
        let container = widgets::vertical_box(8);
        container.append(&widgets::styled_label(
            "Start the FortiClient service",
            "heading",
        ));
        let description = widgets::styled_label("", "dim-label");
        container.append(&description);
        let block = widgets::CommandBlock::new(command);
        container.append(&block.container);
        let section = Self {
            container,
            description,
            copy: block.copy,
        };
        section.show(false);
        section
    }

    /// Describe the command for a service known to be stopped, or one that
    /// may be.
    fn show(&self, stopped: bool) {
        self.description.set_text(if stopped {
            RUN_SERVICE_COMMAND
        } else {
            "FortiClient’s service may be stopped. Run this command in Terminal, then check again."
        });
    }
}

impl InstallPage {
    fn build(guide: &InstallGuide) -> Self {
        let page = widgets::vertical_box(24);
        let title = widgets::styled_label("Install FortiClient to get started.", "title-3");
        page.append(&title);
        let (download_section, repository_copy) = download_section(guide);
        page.append(&download_section);
        let select_button = widgets::button("Select Installer…", None);
        select_button.set_visible(guide.format.is_some());
        let command_label = widgets::command_label("");
        let copy_button = widgets::button("Copy Install Command", None);
        let command_panel = widgets::vertical_box(12);
        command_panel.set_visible(false);
        command_panel.append(&widgets::styled_label(
            "Run the copied command in Terminal.",
            "dim-label",
        ));
        command_panel.append(&copy_button);
        let details = gtk::Expander::builder()
            .label("Show command")
            .child(&command_label)
            .build();
        command_panel.append(&details);
        let install = widgets::vertical_box(12);
        install.append(&install_heading(guide));
        install.append(&select_button);
        install.append(&command_panel);
        if let Some(note) = &guide.note {
            install.append(&widgets::styled_label(note, "dim-label"));
        }
        // Without a package format or a note, there is nothing to install here.
        install.set_visible(guide.format.is_some() || guide.note.is_some());
        page.append(&install);
        page.append(&check_section());
        page.append(&help_section());
        Self {
            page,
            title,
            download_section,
            repository_copy,
            select_button,
            command_panel,
            copy_button,
            command_label,
        }
    }
}

/// Where FortiClient comes from: Fortinet's downloads, or the repository
/// command that builds and installs it (the AUR), with that command's copy
/// button.
fn download_section(guide: &InstallGuide) -> (gtk::Box, Option<gtk::Button>) {
    let section = widgets::vertical_box(8);
    let Some(command) = &guide.repository_command else {
        section.append(&widgets::styled_label("Download", "heading"));
        section.append(&widgets::styled_label(
            &guide.download_description,
            "dim-label",
        ));
        section.append(&widgets::styled_label(&guide.download_hint, "dim-label"));
        section.append(&widgets::link("Download FortiClient", DOWNLOAD_URL));
        return (section, None);
    };
    section.append(&widgets::styled_label("Install from the AUR", "heading"));
    section.append(&widgets::styled_label(
        &guide.download_description,
        "dim-label",
    ));
    section.append(&widgets::styled_label(
        "This command builds and installs FortiClient from the AUR.",
        "dim-label",
    ));
    let block = widgets::CommandBlock::new(command);
    section.append(&block.container);
    (section, Some(block.copy))
}

/// What the package file section is called. Where a repository command
/// installs FortiClient, a package file is only for one built already.
fn install_heading(guide: &InstallGuide) -> gtk::Label {
    if guide.repository_command.is_some() {
        widgets::styled_label("Already built the package?", "caption-heading")
    } else {
        widgets::styled_label("Install", "heading")
    }
}

fn check_section() -> gtk::Box {
    let section = widgets::vertical_box(8);
    section.append(&widgets::styled_label("Check", "heading"));
    section.append(&widgets::styled_label(
        "Finish setup in FortiClient, then check again.",
        "dim-label",
    ));
    section
}

fn help_section() -> gtk::Expander {
    let content = widgets::vertical_box(12);
    content.set_margin_top(12);
    content.append(&widgets::styled_label(
        "For a work VPN, use the installer your IT team provides. This app supports the \
         FortiClient 7.4 VPN CLI.",
        "dim-label",
    ));
    content.append(&widgets::link("Installation Guide", HELP_URL));
    gtk::Expander::builder()
        .label("Need help?")
        .child(&content)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use forti_vpn_core::model::Status;

    fn situation(change: impl FnOnce(&mut State)) -> Situation {
        let mut state = State {
            status: Status::new(ConnectionState::Disconnected),
            ..State::default()
        };
        change(&mut state);
        Situation::new(&state, false)
    }

    #[test]
    fn a_stopped_service_says_how_to_start_it() {
        let stopped = situation(|state| {
            state.status = Status::unavailable();
            state.error = SERVICE_STOPPED.to_owned();
        });
        assert_eq!(stopped.page(false), RECOVERY);
        assert_eq!(stopped.message(true), RUN_SERVICE_COMMAND);
        assert_eq!(
            stopped.recovery_text(true),
            ("FortiClient’s service is stopped", "")
        );
        assert_eq!(stopped.message(false), START_SERVICE);
        assert_eq!(stopped.recovery_text(false).1, START_SERVICE);
        let checking = Situation {
            refreshing: true,
            ..stopped
        };
        assert_eq!(checking.message(true), CHECKING);
        assert_eq!(checking.recovery_text(true).0, CHECKING);
    }

    #[test]
    fn other_failures_keep_the_usual_recovery_text() {
        let unavailable = situation(|state| {
            state.status = Status::unavailable();
            state.error = "FortiClient did not answer.".to_owned();
        });
        assert_eq!(unavailable.page(false), RECOVERY);
        assert_eq!(unavailable.page(true), INSTALL);
        assert_eq!(
            unavailable.recovery_text(true).0,
            "FortiClient needs attention"
        );
        assert!(unavailable.message(true).contains("not responding"));
        let ready = situation(|_| {});
        assert_eq!(ready.page(false), READY);
        assert_eq!(ready.message(true), "FortiClient is ready.");
        let missing = situation(|state| state.setup_required = true);
        assert_eq!(missing.page(false), INSTALL);
        assert!(missing.message(true).contains("not been found"));
    }
}
