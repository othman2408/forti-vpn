//! The main window: a port of `UtilityWindow` in the Python
//! `forti_vpn/ui.py`.
//!
//! What the window shows comes from [`presentation::main_view`]. This module
//! copies that view into widgets and performs the side effects that need
//! widget state: announcements, fades, prompts, the tray, and open dialogs.
use std::cell::{Cell, RefCell};
use std::io;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use forti_vpn_core::backend::Backend;
use forti_vpn_core::model::{ConnectionState, Profile};
use forti_vpn_core::platform::Platform;
use forti_vpn_core::preferences::{Appearance, CloseBehavior, PreferenceValues, Preferences};
use forti_vpn_core::presentation::{
    self, Badge, ButtonStyle, Control, EXIT_FAILED, ExitStep, MainInputs, MainView, NoticeKind,
    RetryAction,
};
use forti_vpn_core::workflow::{
    Busy, Challenge, Clock, Effect, Event, EventSink, Operation, State, Workflow,
};
use forti_vpn_core::{APP_ID, APP_NAME};
use gtk::{AccessibleAnnouncementPriority, gdk, gio, glib};

use crate::about;
use crate::dialogs::{self, AuthenticationDialog, ProfileEditor};
use crate::settings::SettingsDialog;
use crate::setup_dialog::SetupDialog;
use crate::tray::Tray;
use crate::widgets;

/// The window's default and minimum size: a deliberate product decision.
const WIDTH: i32 = 480;
const HEIGHT: i32 = 740;
/// On monitors shorter than this (logical pixels) the minimum height drops
/// to fit, down to [`LOWEST_MINIMUM_HEIGHT`]; the content scrolls.
const SHORT_MONITOR: i32 = 860;
const MONITOR_MARGIN: i32 = 120;
const LOWEST_MINIMUM_HEIGHT: i32 = 560;
/// Windows narrower than this show "Manage Profile" as an icon. The unit is
/// scale-independent pixels, so a larger text scaling factor (not a larger
/// interface font) also triggers it.
const NARROW_WIDTH_SP: f64 = 420.0;
const BADGE_FADE_MS: u32 = 180;
const GLOW_FADE_MS: u32 = 240;
const CONNECTED_HERO: &str = "io.github.fortivpn.Utility-connected";
const MANAGE_PROFILE: &str = "Manage Profile";
const MANAGE_ICON: &str = "view-more-symbolic";
const SET_UP_FORTICLIENT: &str = "Set Up FortiClient";
const DEMO_NOTICE: &str = "Demo mode: FortiClient is simulated; nothing on this computer changes.";

/// Why closing the window left it open. Shown in the notice card beside,
/// never instead of, the last operation's error, until it no longer
/// applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowNotice {
    /// No tray is known to show the icon, so a hidden window could not be
    /// brought back.
    NoTray,
    /// An operation is running.
    Busy,
}

impl WindowNotice {
    fn message(self, platform: &Platform) -> String {
        match self {
            Self::NoTray => format!(
                "The {} is unavailable, so the window stays open. Use Disconnect and Quit from \
                 the menu, or see Settings to set it up.",
                tray_icon(platform)
            ),
            Self::Busy => "Finish or cancel the current operation before hiding the window.".into(),
        }
    }
}

/// "top-bar icon" on GNOME and Unity, "system tray icon" elsewhere.
fn tray_icon(platform: &Platform) -> &'static str {
    if platform.tray_term() == "top bar" {
        "top-bar icon"
    } else {
        "system tray icon"
    }
}

/// What a window needs from outside: FortiClient, the system, the user's
/// settings, and how to quit.
pub struct Services {
    pub backend: Arc<dyn Backend>,
    pub platform: Platform,
    pub preferences: Preferences,
    /// Ends the application. Tests replace it to observe "Disconnect and
    /// Quit".
    pub quit: Rc<dyn Fn()>,
    /// The backend is the demo; the session log says so.
    pub demo: bool,
}

/// The widgets `render` updates.
pub struct Widgets {
    pub window: adw::ApplicationWindow,
    pub toast_overlay: adw::ToastOverlay,
    pub refresh_button: gtk::Button,
    /// The main menu; F10 opens it.
    pub menu_button: gtk::MenuButton,
    pub connection_glow: gtk::Box,
    pub connection_header: gtk::Box,
    pub hero_icon: gtk::Image,
    pub status_badge: gtk::Box,
    pub status_icon: gtk::Image,
    pub status_label: gtk::Label,
    pub spinner: gtk::Spinner,
    pub notice_box: gtk::Box,
    pub notice_icon: gtk::Image,
    pub notice_title: gtk::Label,
    pub error_label: gtk::Label,
    pub warning_label: gtk::Label,
    pub retry_button: gtk::Button,
    pub details_button: gtk::Button,
    pub setup_group: adw::PreferencesGroup,
    pub setup_button: gtk::Button,
    pub profiles_group: adw::PreferencesGroup,
    pub profile_row: adw::ComboRow,
    pub manage_button: gtk::MenuButton,
    pub new_button: gtk::Button,
    pub view_button: gtk::Button,
    pub edit_button: gtk::Button,
    pub password_button: gtk::Button,
    pub delete_button: gtk::Button,
    pub connection_summary: adw::PreferencesGroup,
    pub connected_profile: adw::ActionRow,
    pub connected_server: adw::ActionRow,
    pub operation_note: gtk::Label,
    pub credentials: adw::PreferencesGroup,
    pub username: adw::EntryRow,
    pub remember: adw::SwitchRow,
    pub primary: gtk::Button,
    pub cancel_button: gtk::Button,
    pub setup_help: gtk::Button,
    pub note: gtk::Label,
}

/// Dialogs the window keeps track of while they are open.
#[derive(Default)]
struct Dialogs {
    challenge: RefCell<Option<Rc<AuthenticationDialog>>>,
    profile: RefCell<Option<Rc<ProfileEditor>>>,
    setup: RefCell<Option<Rc<SetupDialog>>>,
    settings: RefCell<Option<Rc<SettingsDialog>>>,
    /// "Quit without disconnecting?", while it is open.
    quit: glib::WeakRef<adw::AlertDialog>,
}

pub struct MainWindow {
    pub widgets: Widgets,
    workflow: RefCell<Workflow>,
    preferences: RefCell<Preferences>,
    platform: Platform,
    quit: Rc<dyn Fn()>,
    /// Shared so that `render` can update the tray without holding a
    /// borrow while D-Bus signals go out.
    tray: RefCell<Option<Rc<Tray>>>,
    dialogs: Dialogs,
    /// Whether a tray host shows the icon: `None` until the tray (or its
    /// absence) is first reported.
    indicator: Cell<Option<bool>>,
    window_notice: Cell<Option<WindowNotice>>,
    /// The profile selector's model is being replaced.
    rendering: Cell<bool>,
    /// The first refresh after launch has not finished: nothing is known
    /// about FortiClient yet.
    first_check: Cell<bool>,
    /// "Remember password" as the user or the preferences last set it. The
    /// switch shows it unless the selected profile has a saved password.
    remember_choice: Cell<bool>,
    /// The "remember password" preference as last applied: only a change
    /// to it replaces the user's choice.
    remember_default: Cell<bool>,
    /// The selected profile has a password saved in FortiClient, as of the
    /// last render.
    saved_password: Cell<bool>,
    /// The profile whose "Update Saved Password" was the last connection
    /// attempt: "Try Again" repeats the update for it, because a plain
    /// Connect would reuse the password FortiClient has saved.
    password_retry: RefCell<Option<String>>,
    /// FortiClient answered at least once, so quitting must check the VPN.
    cli_seen: Cell<bool>,
    exit_requested: Cell<bool>,
    exit_checked: Cell<bool>,
    exiting: Cell<bool>,
    hero_connected: Cell<bool>,
    glow_visible: Cell<bool>,
    badge: Cell<Option<Badge>>,
    /// Profile names in the selector, in order.
    names: RefCell<Vec<String>>,
    badge_animation: RefCell<Option<adw::TimedAnimation>>,
    glow_animation: RefCell<Option<adw::TimedAnimation>>,
    last_toast: RefCell<Option<adw::Toast>>,
    handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
}

impl MainWindow {
    pub fn new(app: &adw::Application, services: Services) -> Rc<Self> {
        let (sender, receiver) = async_channel::unbounded();
        let sink: EventSink = Arc::new(move |event| {
            // Sending fails only once the window is gone, when events are moot.
            let _ = sender.send_blocking(event);
        });
        let workflow = Workflow::new(services.backend, sink, local_clock());
        let remember = services.preferences.values().remember_password;
        let window = Rc::new(Self {
            widgets: Widgets::build(app),
            workflow: RefCell::new(workflow),
            preferences: RefCell::new(services.preferences),
            platform: services.platform,
            quit: services.quit,
            tray: RefCell::default(),
            dialogs: Dialogs::default(),
            indicator: Cell::new(None),
            window_notice: Cell::new(None),
            rendering: Cell::new(false),
            first_check: Cell::new(true),
            remember_choice: Cell::new(remember),
            remember_default: Cell::new(remember),
            saved_password: Cell::new(false),
            password_retry: RefCell::default(),
            cli_seen: Cell::new(false),
            exit_requested: Cell::new(false),
            exit_checked: Cell::new(false),
            exiting: Cell::new(false),
            hero_connected: Cell::new(false),
            glow_visible: Cell::new(false),
            badge: Cell::new(None),
            names: RefCell::default(),
            badge_animation: RefCell::default(),
            glow_animation: RefCell::default(),
            last_toast: RefCell::default(),
            handlers: RefCell::default(),
        });
        window.connect_signals();
        window.receive_events(receiver);
        if services.demo {
            window.operate(|workflow| workflow.log(DEMO_NOTICE));
        }
        window.apply_preferences();
        window.refresh();
        // A refresh that could not start, or finished at once, has ended the
        // first check already.
        if window.first_check.get() && !window.is_refreshing() {
            window.first_check.set(false);
            window.render();
        }
        window
    }

    /// Run `action` on the workflow, then apply the effects it queued. The
    /// workflow is not borrowed while effects run, so they may call back in.
    pub fn operate<T>(self: &Rc<Self>, action: impl FnOnce(&mut Workflow) -> T) -> T {
        let (result, mut effects, refresh_finished) = {
            let mut workflow = self.workflow.borrow_mut();
            let was_refreshing = workflow.is_refreshing();
            let result = action(&mut workflow);
            let refresh_finished = was_refreshing && !workflow.is_refreshing();
            (result, workflow.take_effects(), refresh_finished)
        };
        if refresh_finished {
            // Whichever refresh it was, FortiClient has been read now.
            self.first_check.set(false);
        }
        effects.dedup_by(|later, earlier| {
            matches!((earlier, later), (Effect::Changed, Effect::Changed))
        });
        for effect in effects {
            match effect {
                Effect::Changed => self.render(),
                Effect::Prompt(challenge) => self.show_challenge(challenge),
                Effect::Completed { operation, success } => {
                    self.operation_finished(operation, success);
                }
            }
        }
        result
    }

    /// Read the workflow state. `read` must not call into GTK.
    pub fn with_state<T>(&self, read: impl FnOnce(&State) -> T) -> T {
        read(self.workflow.borrow().state())
    }

    pub fn state(&self) -> State {
        self.with_state(State::clone)
    }

    /// Change the workflow state directly, as the window does for messages
    /// that no operation produced, then show the result.
    pub fn update_state(self: &Rc<Self>, change: impl FnOnce(&mut State)) {
        change(self.workflow.borrow_mut().state_mut());
        self.render();
    }

    pub fn is_refreshing(&self) -> bool {
        self.workflow.borrow().is_refreshing()
    }

    pub fn is_closed(&self) -> bool {
        self.workflow.borrow().is_closed()
    }

    /// Whether the current operation was asked to stop.
    pub fn operation_cancelled(&self) -> bool {
        self.workflow.borrow().cancel_token().is_cancelled()
    }

    pub fn backend(&self) -> Arc<dyn Backend> {
        Arc::clone(self.workflow.borrow().backend())
    }

    pub fn platform(&self) -> &Platform {
        &self.platform
    }

    pub fn preferences(&self) -> PreferenceValues {
        self.preferences.borrow().values().clone()
    }

    /// Change and persist the preferences, then apply them. On error nothing
    /// changes.
    pub fn update_preferences(
        self: &Rc<Self>,
        write: impl FnOnce(&mut Preferences) -> io::Result<()>,
    ) -> io::Result<()> {
        let written = write(&mut self.preferences.borrow_mut());
        if written.is_ok() {
            self.apply_preferences();
        }
        written
    }

    /// Whether a tray host is known to show the icon.
    pub fn indicator_available(&self) -> bool {
        self.indicator.get() == Some(true)
    }

    /// Whether a tray host shows the icon, or `None` while that is not
    /// known yet.
    pub fn indicator(&self) -> Option<bool> {
        self.indicator.get()
    }

    pub fn exit_requested(&self) -> bool {
        self.exit_requested.get()
    }

    pub fn last_toast(&self) -> Option<adw::Toast> {
        self.last_toast.borrow().clone()
    }

    pub fn challenge_dialog(&self) -> Option<Rc<AuthenticationDialog>> {
        self.dialogs.challenge.borrow().clone()
    }

    pub fn profile_dialog(&self) -> Option<Rc<ProfileEditor>> {
        self.dialogs.profile.borrow().clone()
    }

    pub fn setup_dialog(&self) -> Option<Rc<SetupDialog>> {
        self.dialogs.setup.borrow().clone()
    }

    pub fn settings_dialog(&self) -> Option<Rc<SettingsDialog>> {
        self.dialogs.settings.borrow().clone()
    }

    pub fn present(&self) {
        self.widgets.window.present();
    }

    /// Show the window for the tray, with the panel's XDG activation token
    /// so Wayland compositors raise it.
    pub fn present_with_token(&self, token: Option<String>) {
        self.use_activation_token(token);
        self.present();
    }

    /// Open Settings for the tray, raising the window with the panel's XDG
    /// activation token like [`Self::present_with_token`].
    pub fn show_settings_with_token(self: &Rc<Self>, token: Option<String>) {
        self.use_activation_token(token);
        self.show_settings();
    }

    /// Give the next `present()` the panel's activation token; GTK uses it
    /// only when it is set before the window is presented.
    fn use_activation_token(&self, token: Option<String>) {
        if let Some(token) = token {
            self.widgets.window.set_startup_id(&token);
        }
    }

    pub fn attach_tray(self: &Rc<Self>, tray: Tray) {
        self.tray.replace(Some(Rc::new(tray)));
        self.render();
    }

    pub fn refresh(self: &Rc<Self>) {
        self.operate(Workflow::refresh);
    }

    pub fn disconnect(self: &Rc<Self>) {
        self.operate(Workflow::disconnect);
    }

    pub fn clear_diagnostics(self: &Rc<Self>) {
        self.workflow.borrow_mut().state_mut().diagnostics.clear();
    }

    fn receive_events(self: &Rc<Self>, receiver: async_channel::Receiver<Event>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(event) = receiver.recv().await {
                let Some(window) = weak.upgrade() else {
                    break;
                };
                window.operate(|workflow| workflow.handle(event));
            }
        });
    }

    fn connect_signals(self: &Rc<Self>) {
        let w = &self.widgets;
        self.on_click(&w.refresh_button, Self::refresh);
        self.on_click(&w.setup_button, Self::show_setup);
        self.on_click(&w.setup_help, Self::show_setup);
        self.on_click(&w.primary, Self::primary_action);
        self.on_click(&w.cancel_button, Self::cancel_connection);
        self.on_click(&w.retry_button, Self::retry_operation);
        self.on_click(&w.details_button, Self::show_diagnostics);
        self.on_menu_click(&w.new_button, |window| window.profile_editor(None));
        self.on_menu_click(&w.view_button, Self::view_profile);
        self.on_menu_click(&w.edit_button, Self::edit_profile);
        self.on_menu_click(&w.password_button, Self::update_saved_password);
        self.on_menu_click(&w.delete_button, Self::delete_profile);
        self.connect_inputs();
        self.connect_window_signals();
        fit_to_monitor(&w.window);
        self.connect_style_signals();
    }

    /// The profile selector and the sign-in fields.
    fn connect_inputs(self: &Rc<Self>) {
        let w = &self.widgets;
        let weak = Rc::downgrade(self);
        w.profile_row.connect_selected_notify(move |_| {
            if let Some(window) = weak.upgrade() {
                window.profile_selected();
            }
        });
        let weak = Rc::downgrade(self);
        w.remember.connect_active_notify(move |switch| {
            // A switch held on for a saved password is not a choice.
            if let Some(window) = weak.upgrade()
                && !window.saved_password.get()
            {
                window.remember_choice.set(switch.is_active());
            }
        });
        let weak = Rc::downgrade(self);
        w.username.connect_entry_activated(move |_| {
            // Enter connects, as the Connect button would.
            if let Some(window) = weak.upgrade()
                && window.widgets.primary.is_sensitive()
            {
                window.primary_action();
            }
        });
    }

    fn connect_window_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.widgets.window.connect_close_request(move |_| {
            weak.upgrade().map_or(glib::Propagation::Proceed, |window| {
                window.close_requested()
            })
        });
    }

    fn on_click(self: &Rc<Self>, button: &gtk::Button, action: fn(&Rc<Self>)) {
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                action(&window);
            }
        });
    }

    /// A "Manage Profile" menu item: close the menu, then act.
    fn on_menu_click(self: &Rc<Self>, button: &gtk::Button, action: fn(&Rc<Self>)) {
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                window.widgets.manage_button.popdown();
                action(&window);
            }
        });
    }

    /// Follow the desktop's high-contrast and reduced-motion preferences.
    fn connect_style_signals(self: &Rc<Self>) {
        let style = adw::StyleManager::default();
        let weak = Rc::downgrade(self);
        let contrast = style.connect_high_contrast_notify(move |_| {
            if let Some(window) = weak.upgrade() {
                window.render();
            }
        });
        let mut handlers = vec![(style.upcast(), contrast)];
        if let Some(settings) = gtk::Settings::default() {
            let weak = Rc::downgrade(self);
            let motion = settings.connect_gtk_enable_animations_notify(move |settings| {
                if let Some(window) = weak.upgrade()
                    && !settings.is_gtk_enable_animations()
                {
                    window.finish_animations();
                }
            });
            handlers.push((settings.upcast(), motion));
        }
        self.handlers.replace(handlers);
    }

    /// Port of `UtilityWindow.render()`: show the current state.
    pub fn render(self: &Rc<Self>) {
        let state = self.state();
        self.sync_profiles(&state);
        if state.status.state != ConnectionState::Unavailable {
            self.cli_seen.set(true);
        }
        let view = self.view(&state);
        // The launch placeholder is not news; what replaces it is.
        self.show_status(&view, !self.first_check.get());
        self.show_profile_controls(&view);
        self.show_connection(&view);
        self.show_remember(&view);
        self.show_actions(&view);
        self.show_notice(&state, &view);
        self.show_decoration(&view);
        let tray = self.tray.borrow().clone();
        if let Some(tray) = tray {
            tray.update(
                &view.tray_text,
                view.tray_disconnect_enabled,
                view.tray_connected,
            );
        }
        self.update_dialogs();
        if self.exit_requested.get() && state.busy.is_none() {
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(window) = weak.upgrade() {
                    window.advance_exit();
                }
            });
        }
        if state.busy != Some(Busy::AwaitingAuthentication) {
            self.close_challenge();
        }
    }

    /// What the window shows for `state`.
    fn view(&self, state: &State) -> MainView {
        let selected = self.selected_in(state);
        let profile_dialog_open = self.dialogs.profile.borrow().is_some();
        let close_behavior = self.preferences.borrow().values().close_behavior;
        let high_contrast = adw::StyleManager::default().is_high_contrast();
        let window_notice = self.current_window_notice(state, close_behavior);
        presentation::main_view(MainInputs {
            state,
            selected: selected.as_ref(),
            exit_requested: self.exit_requested.get(),
            profile_dialog_open,
            high_contrast,
            close_behavior,
            platform: &self.platform,
            first_check: self.first_check.get(),
            tray_missing: self.indicator.get() == Some(false),
            window_notice: window_notice.as_deref(),
        })
    }

    /// The window notice's text, after dropping a notice that no longer
    /// applies: the tray became available (or closing now quits), or the
    /// operation finished.
    fn current_window_notice(
        &self,
        state: &State,
        close_behavior: CloseBehavior,
    ) -> Option<String> {
        let notice = self.window_notice.get()?;
        let applies = match notice {
            WindowNotice::NoTray => {
                !self.indicator_available() && close_behavior == CloseBehavior::Hide
            }
            WindowNotice::Busy => state.busy.is_some(),
        };
        if !applies {
            self.window_notice.set(None);
        }
        applies.then(|| notice.message(&self.platform))
    }

    /// Replace the selector's model when the profile list changed, keeping
    /// the selection (or the last profile used) where possible.
    fn sync_profiles(&self, state: &State) {
        let names: Vec<String> = state.profiles.iter().map(|p| p.name.clone()).collect();
        if *self.names.borrow() == names {
            return;
        }
        let wanted = self
            .selected_name()
            .unwrap_or_else(|| self.preferences.borrow().values().last_profile.clone());
        let position = names.iter().position(|name| *name == wanted).unwrap_or(0);
        let model = gtk::StringList::new(&names.iter().map(String::as_str).collect::<Vec<_>>());
        self.rendering.set(true);
        self.widgets.profile_row.set_model(Some(&model));
        self.widgets
            .profile_row
            .set_selected(u32::try_from(position).unwrap_or(u32::MAX));
        self.rendering.set(false);
        self.names.replace(names);
        let username = self
            .selected_in(state)
            .map(|profile| profile.username().to_owned())
            .unwrap_or_default();
        self.widgets.username.set_text(&username);
    }

    /// The name selected in the selector's current model.
    fn selected_name(&self) -> Option<String> {
        let index = usize::try_from(self.widgets.profile_row.selected()).ok()?;
        self.names.borrow().get(index).cloned()
    }

    fn selected_in(&self, state: &State) -> Option<Profile> {
        let index = usize::try_from(self.widgets.profile_row.selected()).ok()?;
        state.profiles.get(index).cloned()
    }

    /// The profile chosen in the selector.
    pub fn selected(&self) -> Option<Profile> {
        let index = usize::try_from(self.widgets.profile_row.selected()).ok()?;
        self.with_state(|state| state.profiles.get(index).cloned())
    }

    /// Show the status badge; a changed status is announced if `announce`.
    fn show_status(&self, view: &MainView, announce: bool) {
        let w = &self.widgets;
        if view.hero_connected != self.hero_connected.replace(view.hero_connected) {
            let (icon, label) = if view.hero_connected {
                (CONNECTED_HERO, "Forti VPN connected")
            } else {
                (APP_ID, APP_NAME)
            };
            w.hero_icon.set_icon_name(Some(icon));
            w.hero_icon
                .update_property(&[gtk::accessible::Property::Label(label)]);
        }
        if w.status_label.text() != view.status_text {
            w.status_label.set_text(&view.status_text);
            w.status_badge.set_opacity(0.85);
            fade(&w.status_badge, 1.0, &self.badge_animation, BADGE_FADE_MS);
            if announce {
                w.status_label
                    .announce(&view.status_text, AccessibleAnnouncementPriority::Medium);
            }
        }
        w.spinner.set_spinning(view.spinner_active);
        w.spinner.set_visible(view.spinner_active);
        if let Some(previous) = self.badge.replace(Some(view.badge)) {
            w.status_badge.remove_css_class(previous.css_class());
        }
        w.status_badge.add_css_class(view.badge.css_class());
        w.status_icon.set_icon_name(Some(view.badge.icon_name()));
        w.status_icon.set_visible(view.status_icon_visible);
    }

    fn show_profile_controls(&self, view: &MainView) {
        let w = &self.widgets;
        let hint = view.profiles_hint.as_deref().map(glib::markup_escape_text);
        w.profiles_group.set_description(hint.as_deref());
        w.profile_row.set_sensitive(view.profile_selector_sensitive);
        w.refresh_button.set_sensitive(view.refresh_sensitive);
        w.manage_button.set_sensitive(view.manage_sensitive);
        show_control(&w.new_button, &view.new_profile, false);
        show_control(&w.view_button, &view.view_profile, false);
        show_control(&w.edit_button, &view.edit_profile, true);
        show_control(&w.delete_button, &view.delete_profile, true);
        show_control(&w.password_button, &view.update_password, true);
    }

    fn show_connection(&self, view: &MainView) {
        let w = &self.widgets;
        w.connection_summary.set_visible(view.summary_visible);
        w.connected_profile
            .set_subtitle(&glib::markup_escape_text(&view.summary_profile));
        w.connected_server
            .set_subtitle(&glib::markup_escape_text(&view.summary_server));
        w.credentials.set_visible(view.credentials_visible);
        w.username.set_sensitive(view.credentials_sensitive);
        let note = view.operation_note.as_deref();
        w.operation_note.set_text(note.unwrap_or_default());
        w.operation_note.set_visible(note.is_some());
    }

    /// "Remember password" is held on for a password FortiClient already
    /// saved: replacing it is "Update Saved Password". Otherwise it shows the
    /// user's choice.
    fn show_remember(&self, view: &MainView) {
        let remember = &self.widgets.remember;
        // Set first, so the switch's handler does not take the held state
        // for a choice.
        self.saved_password.set(view.saved_password);
        remember.set_active(view.saved_password || self.remember_choice.get());
        remember.set_sensitive(view.credentials_sensitive && !view.saved_password);
        remember.set_subtitle(&glib::markup_escape_text(&view.remember_subtitle));
    }

    /// What Connect and "Try Again" ask FortiClient: with a saved password
    /// it is used, so nothing is remembered anew and no prompt appears.
    fn remember_password(&self) -> bool {
        !self.saved_password.get() && self.widgets.remember.is_active()
    }

    fn show_actions(&self, view: &MainView) {
        let w = &self.widgets;
        w.setup_group.set_visible(view.setup_group_visible);
        w.profiles_group.set_visible(view.profiles_group_visible);
        w.setup_help.set_visible(view.setup_help_visible);
        show_button(&w.primary, &view.primary);
        describe(&w.primary, view.primary_hint.as_deref());
        for style in [ButtonStyle::Suggested, ButtonStyle::Destructive] {
            widgets::set_css_class(&w.primary, style.css_class(), style == view.primary_style);
        }
        show_button(&w.cancel_button, &view.cancel);
        w.note.set_text(&view.footer_note);
        w.note.set_visible(view.footer_note_visible);
    }

    fn show_notice(&self, state: &State, view: &MainView) {
        let w = &self.widgets;
        let previous_error = w.error_label.text();
        w.error_label.set_text(&state.error);
        if !state.error.is_empty() && state.error != previous_error {
            w.error_label
                .announce(&state.error, AccessibleAnnouncementPriority::High);
        }
        w.error_label.set_visible(view.error.is_some());
        let warning = view.warning.as_deref().unwrap_or_default();
        let previous_warning = w.warning_label.text();
        w.warning_label.set_text(warning);
        w.warning_label.set_visible(view.warning.is_some());
        if !warning.is_empty() && warning != previous_warning {
            w.warning_label
                .announce(warning, AccessibleAnnouncementPriority::Medium);
        }
        w.notice_box.set_visible(view.notice.is_some());
        if let Some((title, kind)) = &view.notice {
            w.notice_title.set_text(title);
            for widget in [
                w.notice_icon.upcast_ref::<gtk::Widget>(),
                w.notice_title.upcast_ref(),
            ] {
                for style in [NoticeKind::Error, NoticeKind::Warning] {
                    widgets::set_css_class(widget, style.css_class(), style == *kind);
                }
            }
        }
        w.retry_button.set_visible(view.retry_visible);
        w.retry_button.set_sensitive(view.retry_sensitive);
        w.retry_button
            .set_label(view.retry.map_or("Try Again", RetryAction::label));
    }

    fn show_decoration(&self, view: &MainView) {
        let w = &self.widgets;
        widgets::set_css_class(&w.connection_header, "high-contrast", view.high_contrast);
        for surface in [&w.connection_header, &w.connection_glow] {
            widgets::set_css_class(surface, "is-connected", view.connected_decoration);
        }
        if view.high_contrast {
            self.glow_visible.set(false);
            // Not borrowed while it runs: pausing calls back into GTK.
            let animation = self.glow_animation.borrow().clone();
            if let Some(animation) = animation {
                animation.pause();
            }
            w.connection_glow.set_opacity(0.0);
        } else if view.connected_decoration != self.glow_visible.get() {
            self.glow_visible.set(view.connected_decoration);
            let opacity = if view.connected_decoration { 1.0 } else { 0.0 };
            fade(
                &w.connection_glow,
                opacity,
                &self.glow_animation,
                GLOW_FADE_MS,
            );
        }
    }

    /// Jump fades to their end once the desktop asks for reduced motion.
    fn finish_animations(&self) {
        for slot in [&self.badge_animation, &self.glow_animation] {
            // Not borrowed while it runs: skipping runs the animation's
            // callbacks.
            let animation = slot.borrow().clone();
            if let Some(animation) = animation {
                animation.skip();
            }
        }
    }

    fn update_dialogs(&self) {
        if let Some(setup) = self.setup_dialog() {
            setup.update();
        }
        if let Some(settings) = self.settings_dialog() {
            settings.update();
        }
    }

    fn profile_selected(self: &Rc<Self>) {
        if self.rendering.get() {
            return;
        }
        // A failed password update was for the profile selected before.
        self.password_retry.replace(None);
        if let Some(profile) = self.selected() {
            self.widgets.username.set_text(profile.username());
            let remember = self.preferences.borrow().values().remember_password;
            // `render` below shows it.
            self.remember_choice.set(remember);
            let saved = self.preferences.borrow_mut().select_profile(&profile.name);
            if saved.is_err() {
                self.show_toast("Could not save your last selected profile.");
            }
        }
        self.render();
    }

    pub fn primary_action(self: &Rc<Self>) {
        let (busy, status) = self.with_state(|state| (state.busy, state.status.state));
        if busy.is_some() {
            return;
        }
        match (status, self.selected()) {
            (ConnectionState::Connected, _) => self.disconnect(),
            (ConnectionState::Disconnected, Some(profile)) => {
                let username = self.widgets.username.text().into();
                let remember = self.remember_password();
                self.password_retry.replace(None);
                self.operate(|workflow| workflow.connect(profile, username, remember));
            }
            _ => self.refresh(),
        }
    }

    /// Cancel the running operation, or an attempt FortiClient still
    /// reports as pending.
    pub fn cancel_connection(self: &Rc<Self>) {
        let pending = self.with_state(|state| {
            state.busy.is_none()
                && matches!(
                    state.status.state,
                    ConnectionState::Connecting | ConnectionState::AwaitingAuthentication
                )
        });
        if pending {
            self.disconnect();
        } else {
            self.operate(|workflow| workflow.cancel());
        }
        self.close_challenge();
    }

    fn retry_operation(self: &Rc<Self>) {
        let state = self.state();
        if state.busy.is_some() || self.exit_requested.get() || state.error.is_empty() {
            return;
        }
        let selected = self.selected_in(&state);
        match presentation::retry_choice(&state, selected.is_some()) {
            Some(RetryAction::TryAgain) => match selected {
                Some(profile) if self.retries_password_update(&profile) => {
                    let username = self.update_username(&profile);
                    self.save_password(&profile, &username);
                }
                _ => self.primary_action(),
            },
            Some(RetryAction::RetryDisconnect) => self.disconnect(),
            Some(RetryAction::Refresh | RetryAction::CheckAgain) => self.refresh(),
            Some(RetryAction::ManageProfiles) => self.widgets.manage_button.popup(),
            None => {}
        }
    }

    /// Whether the last connection attempt updated `profile`'s saved
    /// password, so that "Try Again" must repeat the update.
    fn retries_password_update(&self, profile: &Profile) -> bool {
        self.password_retry.borrow().as_deref() == Some(profile.name.as_str())
    }

    pub fn show_toast(&self, title: &str) {
        if let Some(previous) = self.last_toast.take() {
            previous.dismiss();
        }
        let toast = adw::Toast::builder().title(title).timeout(3).build();
        self.widgets.toast_overlay.add_toast(toast.clone());
        self.last_toast.replace(Some(toast));
    }

    fn operation_finished(self: &Rc<Self>, operation: Operation, success: bool) {
        if operation == Operation::SavingProfile
            && let Some(editor) = self.profile_dialog()
        {
            editor.finish_save(success);
        }
        if success {
            match operation {
                Operation::SavingProfile => self.show_toast("Profile saved"),
                Operation::RemovingProfile => self.show_toast("Profile deleted"),
                _ => {}
            }
        }
    }

    /// Ask for a prompt's answer, replacing any prompt still open. The
    /// window comes to the front first, so that the prompt is never hidden
    /// behind other windows while the operation waits for it.
    pub fn show_challenge(self: &Rc<Self>, challenge: Challenge) {
        self.close_challenge();
        self.present();
        let dialog = AuthenticationDialog::new(self, challenge);
        self.dialogs.challenge.replace(Some(Rc::clone(&dialog)));
        dialog.present(&self.widgets.window);
    }

    fn close_challenge(&self) {
        let dialog = self.dialogs.challenge.take();
        if let Some(dialog) = dialog {
            dialog.close();
        }
    }

    pub(crate) fn challenge_closed(&self, dialog: &adw::Dialog) {
        let current = self.challenge_dialog();
        if current.is_some_and(|current| current.dialog() == dialog) {
            self.dialogs.challenge.replace(None);
        }
    }

    pub fn view_profile(self: &Rc<Self>) {
        if let Some(profile) = self.selected() {
            dialogs::show_profile_details(&self.widgets.window, &profile);
        }
    }

    fn edit_profile(self: &Rc<Self>) {
        if let Some(profile) = self.selected() {
            self.profile_editor(Some(profile));
        }
    }

    /// Open the profile editor, or bring back the one already open. Built on
    /// demand, so each opening shows FortiClient's latest values.
    pub fn profile_editor(self: &Rc<Self>, existing: Option<Profile>) {
        let open = self.profile_dialog();
        let editor = open.unwrap_or_else(|| {
            let editor = ProfileEditor::new(self, existing);
            self.dialogs.profile.replace(Some(Rc::clone(&editor)));
            // The notice's retry button hides while the editor is open.
            self.render();
            editor
        });
        editor.present(&self.widgets.window);
    }

    pub(crate) fn profile_editor_closed(self: &Rc<Self>, dialog: &adw::Dialog) {
        let Some(editor) = self.profile_dialog() else {
            return;
        };
        if editor.dialog() != dialog {
            return;
        }
        self.dialogs.profile.replace(None);
        // libadwaita 1.5.0 can close a dialog that refuses to close, so a
        // save still running must not outlive its form.
        if editor.is_saving() {
            self.cancel_connection();
        }
        if !self.is_closed() {
            self.render();
        }
    }

    pub fn update_saved_password(self: &Rc<Self>) {
        let Some(profile) = self.selected() else {
            return;
        };
        let ready = self.with_state(|state| {
            state.busy.is_none() && state.status.state == ConnectionState::Disconnected
        });
        if !ready {
            return;
        }
        let username = self.update_username(&profile);
        let account = if username.is_empty() {
            String::new()
        } else {
            format!(" as “{username}”")
        };
        let body = format!(
            "Connect to “{}”{account} and save the password you enter in FortiClient. MFA may \
             be required. The VPN will stay connected.",
            profile.name
        );
        let dialog = alert("Update saved password?", &body, "save", "Connect and Save");
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if response == "save"
                && let Some(window) = weak.upgrade()
            {
                window.save_password(&profile, &username);
            }
        });
        dialog.present(Some(&self.widgets.window));
    }

    /// The account "Update Saved Password" signs in with: the username
    /// typed in the window, else the profile's own.
    fn update_username(&self, profile: &Profile) -> String {
        let typed = self.widgets.username.text();
        match typed.trim() {
            "" => profile.username().to_owned(),
            typed => typed.to_owned(),
        }
    }

    fn save_password(self: &Rc<Self>, profile: &Profile, username: &str) {
        let ready = self.with_state(|state| {
            state.busy.is_none()
                && state.status.state == ConnectionState::Disconnected
                && state.profiles.iter().any(|item| item.name == profile.name)
        });
        if !ready {
            self.show_toast(
                "Refresh profiles and disconnect the VPN before updating the saved password.",
            );
            return;
        }
        // Remember=true forces a fresh password prompt, even for saved
        // profiles. Credentials still go only through the PTY prompt flow.
        self.password_retry.replace(Some(profile.name.clone()));
        let (profile, username) = (profile.clone(), username.to_owned());
        self.operate(|workflow| workflow.connect(profile, username, true));
    }

    fn delete_profile(self: &Rc<Self>) {
        let Some(profile) = self.selected() else {
            return;
        };
        let body = format!(
            "“{}” will be deleted from FortiClient. This cannot be undone.",
            profile.name
        );
        let dialog = alert("Delete this profile?", &body, "delete", "Delete");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if response == "delete"
                && let Some(window) = weak.upgrade()
            {
                let profile = profile.clone();
                window.operate(|workflow| workflow.remove(profile));
            }
        });
        dialog.present(Some(&self.widgets.window));
    }

    pub fn apply_preferences(self: &Rc<Self>) {
        let values = self.preferences();
        adw::StyleManager::default().set_color_scheme(color_scheme(values.appearance));
        // A new default replaces the switch's state; other settings keep
        // what the user chose.
        let remember = values.remember_password;
        if self.remember_default.replace(remember) != remember {
            self.remember_choice.set(remember);
        }
        // The footer note follows the close behavior, and the switch the
        // choice.
        self.render();
    }

    pub fn show_setup(self: &Rc<Self>) {
        self.present();
        let open = self.setup_dialog();
        let dialog = open.unwrap_or_else(|| {
            let dialog = SetupDialog::new(self);
            self.dialogs.setup.replace(Some(Rc::clone(&dialog)));
            self.refresh();
            dialog
        });
        dialog.present(&self.widgets.window);
    }

    pub(crate) fn setup_closed(&self, dialog: &adw::Dialog) {
        if self
            .setup_dialog()
            .is_some_and(|setup| setup.content.dialog == *dialog)
        {
            self.dialogs.setup.replace(None);
        }
    }

    pub fn show_settings(self: &Rc<Self>) {
        self.present();
        let open = self.settings_dialog();
        let settings = open.unwrap_or_else(|| {
            let settings = SettingsDialog::new(self);
            self.dialogs.settings.replace(Some(Rc::clone(&settings)));
            settings
        });
        settings.present(&self.widgets.window);
    }

    pub(crate) fn settings_closed(&self, dialog: &adw::Dialog) {
        if self
            .settings_dialog()
            .is_some_and(|settings| settings.dialog.upcast_ref::<adw::Dialog>() == dialog)
        {
            self.dialogs.settings.replace(None);
        }
    }

    pub fn show_diagnostics(self: &Rc<Self>) {
        self.show_settings();
        if let Some(settings) = self.settings_dialog() {
            settings.show_diagnostics();
        }
    }

    pub fn show_about(self: &Rc<Self>) {
        about::show_about(self);
    }

    /// Whether a tray host shows the icon: reported by the tray whenever a
    /// host appears or disappears, and once by the application when there
    /// is no tray at all.
    pub fn indicator_changed(self: &Rc<Self>, available: bool) {
        self.indicator.set(Some(available));
        if !available && !self.widgets.window.is_visible() {
            self.present();
        }
        // The footer note, the window notice, and Settings follow it.
        self.render();
    }

    /// Hide the window in the tray, quit, or say why the window stays open.
    ///
    /// libadwaita's dialog host answers a close request before this
    /// handler: while any dialog is open, it closes the topmost one
    /// instead, so no dialog is open here. Settings or About thus close on
    /// one request and the window hides on the next; an editor that is
    /// saving takes the request as Cancel, as it does Escape.
    fn close_requested(self: &Rc<Self>) -> glib::Propagation {
        if self.exiting.get() {
            return glib::Propagation::Proceed;
        }
        let close_behavior = self.preferences.borrow().values().close_behavior;
        if close_behavior == CloseBehavior::Quit {
            self.request_exit();
        } else if let Some(notice) = self.hide_blocker() {
            self.window_notice.set(Some(notice));
            self.render();
        } else {
            self.widgets.window.set_visible(false);
        }
        glib::Propagation::Stop
    }

    /// Why closing must not hide the window, if it must not. Anything but a
    /// tray known to show the icon counts as none: the window could not be
    /// brought back.
    fn hide_blocker(&self) -> Option<WindowNotice> {
        if !self.indicator_available() {
            Some(WindowNotice::NoTray)
        } else if self.with_state(|state| state.busy.is_some()) {
            Some(WindowNotice::Busy)
        } else {
            None
        }
    }

    fn open_dialogs(&self) -> Vec<adw::Dialog> {
        let open = self.widgets.window.dialogs();
        (0..open.n_items())
            .filter_map(|index| open.item(index).and_downcast())
            .collect()
    }

    /// "Disconnect and Quit": disconnect if needed, then quit only once
    /// FortiClient confirms the VPN is down.
    pub fn request_exit(self: &Rc<Self>) {
        let setup_required = self.with_state(|state| state.setup_required);
        if setup_required && !self.cli_seen.get() {
            // A fresh setup screen never had a usable CLI or started a
            // connection.
            self.quit_now();
            return;
        }
        if self.exit_requested.replace(true) {
            return;
        }
        self.exit_checked.set(false);
        self.present();
        self.cancel_connection();
        self.advance_exit();
    }

    fn advance_exit(self: &Rc<Self>) {
        if self.exiting.get() || !self.exit_requested.get() {
            return;
        }
        let (checked, cli_seen) = (self.exit_checked.get(), self.cli_seen.get());
        match self.with_state(|state| presentation::exit_step(state, checked, cli_seen)) {
            ExitStep::Wait => {}
            ExitStep::Check => {
                self.exit_checked.set(true);
                self.operate(Workflow::check_before_exit);
            }
            ExitStep::Quit => self.quit_now(),
            ExitStep::Disconnect => self.disconnect(),
            ExitStep::Fail => self.exit_failed(),
        }
    }

    fn quit_now(&self) {
        self.exiting.set(true);
        (self.quit)();
    }

    /// Stay open and say why. When FortiClient stopped answering, the VPN
    /// can be neither checked nor ended, so the user may quit regardless.
    fn exit_failed(self: &Rc<Self>) {
        self.exit_requested.set(false);
        let unavailable =
            self.with_state(|state| state.status.state == ConnectionState::Unavailable);
        self.update_state(|state| state.error = EXIT_FAILED.to_owned());
        if unavailable {
            self.offer_to_quit_anyway();
        }
    }

    fn offer_to_quit_anyway(self: &Rc<Self>) {
        // Asked again, it is still the same question.
        let open = self.dialogs.quit.upgrade();
        if open.is_some_and(|alert| alert.root().is_some()) {
            return;
        }
        let dialog = alert(
            "Quit without disconnecting?",
            "FortiClient is not responding, so Forti VPN cannot check or end the VPN connection.",
            "quit",
            "Quit Anyway",
        );
        dialog.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if response == "quit"
                && let Some(window) = weak.upgrade()
            {
                window.quit_now();
            }
        });
        self.dialogs.quit.set(Some(&dialog));
        dialog.present(Some(&self.widgets.window));
    }

    /// Stop the workflow (an established VPN stays up), close dialogs and
    /// the tray, and destroy the window.
    pub fn shutdown(self: &Rc<Self>) {
        self.operate(Workflow::close);
        if let Some(tray) = self.tray.take() {
            tray.close();
        }
        for (object, handler) in self.handlers.take() {
            object.disconnect(handler);
        }
        for dialog in self.open_dialogs() {
            dialog.force_close();
        }
        for slot in [&self.badge_animation, &self.glow_animation] {
            if let Some(animation) = slot.take() {
                animation.pause();
            }
        }
        self.exiting.set(true);
        self.widgets.window.destroy();
    }
}

impl Widgets {
    fn build(app: &adw::Application) -> Self {
        let refresh_button = refresh_button();
        let menu_button = app_menu_button();
        let heading = Heading::build();
        let notice = Notice::build();
        let (setup_group, setup_button) = setup_group();
        let profiles = Profiles::build();
        let summary = Summary::build();
        let operation_note = widgets::centered_label("", Some("dim-label"));
        let credentials = Credentials::build();
        let primary = main_button("Connect", Some("suggested-action"));
        let cancel_button = main_button("Cancel", None);
        cancel_button.set_visible(false);
        let setup_help = widgets::button(SET_UP_FORTICLIENT, None);
        setup_help.set_halign(gtk::Align::Start);
        setup_help.set_visible(false);
        let note = widgets::styled_label(
            "Closing this window keeps an established VPN connection running.",
            "dim-label",
        );
        let content = widgets::vertical_box(16);
        widgets::margins(&content, 18);
        for section in [
            heading.container.upcast_ref::<gtk::Widget>(),
            notice.container.upcast_ref(),
            setup_group.upcast_ref(),
            profiles.group.upcast_ref(),
            summary.group.upcast_ref(),
            operation_note.upcast_ref(),
            credentials.group.upcast_ref(),
            primary.upcast_ref(),
            cancel_button.upcast_ref(),
            setup_help.upcast_ref(),
            note.upcast_ref(),
        ] {
            content.append(section);
        }
        let connection_glow = connection_glow();
        let toast_overlay = adw::ToastOverlay::new();
        let header = header_bar(&refresh_button, &menu_button);
        toast_overlay.set_child(Some(&frame(&header, &connection_glow, &content)));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(APP_NAME)
            .default_width(WIDTH)
            .default_height(HEIGHT)
            .content(&toast_overlay)
            .build();
        window.set_size_request(WIDTH, HEIGHT);
        compact_when_narrow(&window, &profiles.manage);
        Self {
            window,
            toast_overlay,
            refresh_button,
            menu_button,
            connection_glow,
            connection_header: heading.container,
            hero_icon: heading.hero_icon,
            status_badge: heading.status_badge,
            status_icon: heading.status_icon,
            status_label: heading.status_label,
            spinner: heading.spinner,
            notice_box: notice.container,
            notice_icon: notice.icon,
            notice_title: notice.title,
            error_label: notice.error,
            warning_label: notice.warning,
            retry_button: notice.retry,
            details_button: notice.details,
            setup_group,
            setup_button,
            profiles_group: profiles.group,
            profile_row: profiles.row,
            manage_button: profiles.manage,
            new_button: profiles.new,
            view_button: profiles.view,
            edit_button: profiles.edit,
            password_button: profiles.password,
            delete_button: profiles.delete,
            connection_summary: summary.group,
            connected_profile: summary.profile,
            connected_server: summary.server,
            operation_note,
            credentials: credentials.group,
            username: credentials.username,
            remember: credentials.remember,
            primary,
            cancel_button,
            setup_help,
            note,
        }
    }
}

fn refresh_button() -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Refresh profiles and status")
        .build();
    button.update_property(&[gtk::accessible::Property::Label(
        "Refresh profiles and status",
    )]);
    button
}

/// The decoration behind the heading while connected.
fn connection_glow() -> gtk::Box {
    gtk::Box::builder()
        .valign(gtk::Align::Start)
        .height_request(340)
        .can_target(false)
        .css_classes(["connection-glow"])
        .opacity(0.0)
        .build()
}

/// Refresh at the start, the main menu at the end.
fn header_bar(refresh_button: &gtk::Button, menu_button: &gtk::MenuButton) -> adw::HeaderBar {
    let header = adw::HeaderBar::new();
    header.pack_start(refresh_button);
    header.pack_end(menu_button);
    header
}

/// The header bar above the scrolling content.
fn frame(header: &adw::HeaderBar, glow: &gtk::Box, content: &gtk::Box) -> gtk::Box {
    let root = widgets::vertical_box(0);
    root.append(header);
    root.append(&backdrop(glow, content));
    root
}

/// The main menu, which F10 opens.
fn app_menu_button() -> gtk::MenuButton {
    let menu = gio::Menu::new();
    menu.append(Some("Settings"), Some("app.settings"));
    menu.append(Some(SET_UP_FORTICLIENT), Some("app.setup"));
    menu.append(Some(&format!("About {APP_NAME}")), Some("app.about"));
    menu.append(Some("Disconnect and Quit"), Some("app.quit"));
    let button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .primary(true)
        .tooltip_text("Application menu")
        .build();
    button.update_property(&[gtk::accessible::Property::Label("Application menu")]);
    button
}

/// The scrolling content over a decorative glow that does not scroll.
fn backdrop(glow: &gtk::Box, content: &gtk::Box) -> gtk::Overlay {
    let clamp = adw::Clamp::builder()
        .maximum_size(560)
        .child(content)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();
    let backdrop = gtk::Overlay::builder().vexpand(true).child(glow).build();
    backdrop.add_overlay(&scroll);
    backdrop.set_measure_overlay(&scroll, true);
    backdrop
}

/// The app icon, title, and status badge.
struct Heading {
    container: gtk::Box,
    hero_icon: gtk::Image,
    status_badge: gtk::Box,
    status_icon: gtk::Image,
    status_label: gtk::Label,
    spinner: gtk::Spinner,
}

impl Heading {
    fn build() -> Self {
        let hero_icon = gtk::Image::builder()
            .icon_name(APP_ID)
            .pixel_size(80)
            .build();
        hero_icon.update_property(&[gtk::accessible::Property::Label(APP_NAME)]);
        let status_icon = gtk::Image::builder()
            .icon_name("network-offline-symbolic")
            .pixel_size(16)
            .build();
        // What the first render shows too, so it is not announced as news.
        let status_label = widgets::centered_label(presentation::CHECKING, None);
        status_label.set_max_width_chars(28);
        let spinner = gtk::Spinner::new();
        let status_badge = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::Center)
            .css_classes(["status-badge"])
            .build();
        status_badge.append(&status_icon);
        status_badge.append(&status_label);
        status_badge.append(&spinner);
        let container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(8)
            .margin_bottom(8)
            .css_classes(["connection-header"])
            .build();
        container.append(&hero_icon);
        container.append(&widgets::centered_label(APP_NAME, Some("title-2")));
        container.append(&status_badge);
        Self {
            container,
            hero_icon,
            status_badge,
            status_icon,
            status_label,
            spinner,
        }
    }
}

/// The card that explains an error or warning.
struct Notice {
    container: gtk::Box,
    icon: gtk::Image,
    title: gtk::Label,
    error: gtk::Label,
    warning: gtk::Label,
    retry: gtk::Button,
    details: gtk::Button,
}

impl Notice {
    fn build() -> Self {
        let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
        let title = widgets::styled_label("", "heading");
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        heading.append(&icon);
        heading.append(&title);
        let error = widgets::message_label(None);
        let warning = widgets::message_label(None);
        let retry = widgets::button("Try Again", None);
        retry.set_visible(false);
        let details = widgets::button("View Diagnostics", Some("flat"));
        let actions = widgets::vertical_box(8);
        actions.set_halign(gtk::Align::Start);
        actions.append(&retry);
        actions.append(&details);
        let container = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .css_classes(["card", "operation-notice"])
            .visible(false)
            .build();
        container.append(&heading);
        container.append(&error);
        container.append(&warning);
        container.append(&actions);
        Self {
            container,
            icon,
            title,
            error,
            warning,
            retry,
            details,
        }
    }
}

fn setup_group() -> (adw::PreferencesGroup, gtk::Button) {
    let group = adw::PreferencesGroup::builder()
        .title("Get started")
        .description("Install FortiClient to use VPN connections and profiles.")
        .visible(false)
        .build();
    let button = widgets::button(SET_UP_FORTICLIENT, Some("suggested-action"));
    group.add(&button);
    (group, button)
}

/// The profile selector and its "Manage Profile" menu.
struct Profiles {
    group: adw::PreferencesGroup,
    row: adw::ComboRow,
    manage: gtk::MenuButton,
    new: gtk::Button,
    view: gtk::Button,
    edit: gtk::Button,
    password: gtk::Button,
    delete: gtk::Button,
}

impl Profiles {
    fn build() -> Self {
        let row = adw::ComboRow::builder()
            .title("Profile")
            .model(&gtk::StringList::new(&[]))
            .build();
        let new = menu_item("New", "list-add-symbolic");
        let view = menu_item("View", "dialog-information-symbolic");
        let edit = menu_item("Edit", "document-edit-symbolic");
        let password = menu_item("Update Saved Password", "dialog-password-symbolic");
        password.update_property(&[gtk::accessible::Property::Label("Update saved password")]);
        let delete = menu_item("Delete", "user-trash-symbolic");
        let items = gtk::Box::new(gtk::Orientation::Vertical, 2);
        for item in [&new, &view, &edit, &password, &delete] {
            items.append(item);
        }
        let manage = gtk::MenuButton::builder()
            .label(MANAGE_PROFILE)
            .tooltip_text(MANAGE_PROFILE)
            // Not as tall as the group's description, when it has one.
            .valign(gtk::Align::Center)
            .popover(&gtk::Popover::builder().child(&items).build())
            .build();
        manage.update_property(&[gtk::accessible::Property::Label(MANAGE_PROFILE)]);
        let group = adw::PreferencesGroup::builder()
            .title("VPN profile")
            .header_suffix(&manage)
            .build();
        group.add(&row);
        Self {
            group,
            row,
            manage,
            new,
            view,
            edit,
            password,
            delete,
        }
    }
}

fn menu_item(text: &str, icon: &str) -> gtk::Button {
    let item = widgets::icon_button(text, icon, &format!("{text} profile"));
    item.add_css_class("flat");
    if let Some(content) = item.child() {
        content.set_halign(gtk::Align::Start);
    }
    item
}

/// In narrow windows, or with a larger text scaling factor, "Manage Profile"
/// shows only an icon so that the "VPN profile" title beside it is not cut
/// off.
fn compact_when_narrow(window: &adw::ApplicationWindow, manage: &gtk::MenuButton) {
    let condition = adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        NARROW_WIDTH_SP,
        adw::LengthUnit::Sp,
    );
    let breakpoint = adw::Breakpoint::new(condition);
    // Signals rather than property setters: restoring a menu button's label
    // and icon depends on the order they are set in.
    let button = manage.clone();
    breakpoint.connect_apply(move |_| show_manage_button(&button, true));
    let button = manage.clone();
    breakpoint.connect_unapply(move |_| show_manage_button(&button, false));
    window.add_breakpoint(breakpoint);
}

fn show_manage_button(button: &gtk::MenuButton, compact: bool) {
    if compact {
        button.set_icon_name(MANAGE_ICON);
    } else {
        button.set_label(MANAGE_PROFILE);
    }
    // New content must not change what assistive technologies read.
    button.update_property(&[gtk::accessible::Property::Label(MANAGE_PROFILE)]);
}

/// The active connection's profile and server.
struct Summary {
    group: adw::PreferencesGroup,
    profile: adw::ActionRow,
    server: adw::ActionRow,
}

impl Summary {
    fn build() -> Self {
        let group = adw::PreferencesGroup::builder()
            .title("Active connection")
            .build();
        let profile = summary_row("Profile", "network-vpn-symbolic");
        let server = summary_row("Server", "network-server-symbolic");
        group.add(&profile);
        group.add(&server);
        Self {
            group,
            profile,
            server,
        }
    }
}

fn summary_row(title: &str, icon: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle_selectable(true)
        .build();
    row.add_prefix(
        &gtk::Image::builder()
            .icon_name(icon)
            .css_classes(["dim-label"])
            .build(),
    );
    row
}

/// The username and remember-password fields shown while disconnected.
struct Credentials {
    group: adw::PreferencesGroup,
    username: adw::EntryRow,
    remember: adw::SwitchRow,
}

impl Credentials {
    fn build() -> Self {
        let username = adw::EntryRow::builder().title("Username").build();
        let remember = adw::SwitchRow::builder()
            .title("Remember password")
            .subtitle("Stored by FortiClient, never by this app")
            .build();
        let group = adw::PreferencesGroup::builder().title("Sign in").build();
        group.add(&username);
        group.add(&remember);
        Self {
            group,
            username,
            remember,
        }
    }
}

fn main_button(text: &str, class: Option<&str>) -> gtk::Button {
    let button = widgets::button(text, class);
    button.set_size_request(-1, 42);
    button
}

fn show_button(button: &gtk::Button, view: &presentation::ButtonView) {
    button.set_label(&view.label);
    button.set_sensitive(view.sensitive);
    button.set_visible(view.visible);
}

/// A menu item whose tooltip explains why it is unavailable. `describe`
/// also sets the accessible description, as the Python UI did.
fn show_control(button: &gtk::Button, control: &Control, describe: bool) {
    button.set_sensitive(control.sensitive);
    button.set_tooltip_text(Some(&control.tooltip));
    if describe {
        button.update_property(&[gtk::accessible::Property::Description(&control.tooltip)]);
    }
}

/// Explain `button` in its tooltip and accessible description, or remove
/// both when there is nothing to explain.
fn describe(button: &gtk::Button, explanation: Option<&str>) {
    button.set_tooltip_text(explanation);
    match explanation {
        Some(text) => button.update_property(&[gtk::accessible::Property::Description(text)]),
        None => button.reset_property(gtk::AccessibleProperty::Description),
    }
}

fn alert(heading: &str, body: &str, response: &str, label: &str) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("cancel", "Cancel"), (response, label)]);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog
}

/// Animate `widget`'s opacity to `opacity`, replacing the animation in
/// `slot`. libadwaita jumps to the end when animations are off or the
/// widget is not mapped; high contrast skips decoration entirely.
fn fade(
    widget: &impl IsA<gtk::Widget>,
    opacity: f64,
    slot: &RefCell<Option<adw::TimedAnimation>>,
    duration: u32,
) {
    if let Some(previous) = slot.take() {
        previous.pause();
    }
    let target = widget.as_ref().downgrade();
    let target = adw::CallbackAnimationTarget::new(move |value| {
        if let Some(widget) = target.upgrade() {
            widget.set_opacity(value);
        }
    });
    let animation = adw::TimedAnimation::new(widget, widget.opacity(), opacity, duration, target);
    slot.replace(Some(animation.clone()));
    if adw::StyleManager::default().is_high_contrast() {
        animation.skip();
    } else {
        animation.play();
    }
}

fn color_scheme(appearance: Appearance) -> adw::ColorScheme {
    match appearance {
        Appearance::System => adw::ColorScheme::Default,
        Appearance::Light => adw::ColorScheme::ForceLight,
        Appearance::Dark => adw::ColorScheme::ForceDark,
    }
}

/// Diagnostics timestamps in local time.
fn local_clock() -> Clock {
    Arc::new(|| {
        glib::DateTime::now_local()
            .and_then(|now| now.format("%H:%M:%S"))
            .map(String::from)
            .unwrap_or_else(|_| "--:--:--".to_owned())
    })
}

/// Lower the minimum height on short monitors so the window fits: on the
/// shortest of the monitors it is on.
fn fit_to_monitor(window: &adw::ApplicationWindow) {
    window.connect_realize(|window| {
        let Some(surface) = window.surface() else {
            return;
        };
        // Until the compositor says which monitors the window is on, guess
        // the one it opens on.
        if let Some(monitor) = WidgetExt::display(window).monitor_at_surface(&surface) {
            apply_minimum_height(window, &[monitor]);
        }
        let monitors = Rc::new(RefCell::new(Vec::<gdk::Monitor>::new()));
        let (entered, weak) = (Rc::clone(&monitors), window.downgrade());
        surface.connect_enter_monitor(move |_, monitor| {
            let current = track_monitors(&entered, |monitors| {
                if !monitors.contains(monitor) {
                    monitors.push(monitor.clone());
                }
            });
            if let Some(window) = weak.upgrade() {
                apply_minimum_height(&window, &current);
            }
        });
        let (left, weak) = (monitors, window.downgrade());
        surface.connect_leave_monitor(move |_, monitor| {
            let current = track_monitors(&left, |monitors| {
                monitors.retain(|known| known != monitor);
            });
            if let Some(window) = weak.upgrade() {
                apply_minimum_height(&window, &current);
            }
        });
    });
}

/// Apply `change` to the tracked monitors and return them, without holding
/// the borrow while GTK resizes the window.
fn track_monitors(
    monitors: &RefCell<Vec<gdk::Monitor>>,
    change: impl FnOnce(&mut Vec<gdk::Monitor>),
) -> Vec<gdk::Monitor> {
    let mut monitors = monitors.borrow_mut();
    change(&mut monitors);
    monitors.clone()
}

/// Set the minimum height for the shortest of `monitors`, keeping the
/// minimum width. Without monitors, nothing changes.
fn apply_minimum_height(window: &adw::ApplicationWindow, monitors: &[gdk::Monitor]) {
    let shortest = monitors
        .iter()
        .map(|monitor| monitor.geometry().height())
        .min();
    if let Some(height) = shortest {
        let (width, _) = window.size_request();
        window.set_size_request(width, minimum_height(height));
    }
}

fn minimum_height(monitor_height: i32) -> i32 {
    if monitor_height < SHORT_MONITOR {
        (monitor_height - MONITOR_MARGIN).max(LOWEST_MINIMUM_HEIGHT)
    } else {
        HEIGHT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_monitors_lower_only_the_minimum_height() {
        assert_eq!(minimum_height(1080), HEIGHT);
        assert_eq!(minimum_height(SHORT_MONITOR), HEIGHT);
        assert_eq!(minimum_height(SHORT_MONITOR - 1), 739);
        assert_eq!(minimum_height(768), 648);
        assert_eq!(minimum_height(600), LOWEST_MINIMUM_HEIGHT);
    }

    #[test]
    fn appearances_map_to_color_schemes() {
        assert_eq!(color_scheme(Appearance::System), adw::ColorScheme::Default);
        assert_eq!(
            color_scheme(Appearance::Light),
            adw::ColorScheme::ForceLight
        );
        assert_eq!(color_scheme(Appearance::Dark), adw::ColorScheme::ForceDark);
    }

    #[test]
    fn clock_reads_hours_minutes_and_seconds() {
        let time = local_clock()();
        assert_eq!(time.len(), 8, "{time}");
        assert_eq!(time.matches(':').count(), 2, "{time}");
    }
}
