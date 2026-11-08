//! Adaptive dialogs with consistent focus, validation, and credential
//! cleanup: a port of the Python `forti_vpn/dialogs.py`.
//!
//! Ownership: the main window's dialog slot owns each dialog's `Rc` state,
//! and widget callbacks hold it weakly. Only the `closed` handlers capture
//! what their cleanup needs directly, because the window may already have
//! let go of a dialog that is still closing.
use std::cell::Cell;
use std::ffi::CStr;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use forti_vpn_core::gateway::{build_gateway, split_gateway};
use forti_vpn_core::model::{
    AuthenticationMode, CertificateKind, ConnectionState, Profile, ProfileSettings, PromptKind,
    Secret,
};
use forti_vpn_core::workflow::{Challenge, State};
use gtk::{AccessibleAnnouncementPriority, gio, glib};
use zeroize::Zeroizing;

use crate::widgets;
use crate::window::MainWindow;

const DISCONNECT_FIRST: &str = "Disconnect the VPN before saving this profile.";
/// The first choice of an existing profile's authentication and
/// certificate rows.
const KEEP: &str = "Keep existing setting";

/// An `adw::Dialog` with a header bar, a scrolling body, and a footer that
/// holds an error message and the action buttons.
#[derive(Clone)]
pub struct ContentDialog {
    pub dialog: adw::Dialog,
    pub body: gtk::Box,
    pub footer: gtk::Box,
    pub error: gtk::Label,
    pub actions: gtk::Box,
}

impl ContentDialog {
    pub(crate) fn new(title: &str, width: i32) -> Self {
        let body = widgets::vertical_box(16);
        widgets::margins(&body, 18);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(580)
            .vexpand(true)
            .child(&body)
            .build();
        let error = widgets::message_label(Some("error"));
        error.set_visible(false);
        let actions = gtk::Box::builder().spacing(8).homogeneous(true).build();
        let footer = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(18)
            .margin_end(18)
            .margin_top(12)
            .margin_bottom(18)
            .visible(false)
            .build();
        footer.append(&error);
        footer.append(&actions);
        let layout = widgets::vertical_box(0);
        layout.append(&adw::HeaderBar::new());
        layout.append(&scroll);
        layout.append(&footer);
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(width)
            .child(&layout)
            .build();
        Self {
            dialog,
            body,
            footer,
            error,
            actions,
        }
    }

    pub(crate) fn set_actions(&self, buttons: &[&gtk::Button]) {
        for button in buttons {
            self.actions.append(*button);
        }
        self.footer.set_visible(true);
    }

    /// Show `message` under the form, or hide the error when it is empty.
    pub(crate) fn show_error(&self, message: &str) {
        show_error(&self.error, message);
    }
}

pub(crate) fn show_error(label: &gtk::Label, message: &str) {
    label.set_text(message);
    label.set_visible(!message.is_empty());
    if !message.is_empty() {
        label.announce(message, AccessibleAnnouncementPriority::High);
    }
}

/// Whether an entry is empty, read without copying its text: the text may
/// be a password.
fn is_empty(entry: &adw::EntryRow) -> bool {
    entry
        .delegate()
        .and_downcast::<gtk::Text>()
        .map_or_else(|| entry.text().is_empty(), |text| text.text_length() == 0)
}

/// A FortiClient prompt (password, MFA code, …) shown while an operation
/// waits for the answer.
pub struct AuthenticationDialog {
    pub content: ContentDialog,
    pub entry: adw::EntryRow,
    pub proceed: gtk::Button,
    challenge: Challenge,
    submitted: Rc<Cell<bool>>,
}

impl AuthenticationDialog {
    pub(crate) fn new(window: &Rc<MainWindow>, challenge: Challenge) -> Rc<Self> {
        let kind = challenge.kind;
        let title = prompt_title(kind);
        let content = ContentDialog::new(title, 380);
        content
            .body
            .append(&widgets::label(prompt_instructions(kind)));
        let entry: adw::EntryRow = if kind == PromptKind::Username {
            adw::EntryRow::builder().title(title).build()
        } else {
            adw::PasswordEntryRow::builder()
                .title(title)
                .build()
                .upcast()
        };
        let group = adw::PreferencesGroup::new();
        group.add(&entry);
        content.body.append(&group);
        let proceed = widgets::button("Continue", Some("suggested-action"));
        proceed.set_sensitive(false);
        let cancel = widgets::button("Cancel", None);
        content.set_actions(&[&cancel, &proceed]);
        content.dialog.set_default_widget(Some(&proceed));
        content.dialog.set_focus(Some(&entry));
        let dialog = Rc::new(Self {
            content,
            entry,
            proceed,
            challenge,
            submitted: Rc::default(),
        });
        dialog.connect_signals(&cancel);
        dialog.connect_closed(window);
        dialog
    }

    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.content.dialog.present(Some(parent));
        // libadwaita 1.5.0 can drop the dialog's initial focus.
        self.entry.grab_focus();
    }

    pub fn close(&self) {
        self.content.dialog.close();
    }

    pub fn dialog(&self) -> &adw::Dialog {
        &self.content.dialog
    }

    fn connect_signals(self: &Rc<Self>, cancel: &gtk::Button) {
        let dialog = self.content.dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        });
        let weak = Rc::downgrade(self);
        self.proceed.connect_clicked(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.submit();
            }
        });
        let weak = Rc::downgrade(self);
        self.entry.connect_entry_activated(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.submit();
            }
        });
        let weak = Rc::downgrade(self);
        self.entry.connect_changed(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.changed();
            }
        });
    }

    /// Whatever closes the dialog, the typed text is cleared and an
    /// unanswered prompt counts as dismissed.
    fn connect_closed(&self, window: &Rc<MainWindow>) {
        let entry = self.entry.clone();
        let challenge = self.challenge.clone();
        let submitted = Rc::clone(&self.submitted);
        let window = Rc::downgrade(window);
        self.content.dialog.connect_closed(move |dialog| {
            entry.set_text("");
            if !submitted.get() {
                challenge.respond(None);
            }
            if let Some(window) = window.upgrade() {
                window.challenge_closed(dialog);
            }
        });
    }

    fn changed(&self) {
        self.proceed
            .set_sensitive(!is_empty(&self.entry) && !self.submitted.get());
        self.entry.remove_css_class("error");
        self.content.error.set_visible(false);
    }

    fn submit(&self) {
        if self.submitted.get() {
            return;
        }
        if is_empty(&self.entry) {
            self.entry.add_css_class("error");
            self.content.show_error("Enter a value to continue.");
            self.entry.grab_focus();
            return;
        }
        self.submitted.set(true);
        let value = take_secret(&self.entry);
        self.challenge.respond(Some(value));
        self.close();
    }
}

/// Move the entry's text into a [`Secret`] and clear the entry. The text is
/// borrowed from GTK and copied once, straight into memory that is wiped on
/// drop: `EditableExt::text` would leave two unwiped copies behind.
fn take_secret(entry: &adw::EntryRow) -> Secret {
    let editable = entry.upcast_ref::<gtk::Editable>();
    // SAFETY: `editable` is a live GtkEditable on the main thread, and
    // `gtk_editable_get_text` only reads it; the result is transfer none.
    let text = unsafe { gtk::ffi::gtk_editable_get_text(editable.as_ptr()) };
    let secret = if text.is_null() {
        Secret::default()
    } else {
        // SAFETY: a non-null result is the editable's own NUL-terminated
        // text, valid until the text changes, which happens only after the
        // copy below.
        let text = unsafe { CStr::from_ptr(text) };
        // GTK text is UTF-8, so this borrows rather than converts.
        Zeroizing::new(String::from_utf8_lossy(text.to_bytes()).into_owned())
    };
    entry.set_text("");
    secret
}

fn prompt_title(kind: PromptKind) -> &'static str {
    match kind {
        PromptKind::Password => "VPN password",
        PromptKind::Mfa => "Verification code",
        PromptKind::Username => "VPN username",
        PromptKind::CertificatePassword => "Client certificate password",
        PromptKind::VerifyPassword => "Confirm password",
        // FortiClient asks these only while editing, and the CLI driver
        // answers them from the editor's fields.
        PromptKind::Gateway => "Remote gateway",
        PromptKind::Authentication => "Authentication",
        PromptKind::CertificateType => "Certificate type",
        PromptKind::Certificate => "Certificate path or library",
    }
}

fn prompt_instructions(kind: PromptKind) -> &'static str {
    match kind {
        PromptKind::Mfa => "Enter the code from your authenticator.",
        PromptKind::Username => "Enter your VPN username.",
        PromptKind::CertificatePassword => "Enter the client certificate password.",
        PromptKind::VerifyPassword => "Enter the client certificate password again.",
        // The editing prompts never reach a dialog (see `prompt_title`).
        PromptKind::Password
        | PromptKind::Gateway
        | PromptKind::Authentication
        | PromptKind::CertificateType
        | PromptKind::Certificate => "Sign in to your VPN profile.",
    }
}

/// Read-only details of a profile, as FortiClient reports them.
pub(crate) fn show_profile_details(parent: &impl IsA<gtk::Widget>, profile: &Profile) {
    let content = ContentDialog::new("Profile details", 420);
    content
        .body
        .append(&widgets::styled_label(&profile.name, "title-2"));
    let group = adw::PreferencesGroup::new();
    for (key, value) in &profile.fields {
        group.add(
            &adw::ActionRow::builder()
                .title(glib::markup_escape_text(key))
                .subtitle(glib::markup_escape_text(value))
                .subtitle_selectable(true)
                .build(),
        );
    }
    content.body.append(&group);
    let close = widgets::button("Close", None);
    let dialog = content.dialog.downgrade();
    close.connect_clicked(move |_| {
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
    });
    content.set_actions(&[&close]);
    content.dialog.set_default_widget(Some(&close));
    content.dialog.set_focus(Some(&close));
    content.dialog.present(Some(parent));
    close.grab_focus();
}

/// Creates a profile (`existing` is `None`) or edits one.
pub struct ProfileEditor {
    window: Weak<MainWindow>,
    existing: Option<Profile>,
    saving: Cell<bool>,
    pub content: ContentDialog,
    pub connection: adw::PreferencesGroup,
    pub authentication: adw::PreferencesGroup,
    pub certificate: adw::PreferencesGroup,
    pub name: adw::EntryRow,
    pub gateway: adw::EntryRow,
    pub port: adw::EntryRow,
    pub auth: adw::ComboRow,
    pub username: adw::EntryRow,
    pub cert: adw::ComboRow,
    pub path: adw::EntryRow,
    pub browse: gtk::Button,
    pub cancel: gtk::Button,
    pub save_button: gtk::Button,
}

impl ProfileEditor {
    pub(crate) fn new(window: &Rc<MainWindow>, existing: Option<Profile>) -> Rc<Self> {
        let editing = existing.is_some();
        // Named, so that each editor says which profile it changes.
        let title = existing.as_ref().map_or_else(
            || "New profile".to_owned(),
            |profile| format!("Edit “{}”", profile.name),
        );
        let content = ContentDialog::new(&title, 420);
        let connection = ConnectionRows::build(existing.as_ref());
        let authentication = AuthenticationRows::build(existing.as_ref());
        let certificate = CertificateRows::build(editing);
        for section in [&connection.group, &authentication.group, &certificate.group] {
            content.body.append(section);
        }
        content.body.append(&widgets::styled_label(
            "Other settings stay managed by FortiClient.",
            "dim-label",
        ));
        let cancel = widgets::button("Cancel", None);
        let save_button = widgets::button("Save", Some("suggested-action"));
        content.set_actions(&[&cancel, &save_button]);
        content.dialog.set_default_widget(Some(&save_button));
        let editor = Rc::new(Self {
            window: Rc::downgrade(window),
            existing,
            saving: Cell::new(false),
            content,
            connection: connection.group,
            authentication: authentication.group,
            certificate: certificate.group,
            name: connection.name,
            gateway: connection.gateway,
            port: connection.port,
            auth: authentication.mode,
            username: authentication.username,
            cert: certificate.kind,
            path: certificate.path,
            browse: certificate.browse,
            cancel,
            save_button,
        });
        editor
            .content
            .dialog
            .set_focus(Some(editor.initial_focus()));
        editor.connect_signals();
        editor.connect_closed(window);
        editor
    }

    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.content.dialog.present(Some(parent));
        // libadwaita 1.5.0 can drop the dialog's initial focus.
        self.initial_focus().grab_focus();
    }

    pub fn dialog(&self) -> &adw::Dialog {
        &self.content.dialog
    }

    pub fn is_saving(&self) -> bool {
        self.saving.get()
    }

    fn initial_focus(&self) -> &adw::EntryRow {
        if self.existing.is_some() {
            &self.gateway
        } else {
            &self.name
        }
    }

    fn text_rows(&self) -> [&adw::EntryRow; 5] {
        [
            &self.name,
            &self.gateway,
            &self.port,
            &self.username,
            &self.path,
        ]
    }

    fn connect_signals(self: &Rc<Self>) {
        for row in self.text_rows() {
            let weak = Rc::downgrade(self);
            row.connect_entry_activated(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.save();
                }
            });
            row.connect_changed(|row| row.remove_css_class("error"));
        }
        let username = self.username.downgrade();
        let editing = self.existing.is_some();
        self.auth.connect_selected_notify(move |auth| {
            if let Some(username) = username.upgrade() {
                let mode = authentication_mode(auth.selected(), editing);
                username.set_sensitive(mode == AuthenticationMode::Save);
            }
        });
        self.on(&self.cert, |editor| editor.certificate_changed());
        self.on_click(&self.browse, |editor| editor.choose_certificate());
        self.on_click(&self.cancel, |editor| editor.cancel_or_close());
        self.on_click(&self.save_button, |editor| editor.save());
        let weak = Rc::downgrade(self);
        self.content.dialog.connect_close_attempt(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.cancel_or_close();
            }
        });
    }

    fn on(self: &Rc<Self>, row: &adw::ComboRow, action: fn(&Rc<Self>)) {
        let weak = Rc::downgrade(self);
        row.connect_selected_notify(move |_| {
            if let Some(editor) = weak.upgrade() {
                action(&editor);
            }
        });
    }

    fn on_click(self: &Rc<Self>, button: &gtk::Button, action: fn(&Rc<Self>)) {
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                action(&editor);
            }
        });
    }

    fn connect_closed(&self, window: &Rc<MainWindow>) {
        let window = Rc::downgrade(window);
        self.content.dialog.connect_closed(move |dialog| {
            if let Some(window) = window.upgrade() {
                window.profile_editor_closed(dialog);
            }
        });
    }

    /// The certificate kind chosen in the editor.
    fn chosen_certificate(&self) -> CertificateKind {
        certificate_kind(self.cert.selected(), self.existing.is_some())
    }

    fn certificate_changed(&self) {
        let kind = self.chosen_certificate();
        self.path.set_sensitive(kind.needs_path());
        self.browse.set_sensitive(kind.needs_path());
        self.path.set_title(match kind {
            CertificateKind::Local => "Certificate file",
            CertificateKind::Smartcard => "PKCS#11 library",
            CertificateKind::Keep | CertificateKind::Disabled => "Certificate path or library",
        });
    }

    fn choose_certificate(self: &Rc<Self>) {
        let kind = self.cert.selected();
        let local = self.chosen_certificate() == CertificateKind::Local;
        let (name, patterns): (_, &[&str]) = if local {
            (
                "PKCS#12 certificates",
                &["*.p12", "*.pfx", "*.P12", "*.PFX"],
            )
        } else {
            ("Shared libraries", &["*.so", "*.so.*"])
        };
        let preferred = file_filter(name, patterns);
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&preferred);
        filters.append(&file_filter("All files", &["*"]));
        let chooser = gtk::FileDialog::builder()
            .title(if local {
                "Choose client certificate"
            } else {
                "Choose PKCS#11 library"
            })
            .filters(&filters)
            .default_filter(&preferred)
            .build();
        let parent = self
            .window
            .upgrade()
            .map(|window| window.widgets.window.clone());
        let weak = Rc::downgrade(self);
        chooser.open(parent.as_ref(), gio::Cancellable::NONE, move |result| {
            if let Some(editor) = weak.upgrade() {
                editor.certificate_chosen(kind, result);
            }
        });
    }

    /// The file picker's result for the certificate type at index `kind`.
    pub fn certificate_chosen(&self, kind: u32, result: Result<gio::File, glib::Error>) {
        match result {
            Ok(file) => {
                let stale = self.content.dialog.root().is_none()
                    || self.saving.get()
                    || self.cert.selected() != kind;
                if stale {
                    return;
                }
                match file
                    .path()
                    .and_then(|path| path.to_str().map(str::to_owned))
                {
                    Some(path) => self.path.set_text(&path),
                    None => self
                        .content
                        .show_error("Choose a file stored on this computer."),
                }
            }
            Err(error) if error.matches(gtk::DialogError::Dismissed) => {}
            Err(_) => self
                .content
                .show_error("Could not open the file picker. Enter the path instead."),
        }
    }

    fn cancel_or_close(&self) {
        if self.saving.get() {
            if let Some(window) = self.window.upgrade() {
                window.cancel_connection();
            }
            self.cancel.set_label("Cancelling…");
            self.cancel.set_sensitive(false);
        } else {
            self.content.dialog.close();
        }
    }

    /// Validate the form and start saving it.
    pub fn save(&self) {
        if self.saving.get() {
            return;
        }
        for row in self.text_rows() {
            row.remove_css_class("error");
        }
        let original = self.existing.as_ref().map_or("", Profile::gateway);
        let gateway = match build_gateway(&self.gateway.text(), &self.port.text(), original) {
            Ok(gateway) => gateway,
            Err(error) => {
                let message = error.message();
                let row = if message.to_lowercase().contains("port") {
                    &self.port
                } else {
                    &self.gateway
                };
                return self.reject(row, message);
            }
        };
        let (server, port) = split_gateway(&gateway);
        self.gateway.set_text(&server);
        self.port.set_text(&port);
        let settings = self.settings(gateway);
        if let Err(error) = settings.validate() {
            return self.reject(self.invalid_row(&settings), error.message());
        }
        let Some(window) = self.window.upgrade() else {
            return;
        };
        if let Some(problem) = window.with_state(|state| self.save_blocker(state)) {
            return self.content.show_error(problem);
        }
        self.content.show_error("");
        self.start_saving();
        let existing = self.existing.clone();
        window.operate(|workflow| workflow.save(settings, existing));
    }

    fn reject(&self, row: &adw::EntryRow, message: &str) {
        row.add_css_class("error");
        self.content.show_error(message);
        row.grab_focus();
    }

    fn settings(&self, gateway: String) -> ProfileSettings {
        let editing = self.existing.is_some();
        ProfileSettings {
            name: self.name.text().trim().to_owned(),
            gateway,
            authentication: authentication_mode(self.auth.selected(), editing),
            username: self.username.text().into(),
            certificate_kind: certificate_kind(self.cert.selected(), editing),
            certificate: self.path.text().into(),
        }
    }

    /// The row to blame for `settings` failing validation.
    fn invalid_row(&self, settings: &ProfileSettings) -> &adw::EntryRow {
        let rows = [&self.name, &self.gateway, &self.username, &self.path];
        if let Some(row) = rows
            .into_iter()
            .find(|row| row.text().contains(['\n', '\r', '\0']))
        {
            return row;
        }
        if settings.name.is_empty() || settings.name.starts_with('-') {
            &self.name
        } else if settings.gateway.is_empty() {
            &self.gateway
        } else if settings.authentication == AuthenticationMode::Save
            && settings.username.trim().is_empty()
        {
            &self.username
        } else {
            &self.path
        }
    }

    /// Why the form cannot be saved in the current state, if it cannot.
    fn save_blocker(&self, state: &State) -> Option<&'static str> {
        if state.busy.is_some() {
            return Some("Wait for the current operation to finish, then try again.");
        }
        let disconnected = state.status.state == ConnectionState::Disconnected;
        // The connection may have changed externally while the editor was open.
        match &self.existing {
            None if disconnected => None,
            None => Some(DISCONNECT_FIRST),
            Some(existing) => match state.profiles.iter().find(|p| p.name == existing.name) {
                None => Some(
                    "This profile no longer exists. Close this dialog and create a new profile.",
                ),
                Some(current) if is_locked(state, current) => Some(DISCONNECT_FIRST),
                Some(_) => None,
            },
        }
    }

    fn start_saving(&self) {
        self.saving.set(true);
        self.content.dialog.set_can_close(false);
        self.cancel.grab_focus();
        self.set_form_sensitive(false);
        self.save_button.set_sensitive(false);
        self.save_button.set_label("Saving…");
        self.cancel.set_label("Cancel Save");
    }

    /// The save operation finished: close on success, otherwise keep the
    /// form for a retry and explain what went wrong.
    pub(crate) fn finish_save(&self, success: bool) {
        if !self.saving.replace(false) {
            return;
        }
        self.content.dialog.set_can_close(true);
        if success {
            self.content.dialog.close();
            return;
        }
        self.set_form_sensitive(true);
        self.save_button.set_label("Retry Save");
        self.save_button.set_sensitive(true);
        self.cancel.set_label("Cancel");
        self.cancel.set_sensitive(true);
        let error = self
            .window
            .upgrade()
            .map(|window| window.with_state(|state| state.error.clone()))
            .unwrap_or_default();
        self.content.show_error(&error);
        self.save_button.grab_focus();
    }

    fn set_form_sensitive(&self, sensitive: bool) {
        for section in [&self.connection, &self.authentication, &self.certificate] {
            section.set_sensitive(sensitive);
        }
    }
}

/// The editor's "Connection" section.
struct ConnectionRows {
    group: adw::PreferencesGroup,
    name: adw::EntryRow,
    gateway: adw::EntryRow,
    port: adw::EntryRow,
}

impl ConnectionRows {
    fn build(existing: Option<&Profile>) -> Self {
        let (server, port) = split_gateway(existing.map_or("", Profile::gateway));
        let name = entry_row("Profile name", existing.map_or("", |p| &p.name));
        // FortiClient cannot rename a profile.
        name.set_sensitive(existing.is_none());
        let gateway = entry_row("Remote gateway", &server);
        gateway.set_tooltip_text(Some("Server hostname or IP address"));
        let port = entry_row("Port", &port);
        port.set_input_purpose(gtk::InputPurpose::Digits);
        let group = group(
            "Connection",
            &[name.upcast_ref(), gateway.upcast_ref(), port.upcast_ref()],
        );
        Self {
            group,
            name,
            gateway,
            port,
        }
    }
}

/// The editor's "Authentication" section.
struct AuthenticationRows {
    group: adw::PreferencesGroup,
    mode: adw::ComboRow,
    username: adw::EntryRow,
}

impl AuthenticationRows {
    fn build(existing: Option<&Profile>) -> Self {
        let choices = authentication_choices(existing.is_some());
        let labels: Vec<&str> = choices
            .iter()
            .map(|&mode| authentication_label(mode))
            .collect();
        let mode = combo_row("Authentication mode", &labels);
        let username = entry_row("Username", existing.map_or("", Profile::username));
        // Only "Save username" uses it.
        username.set_sensitive(false);
        let group = group(
            "Authentication",
            &[mode.upcast_ref(), username.upcast_ref()],
        );
        Self {
            group,
            mode,
            username,
        }
    }
}

/// The editor's "Client certificate" section.
struct CertificateRows {
    group: adw::PreferencesGroup,
    kind: adw::ComboRow,
    path: adw::EntryRow,
    browse: gtk::Button,
}

impl CertificateRows {
    fn build(editing: bool) -> Self {
        let choices = certificate_choices(editing);
        let labels: Vec<&str> = choices
            .iter()
            .map(|&kind| certificate_label(kind))
            .collect();
        let kind = combo_row("Certificate type", &labels);
        let path = entry_row("Certificate path or library", "");
        path.set_sensitive(false);
        let browse =
            widgets::flat_icon_button("folder-open-symbolic", "Choose certificate or library");
        browse.set_sensitive(false);
        path.add_suffix(&browse);
        let group = group(
            "Client certificate",
            &[kind.upcast_ref(), path.upcast_ref()],
        );
        Self {
            group,
            kind,
            path,
            browse,
        }
    }
}

/// A managed profile, or the one FortiClient is (dis)connecting, cannot be
/// changed.
fn is_locked(state: &State, profile: &Profile) -> bool {
    profile.managed
        || (state.status.state != ConnectionState::Disconnected
            && state
                .status
                .profile
                .as_deref()
                .is_none_or(|active| active == profile.name))
}

/// The authentication row's choices, top to bottom. Only an existing
/// profile has a setting to keep, which comes first; a new one prompts.
fn authentication_choices(editing: bool) -> &'static [AuthenticationMode] {
    use AuthenticationMode::{Disabled, Keep, Prompt, Save};
    if editing {
        &[Keep, Prompt, Save, Disabled]
    } else {
        &[Prompt, Save, Disabled]
    }
}

/// The certificate row's choices, top to bottom. Only an existing profile
/// has a setting to keep, which comes first; a new one has none.
fn certificate_choices(editing: bool) -> &'static [CertificateKind] {
    use CertificateKind::{Disabled, Keep, Local, Smartcard};
    if editing {
        &[Keep, Local, Smartcard, Disabled]
    } else {
        &[Disabled, Local, Smartcard]
    }
}

fn authentication_label(mode: AuthenticationMode) -> &'static str {
    match mode {
        AuthenticationMode::Keep => KEEP,
        AuthenticationMode::Prompt => "Prompt for credentials",
        AuthenticationMode::Save => "Save username",
        AuthenticationMode::Disabled => "Disabled",
    }
}

fn certificate_label(kind: CertificateKind) -> &'static str {
    match kind {
        CertificateKind::Keep => KEEP,
        CertificateKind::Local => "Local certificate (.p12)",
        CertificateKind::Smartcard => "Smartcard (PKCS#11)",
        CertificateKind::Disabled => "None",
    }
}

/// The authentication mode at `index` of the editor's row.
fn authentication_mode(index: u32, editing: bool) -> AuthenticationMode {
    choice(authentication_choices(editing), index)
}

/// The certificate kind at `index` of the editor's row.
fn certificate_kind(index: u32, editing: bool) -> CertificateKind {
    choice(certificate_choices(editing), index)
}

/// The choice at `index`, or the first one when nothing is selected
/// (`GTK_INVALID_LIST_POSITION`).
fn choice<T: Copy + Default>(choices: &[T], index: u32) -> T {
    let index = usize::try_from(index).unwrap_or(usize::MAX);
    choices
        .get(index)
        .or_else(|| choices.first())
        .copied()
        .unwrap_or_default()
}

fn entry_row(title: &str, text: &str) -> adw::EntryRow {
    adw::EntryRow::builder().title(title).text(text).build()
}

fn combo_row(title: &str, choices: &[&str]) -> adw::ComboRow {
    adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(choices))
        .build()
}

fn group(title: &str, rows: &[&gtk::Widget]) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(title).build();
    for row in rows {
        group.add(*row);
    }
    group
}

fn file_filter(name: &str, patterns: &[&str]) -> gtk::FileFilter {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(name));
    for pattern in patterns {
        filter.add_pattern(pattern);
    }
    filter
}

#[cfg(test)]
mod tests {
    use super::*;
    use forti_vpn_core::model::Status;

    #[test]
    fn first_choices_keep_settings_only_when_editing() {
        use AuthenticationMode::{Disabled, Keep, Prompt, Save};
        let editing = (0..4).map(|index| authentication_mode(index, true));
        assert_eq!(editing.collect::<Vec<_>>(), [Keep, Prompt, Save, Disabled]);
        let new = (0..3).map(|index| authentication_mode(index, false));
        assert_eq!(new.collect::<Vec<_>>(), [Prompt, Save, Disabled]);
        assert_eq!(authentication_mode(u32::MAX, true), Keep);
        assert_eq!(authentication_mode(u32::MAX, false), Prompt);

        let editing = (0..4).map(|index| certificate_kind(index, true));
        assert_eq!(
            editing.collect::<Vec<_>>(),
            [
                CertificateKind::Keep,
                CertificateKind::Local,
                CertificateKind::Smartcard,
                CertificateKind::Disabled
            ]
        );
        let new = (0..3).map(|index| certificate_kind(index, false));
        assert_eq!(
            new.collect::<Vec<_>>(),
            [
                CertificateKind::Disabled,
                CertificateKind::Local,
                CertificateKind::Smartcard
            ]
        );
        assert_eq!(certificate_kind(u32::MAX, false), CertificateKind::Disabled);
    }

    #[test]
    fn choices_are_listed_once() {
        for editing in [false, true] {
            let modes = authentication_choices(editing);
            let labels: Vec<_> = modes
                .iter()
                .map(|&mode| authentication_label(mode))
                .collect();
            let kinds = certificate_choices(editing);
            let names: Vec<_> = kinds.iter().map(|&kind| certificate_label(kind)).collect();
            for list in [&labels, &names] {
                let mut unique = list.clone();
                unique.sort_unstable();
                unique.dedup();
                assert_eq!(unique.len(), list.len(), "{list:?}");
                assert_eq!(list.first() == Some(&KEEP), editing, "{list:?}");
            }
        }
    }

    #[test]
    fn profiles_in_use_or_managed_are_locked() {
        let office = Profile::new("Office VPN");
        let mut state = State {
            status: Status::new(ConnectionState::Disconnected),
            ..State::default()
        };
        assert!(!is_locked(&state, &office));
        state.status = Status::with_profile(ConnectionState::Connected, "Lab");
        assert!(!is_locked(&state, &office));
        state.status = Status::new(ConnectionState::Connecting);
        assert!(is_locked(&state, &office));
        state.status = Status::new(ConnectionState::Disconnected);
        let managed = Profile {
            managed: true,
            ..office
        };
        assert!(is_locked(&state, &managed));
    }

    #[test]
    fn prompts_have_titles_and_instructions() {
        for kind in [
            PromptKind::Password,
            PromptKind::Mfa,
            PromptKind::CertificatePassword,
            PromptKind::VerifyPassword,
        ] {
            assert!(kind.is_secret());
            assert!(!prompt_title(kind).is_empty());
        }
        assert_eq!(prompt_title(PromptKind::Mfa), "Verification code");
        let instructions = [
            (PromptKind::Password, "Sign in to your VPN profile."),
            (PromptKind::Mfa, "Enter the code from your authenticator."),
            (PromptKind::Username, "Enter your VPN username."),
            (
                PromptKind::CertificatePassword,
                "Enter the client certificate password.",
            ),
            (
                PromptKind::VerifyPassword,
                "Enter the client certificate password again.",
            ),
        ];
        for (kind, expected) in instructions {
            assert_eq!(prompt_instructions(kind), expected, "{kind:?}");
        }
    }
}
