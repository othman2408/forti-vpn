//! Preferences and session diagnostics: a port of the Python
//! `forti_vpn/settings.py`.
use std::cell::RefCell;
use std::io;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use forti_vpn_core::model::ClientInfo;
use forti_vpn_core::platform::Platform;
use forti_vpn_core::preferences::{Appearance, CloseBehavior, PreferenceValues, Preferences};
use forti_vpn_core::report::COMPATIBILITY;
use forti_vpn_core::setup::{TrayHelp, tray_help};
use gtk::{gio, glib};

use crate::support;
use crate::widgets;
use crate::window::MainWindow;

const TRAY_AVAILABLE: &str = "Available in this desktop session";
const TRAY_CHECKING: &str = "Checking…";
const APPEARANCES: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

pub struct SettingsDialog {
    window: Weak<MainWindow>,
    tray_help: TrayHelp,
    client: RefCell<Option<ClientInfo>>,
    last_toast: RefCell<Option<adw::Toast>>,
    pub dialog: adw::PreferencesDialog,
    pub close_behavior: adw::ComboRow,
    pub tray_row: adw::ActionRow,
    /// The row under `tray_row` with `tray_help.command`, when there is one.
    tray_setup: Option<adw::PreferencesRow>,
    /// `tray_help.command` as selectable text, when there is one.
    pub tray_command: Option<gtk::Label>,
    /// Copies `tray_help.command`, when there is one.
    pub tray_copy: Option<gtk::Button>,
    /// Opens `tray_help.link`, when there is one.
    pub tray_link: Option<gtk::LinkButton>,
    pub appearance: adw::ComboRow,
    pub remember: adw::SwitchRow,
    pub diagnostics_page: adw::PreferencesPage,
    pub client_row: adw::ActionRow,
    pub cli_row: adw::ActionRow,
    pub report_button: gtk::Button,
    pub text: gtk::TextView,
    pub copy_button: gtk::Button,
    pub save_button: gtk::Button,
    pub clear_button: gtk::Button,
}

impl SettingsDialog {
    pub(crate) fn new(window: &Rc<MainWindow>) -> Rc<Self> {
        let platform = window.platform();
        let tray_help = tray_help(platform);
        let general = General::build(platform, &window.preferences(), &tray_help);
        let diagnostics = Diagnostics::build(platform);
        let dialog = adw::PreferencesDialog::builder()
            .title("Settings")
            .content_width(540)
            .content_height(620)
            .search_enabled(false)
            .build();
        dialog.add(&general.page);
        dialog.add(&diagnostics.page);
        let settings = Rc::new(Self {
            window: Rc::downgrade(window),
            tray_help,
            client: RefCell::default(),
            last_toast: RefCell::default(),
            dialog,
            close_behavior: general.close_behavior,
            tray_row: general.tray.row,
            tray_setup: general.tray.setup,
            tray_command: general.tray.command,
            tray_copy: general.tray.copy,
            tray_link: general.tray.link,
            appearance: general.appearance,
            remember: general.remember,
            diagnostics_page: diagnostics.page,
            client_row: diagnostics.client_row,
            cli_row: diagnostics.cli_row,
            report_button: diagnostics.report_button,
            text: diagnostics.text,
            copy_button: diagnostics.copy_button,
            save_button: diagnostics.save_button,
            clear_button: diagnostics.clear_button,
        });
        settings.connect_signals();
        settings.connect_closed(window);
        settings.update();
        settings.probe(window);
        settings
    }

    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.dialog.present(Some(parent));
    }

    pub fn close(&self) {
        self.dialog.close();
    }

    pub fn show_diagnostics(&self) {
        self.dialog.set_visible_page(&self.diagnostics_page);
    }

    /// The latest toast, for tests.
    pub fn last_toast(&self) -> Option<adw::Toast> {
        self.last_toast.borrow().clone()
    }

    /// Show the current session log and tray availability. The window
    /// calls this whenever it renders, so it follows the tray and the
    /// preferences.
    pub fn update(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let contents = window.with_state(|state| state.diagnostics.join("\n"));
        let buffer = self.text.buffer();
        if buffer.text(&buffer.start_iter(), &buffer.end_iter(), false) != contents {
            buffer.set_text(&contents);
        }
        let close_behavior = window.preferences().close_behavior;
        self.show_tray_status(window.indicator(), close_behavior);
    }

    fn show_tray_status(&self, available: Option<bool>, close_behavior: CloseBehavior) {
        let subtitle = tray_status(available, close_behavior, &self.tray_help.summary);
        self.tray_row
            .set_subtitle(&glib::markup_escape_text(&subtitle));
        // Setup help only matters once the tray is known to be missing.
        let missing = available == Some(false);
        if let Some(setup) = &self.tray_setup {
            setup.set_visible(missing);
        }
        if let Some(link) = &self.tray_link {
            link.set_visible(missing);
        }
    }

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.close_behavior.connect_selected_notify(move |row| {
            if let Some(settings) = weak.upgrade() {
                let value = if row.selected() == 0 {
                    CloseBehavior::Hide
                } else {
                    CloseBehavior::Quit
                };
                settings.store(
                    |values| values.close_behavior == value,
                    |preferences| preferences.set_close_behavior(value),
                );
            }
        });
        let weak = Rc::downgrade(self);
        self.appearance.connect_selected_notify(move |row| {
            if let Some(settings) = weak.upgrade() {
                let value = appearance_at(row.selected());
                settings.store(
                    |values| values.appearance == value,
                    |preferences| preferences.set_appearance(value),
                );
            }
        });
        let weak = Rc::downgrade(self);
        self.remember.connect_active_notify(move |row| {
            if let Some(settings) = weak.upgrade() {
                let value = row.is_active();
                settings.store(
                    |values| values.remember_password == value,
                    |preferences| preferences.set_remember_password(value),
                );
            }
        });
        self.on_click(&self.report_button, Self::copy_report);
        self.on_click(&self.copy_button, Self::copy);
        self.on_click(&self.save_button, Self::save);
        self.on_click(&self.clear_button, Self::clear);
    }

    fn on_click(self: &Rc<Self>, button: &gtk::Button, action: fn(&Rc<Self>)) {
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(settings) = weak.upgrade() {
                action(&settings);
            }
        });
    }

    fn connect_closed(&self, window: &Rc<MainWindow>) {
        let window = Rc::downgrade(window);
        self.dialog.connect_closed(move |dialog| {
            if let Some(window) = window.upgrade() {
                window.settings_closed(dialog.upcast_ref());
            }
        });
    }

    /// One bounded read of the installed FortiClient version per opening.
    fn probe(self: &Rc<Self>, window: &Rc<MainWindow>) {
        let weak = Rc::downgrade(self);
        support::probe_client(window, move |info| {
            if let Some(settings) = weak.upgrade() {
                settings.client_detected(info);
            }
        });
    }

    fn client_detected(&self, info: ClientInfo) {
        self.client_row
            .set_subtitle(&glib::markup_escape_text(&info.version));
        self.cli_row
            .set_subtitle(&glib::markup_escape_text(&info.path));
        self.client.replace(Some(info));
        self.report_button.set_sensitive(true);
    }

    /// Persist a changed setting. `unchanged` skips writing a value that is
    /// already stored, such as one restored after a failed write.
    fn store(
        &self,
        unchanged: impl FnOnce(&PreferenceValues) -> bool,
        write: impl FnOnce(&mut Preferences) -> io::Result<()>,
    ) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        if unchanged(&window.preferences()) {
            return;
        }
        if window.update_preferences(write).is_err() {
            self.toast("Could not save this setting.", None);
            // Reflect the persisted choice instead of displaying an unsaved value.
            self.show_preferences(&window.preferences());
        }
    }

    fn show_preferences(&self, values: &PreferenceValues) {
        self.close_behavior
            .set_selected(close_index(values.close_behavior));
        self.appearance
            .set_selected(appearance_index(values.appearance));
        self.remember.set_active(values.remember_password);
    }

    fn toast(&self, title: &str, timeout: Option<u32>) {
        let toast = adw::Toast::new(title);
        if let Some(seconds) = timeout {
            toast.set_timeout(seconds);
        }
        self.dialog.add_toast(toast.clone());
        self.last_toast.replace(Some(toast));
    }

    fn diagnostics_text(&self) -> String {
        self.window
            .upgrade()
            .map(|window| window.with_state(|state| state.diagnostics.join("\n")))
            .unwrap_or_default()
    }

    fn copy_report(self: &Rc<Self>) {
        let (Some(window), Some(client)) = (self.window.upgrade(), self.client.borrow().clone())
        else {
            return;
        };
        let report = support::troubleshooting_report(&window, &client);
        self.dialog.clipboard().set_text(&report);
        self.toast("Troubleshooting report copied", Some(3));
    }

    fn copy(self: &Rc<Self>) {
        self.dialog.clipboard().set_text(&self.diagnostics_text());
        self.toast("Diagnostics copied", Some(3));
    }

    fn clear(self: &Rc<Self>) {
        if let Some(window) = self.window.upgrade() {
            window.clear_diagnostics();
        }
        self.update();
        self.toast("Session log cleared", Some(3));
    }

    fn save(self: &Rc<Self>) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let contents = self.diagnostics_text();
        let chooser = gtk::FileDialog::builder()
            .title("Save diagnostics")
            .initial_name("forti-vpn-diagnostics.txt")
            .build();
        let weak = Rc::downgrade(self);
        chooser.save(
            Some(&window.widgets.window),
            gio::Cancellable::NONE,
            move |result| {
                // Write even if Settings closed while the picker was open.
                let written = result.and_then(|file| {
                    file.replace_contents(
                        contents.as_bytes(),
                        None,
                        false,
                        gio::FileCreateFlags::PRIVATE,
                        gio::Cancellable::NONE,
                    )
                    .map(|_| ())
                });
                if let Some(settings) = weak.upgrade() {
                    settings.saved(written);
                }
            },
        );
    }

    fn saved(&self, written: Result<(), glib::Error>) {
        match written {
            Ok(()) => self.toast("Diagnostics saved", Some(3)),
            Err(error) if error.matches(gtk::DialogError::Dismissed) => {}
            Err(_) => self.toast("Could not save diagnostics", None),
        }
    }
}

/// The General page's widgets.
struct General {
    page: adw::PreferencesPage,
    close_behavior: adw::ComboRow,
    tray: TrayRows,
    appearance: adw::ComboRow,
    remember: adw::SwitchRow,
}

impl General {
    fn build(platform: &Platform, values: &PreferenceValues, tray_help: &TrayHelp) -> Self {
        let page = adw::PreferencesPage::builder()
            .title("General")
            .icon_name("preferences-system-symbolic")
            .build();
        let term = platform.tray_term();
        let keep = format!("Keep in {term}");
        let close_behavior = combo_row(
            "On window close",
            &[keep.as_str(), "Disconnect and quit"],
            close_index(values.close_behavior),
        );
        let tray = TrayRows::build(term, tray_help);
        let behavior = group(
            "Window behavior",
            Some("Quit always disconnects the VPN before exiting."),
            &[close_behavior.upcast_ref(), tray.row.upcast_ref()],
        );
        if let Some(setup) = &tray.setup {
            behavior.add(setup);
        }
        page.add(&behavior);
        let appearance = combo_row(
            "Color scheme",
            &["Follow system", "Light", "Dark"],
            appearance_index(values.appearance),
        );
        page.add(&group("Appearance", None, &[appearance.upcast_ref()]));
        let remember = adw::SwitchRow::builder()
            .title("Remember password by default")
            .subtitle(
                "Uses FortiClient credential storage. Existing saved passwords are not erased \
                 when this is off.",
            )
            .active(values.remember_password)
            .build();
        page.add(&group("Sign in", None, &[remember.upcast_ref()]));
        Self {
            page,
            close_behavior,
            tray,
            appearance,
            remember,
        }
    }
}

/// The tray status row, the setup command in full under it, and a link to
/// more help, when `help` has them.
struct TrayRows {
    row: adw::ActionRow,
    setup: Option<adw::PreferencesRow>,
    command: Option<gtk::Label>,
    copy: Option<gtk::Button>,
    link: Option<gtk::LinkButton>,
}

impl TrayRows {
    fn build(term: &str, help: &TrayHelp) -> Self {
        let title = if term == "top bar" {
            "Top-bar icon"
        } else {
            "Tray icon"
        };
        let row = adw::ActionRow::builder().title(title).build();
        let link = help.link.map(|uri| {
            let link = widgets::link("Learn More", uri);
            link.set_valign(gtk::Align::Center);
            link
        });
        if let Some(link) = &link {
            row.add_suffix(link);
        }
        // The command is shown, not only copied: users see what they run.
        let block = help.command.as_deref().map(widgets::CommandBlock::new);
        let setup = block.as_ref().map(|block| {
            widgets::margins(&block.container, 12);
            adw::PreferencesRow::builder()
                .title(format!("{title} setup command"))
                .activatable(false)
                .focusable(false)
                .child(&block.container)
                .build()
        });
        let (command, copy) = block.map(|block| (block.label, block.copy)).unzip();
        Self {
            row,
            setup,
            command,
            copy,
            link,
        }
    }
}

/// The Diagnostics page's widgets.
struct Diagnostics {
    page: adw::PreferencesPage,
    client_row: adw::ActionRow,
    cli_row: adw::ActionRow,
    report_button: gtk::Button,
    text: gtk::TextView,
    copy_button: gtk::Button,
    save_button: gtk::Button,
    clear_button: gtk::Button,
}

impl Diagnostics {
    fn build(platform: &Platform) -> Self {
        let page = adw::PreferencesPage::builder()
            .title("Diagnostics")
            .icon_name("utilities-terminal-symbolic")
            .build();
        let client_row = info_row("FortiClient", "Checking installed version…");
        let cli_row = info_row("CLI executable", "Checking…");
        let system_row = info_row("System", &platform.os.pretty_name);
        let desktop_row = info_row(
            "Desktop",
            &format!("{} · {}", platform.desktop_name, platform.session.as_str()),
        );
        let report_button = report_button();
        let compatibility = group(
            "Compatibility",
            Some(COMPATIBILITY),
            &[
                client_row.upcast_ref(),
                cli_row.upcast_ref(),
                system_row.upcast_ref(),
                desktop_row.upcast_ref(),
            ],
        );
        compatibility.add(&report_button);
        page.add(&compatibility);
        let text = session_log();
        let copy_button = log_button("Copy", "edit-copy-symbolic");
        let save_button = log_button("Save", "document-save-symbolic");
        let clear_button = log_button("Clear", "edit-clear-all-symbolic");
        page.add(&log_group(
            &text,
            [&copy_button, &save_button, &clear_button],
        ));
        Self {
            page,
            client_row,
            cli_row,
            report_button,
            text,
            copy_button,
            save_button,
            clear_button,
        }
    }
}

/// Enabled once FortiClient's version is known.
fn report_button() -> gtk::Button {
    // The label wraps so large text still fits narrow windows.
    let label = gtk::Label::builder()
        .label("Copy troubleshooting report")
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    gtk::Button::builder()
        .child(&label)
        .sensitive(false)
        .margin_top(12)
        .tooltip_text("Copy versions, connection state, and sanitized session events")
        .build()
}

/// The session log and the buttons that copy, save, and clear it.
fn log_group(text: &gtk::TextView, buttons: [&gtk::Button; 3]) -> adw::PreferencesGroup {
    let scroll = gtk::ScrolledWindow::builder()
        .min_content_height(180)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(text)
        .build();
    scroll.add_css_class("card");
    let actions = gtk::Box::builder()
        .spacing(8)
        .homogeneous(true)
        .margin_top(12)
        .build();
    for button in buttons {
        actions.append(button);
    }
    let log = group(
        "Session log",
        Some("Operation events only. Passwords, codes, and raw terminal output are excluded."),
        &[],
    );
    log.add(&scroll);
    log.add(&actions);
    log
}

fn session_log() -> gtk::TextView {
    let text = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(true)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .build();
    text.update_property(&[gtk::accessible::Property::Label(
        "Sanitized session diagnostics",
    )]);
    text
}

fn log_button(title: &str, icon: &str) -> gtk::Button {
    widgets::icon_button(title, icon, &format!("{title} diagnostics"))
}

fn info_row(title: &str, subtitle: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(glib::markup_escape_text(subtitle))
        .build()
}

fn combo_row(title: &str, choices: &[&str], selected: u32) -> adw::ComboRow {
    adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(choices))
        .selected(selected)
        .build()
}

fn group(title: &str, description: Option<&str>, rows: &[&gtk::Widget]) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(title).build();
    if let Some(description) = description {
        group.set_description(Some(description));
    }
    for row in rows {
        group.add(*row);
    }
    group
}

/// The tray row's subtitle: whether a tray shows the icon (`None` while
/// that is not known yet), what closing the window then does, and `help`
/// to get one.
fn tray_status(available: Option<bool>, close_behavior: CloseBehavior, help: &str) -> String {
    match (available, close_behavior) {
        (Some(true), _) => TRAY_AVAILABLE.to_owned(),
        (None, _) => TRAY_CHECKING.to_owned(),
        (Some(false), CloseBehavior::Hide) => {
            format!("Unavailable — the window stays open when you close it. {help}")
        }
        // Closing quits anyway, so the tray changes nothing about it.
        (Some(false), CloseBehavior::Quit) => format!("Unavailable. {help}"),
    }
}

fn close_index(behavior: CloseBehavior) -> u32 {
    match behavior {
        CloseBehavior::Hide => 0,
        CloseBehavior::Quit => 1,
    }
}

fn appearance_index(appearance: Appearance) -> u32 {
    match appearance {
        Appearance::System => 0,
        Appearance::Light => 1,
        Appearance::Dark => 2,
    }
}

fn appearance_at(index: u32) -> Appearance {
    usize::try_from(index)
        .ok()
        .and_then(|index| APPEARANCES.get(index).copied())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_round_trip() {
        for appearance in APPEARANCES {
            assert_eq!(appearance_at(appearance_index(appearance)), appearance);
        }
        assert_eq!(appearance_at(u32::MAX), Appearance::System);
        assert_eq!(close_index(CloseBehavior::Hide), 0);
        assert_eq!(close_index(CloseBehavior::Quit), 1);
    }

    #[test]
    fn tray_status_says_what_closing_does() {
        let help = "Add the System Tray widget to your panel.";
        for behavior in [CloseBehavior::Hide, CloseBehavior::Quit] {
            assert_eq!(tray_status(Some(true), behavior, help), TRAY_AVAILABLE);
            assert_eq!(tray_status(None, behavior, help), TRAY_CHECKING);
        }
        assert_eq!(
            tray_status(Some(false), CloseBehavior::Hide, help),
            "Unavailable — the window stays open when you close it. Add the System Tray widget \
             to your panel."
        );
        assert_eq!(
            tray_status(Some(false), CloseBehavior::Quit, help),
            "Unavailable. Add the System Tray widget to your panel."
        );
    }
}
