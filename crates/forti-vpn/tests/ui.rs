//! GTK widget tests: a port of the Python `tests/test_ui.py` for behaviour
//! the core presentation tests do not cover.
//!
//! GTK must run on the main thread, so this file has its own `main`
//! (`harness = false`). It needs a display and a session bus:
//!
//! ```sh
//! FORTI_VPN_UI_TESTS=1 FORTI_VPN_DBUS_TESTS=1 GSK_RENDERER=cairo xvfb-run -a dbus-run-session -- cargo test -p forti-vpn
//! ```
//!
//! With `FORTI_VPN_UI_TESTS=1`, the run fails when GTK cannot open a
//! display. Unset, empty or 0, every case is skipped then. Any other value
//! fails, so a typo cannot skip the cases. Set `FORTI_VPN_PREVIEW_DIR` to
//! also render PNG previews of the demo app there.
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use adw::prelude::*;
use forti_vpn::test_support::{self, App, Config, MainWindow, Services};
use forti_vpn_core::backend::{Backend, OperationContext};
use forti_vpn_core::demo::DemoBackend;
use forti_vpn_core::error::VpnError;
use forti_vpn_core::model::{
    AuthenticationMode, CertificateKind, ClientInfo, ConnectionState, Profile, ProfileSettings,
    PromptKind, Secret, Status,
};
use forti_vpn_core::parse::{LOGIN_BLOCKED_MESSAGE, SERVICE_STOPPED};
use forti_vpn_core::platform::{Elevation, Platform, Sandbox};
use forti_vpn_core::preferences::{Appearance, CloseBehavior, Preferences};
use forti_vpn_core::presentation::{CHECKING, EXIT_FAILED};
use forti_vpn_core::setup::tray_help;
use forti_vpn_core::workflow::{Busy, Challenge, Operation};
use gtk::{gdk, gio, glib};

const OFFICE: &str = "Office VPN";
const UBUNTU: &str = "ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=\"Ubuntu 26.04 LTS\"\n";
const ARCH: &str = "ID=arch\nPRETTY_NAME=\"Arch Linux\"\n";
const TIMEOUT: Duration = Duration::from_secs(5);
/// How to run these tests.
const RUN: &str = "FORTI_VPN_UI_TESTS=1 FORTI_VPN_DBUS_TESTS=1 GSK_RENDERER=cairo xvfb-run -a \
                   dbus-run-session -- cargo test -p forti-vpn";
const NO_TRAY: &str = "The top-bar icon is unavailable, so the window stays open. Use Disconnect \
                       and Quit from the menu, or see Settings to set it up.";
const FOOTER_NO_TRAY: &str =
    "Without a top bar icon, this window stays open when closed. Use Disconnect and Quit to exit.";
const FOOTER_HIDE: &str = "Close to keep running in the top bar.";
const FOOTER_QUIT: &str = "Closing this window disconnects the VPN and quits.";

type Case = (&'static str, fn(&Fixture));

const CASES: &[Case] = &[
    (
        "close_hides_and_preserves_connection",
        close_hides_and_preserves_connection,
    ),
    ("no_tray_never_hides_window", no_tray_never_hides_window),
    (
        "refused_close_keeps_the_operation_error",
        refused_close_keeps_the_operation_error,
    ),
    (
        "hiding_waits_for_operations_and_dialogs",
        hiding_waits_for_operations_and_dialogs,
    ),
    (
        "quit_disconnects_and_checks_status",
        quit_disconnects_and_checks_status,
    ),
    (
        "failed_disconnect_keeps_app_open",
        failed_disconnect_keeps_app_open,
    ),
    (
        "unresponsive_client_offers_to_quit_anyway",
        unresponsive_client_offers_to_quit_anyway,
    ),
    (
        "never_answering_client_quits_without_asking",
        never_answering_client_quits_without_asking,
    ),
    (
        "settings_persist_and_diagnostics_copy_and_clear",
        settings_persist_and_diagnostics_copy_and_clear,
    ),
    (
        "settings_show_the_tray_setup_command",
        settings_show_the_tray_setup_command,
    ),
    (
        "tray_reports_reach_the_footer_and_settings",
        tray_reports_reach_the_footer_and_settings,
    ),
    (
        "settings_changes_keep_the_remember_choice",
        settings_changes_keep_the_remember_choice,
    ),
    ("notice_opens_diagnostics", notice_opens_diagnostics),
    (
        "first_check_shows_progress_until_the_first_refresh",
        first_check_shows_progress_until_the_first_refresh,
    ),
    (
        "empty_profile_list_explains_what_to_do",
        empty_profile_list_explains_what_to_do,
    ),
    (
        "keyboard_opens_the_menu_and_connects_from_the_username",
        keyboard_opens_the_menu_and_connects_from_the_username,
    ),
    ("names_are_consistent", names_are_consistent),
    (
        "inputs_survive_renders_and_the_summary_shows_the_active_profile",
        inputs_survive_renders_and_the_summary_shows_the_active_profile,
    ),
    (
        "unconfirmed_attempt_offers_check_status_and_cancel_attempt",
        unconfirmed_attempt_offers_check_status_and_cancel_attempt,
    ),
    (
        "minimum_height_follows_the_monitors_the_window_is_on",
        minimum_height_follows_the_monitors_the_window_is_on,
    ),
    (
        "authentication_submit_clears_secret",
        authentication_submit_clears_secret,
    ),
    (
        "authentication_close_dismisses_prompt",
        authentication_close_dismisses_prompt,
    ),
    (
        "authentication_enter_and_escape_clear_secrets",
        authentication_enter_and_escape_clear_secrets,
    ),
    (
        "prompts_bring_the_window_forward",
        prompts_bring_the_window_forward,
    ),
    (
        "profile_validation_and_failed_save_preserve_form",
        profile_validation_and_failed_save_preserve_form,
    ),
    (
        "profile_port_and_certificate_submission",
        profile_port_and_certificate_submission,
    ),
    ("escape_cancels_pending_save", escape_cancels_pending_save),
    (
        "escape_cancels_save_prompt_and_keeps_form",
        escape_cancels_save_prompt_and_keeps_form,
    ),
    (
        "force_closing_a_saving_editor_cancels_the_save",
        force_closing_a_saving_editor_cancels_the_save,
    ),
    (
        "new_profile_editor_starts_empty_and_focused",
        new_profile_editor_starts_empty_and_focused,
    ),
    (
        "new_profiles_list_each_choice_once",
        new_profiles_list_each_choice_once,
    ),
    (
        "narrow_large_text_keeps_dialog_actions_in_the_window",
        narrow_large_text_keeps_dialog_actions_in_the_window,
    ),
    (
        "narrow_windows_show_manage_profile_as_an_icon",
        narrow_windows_show_manage_profile_as_an_icon,
    ),
    (
        "retry_follows_status_and_never_replays_delete",
        retry_follows_status_and_never_replays_delete,
    ),
    (
        "profile_menu_lock_and_details",
        profile_menu_lock_and_details,
    ),
    (
        "update_saved_password_confirms_first",
        update_saved_password_confirms_first,
    ),
    (
        "update_saved_password_cancel_and_external_connection",
        update_saved_password_cancel_and_external_connection,
    ),
    (
        "try_again_repeats_a_failed_password_update",
        try_again_repeats_a_failed_password_update,
    ),
    (
        "saved_password_is_used_without_asking",
        saved_password_is_used_without_asking,
    ),
    (
        "reduced_motion_finishes_fades",
        reduced_motion_finishes_fades,
    ),
    (
        "missing_cli_setup_and_check_again_recovery",
        missing_cli_setup_and_check_again_recovery,
    ),
    (
        "setup_checks_once_on_open_then_on_request",
        setup_checks_once_on_open_then_on_request,
    ),
    (
        "installed_but_unavailable_client_shows_recovery",
        installed_but_unavailable_client_shows_recovery,
    ),
    (
        "stopped_service_recovery_puts_the_command_first",
        stopped_service_recovery_puts_the_command_first,
    ),
    (
        "setup_focus_starts_and_returns_on_working_controls",
        setup_focus_starts_and_returns_on_working_controls,
    ),
    (
        "arch_setup_installs_from_the_aur",
        arch_setup_installs_from_the_aur,
    ),
    (
        "setup_command_copy_and_fresh_setup_quits",
        setup_command_copy_and_fresh_setup_quits,
    ),
    (
        "refresh_only_on_launch_and_on_request",
        refresh_only_on_launch_and_on_request,
    ),
    (
        "bundled_icons_resolve_without_a_theme",
        bundled_icons_resolve_without_a_theme,
    ),
    ("previews", previews),
];

fn main() {
    let required = display_required();
    let config_home = tempfile::tempdir().expect("create the configuration directory");
    // SAFETY: no other thread exists yet; GTK starts its threads later.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", config_home.path());
        std::env::set_var("FORTI_VPN_DISABLE_INDICATOR", "1");
    }
    if gtk::init().is_err() {
        // A panic, unlike `exit`, still removes the directory.
        assert!(
            !required,
            "FORTI_VPN_UI_TESTS=1, but GTK cannot open a display: run {RUN}"
        );
        println!("skipped: no display (run {RUN})");
        return;
    }
    adw::init().expect("libadwaita starts once GTK has");
    test_support::register_resources();
    let fixture = Fixture::new(config_home.path().to_owned());
    let failures = run_cases(&fixture);
    drop(fixture);
    // `exit` skips destructors, so remove the directory first.
    let _ = config_home.close();
    if failures > 0 {
        std::process::exit(1);
    }
}

/// Whether a run without a display fails: with `FORTI_VPN_UI_TESTS=1` it
/// does, and unset, empty or 0 it skips. Any other value panics, so that a
/// typo cannot skip the cases.
fn display_required() -> bool {
    let value = std::env::var_os("FORTI_VPN_UI_TESTS").unwrap_or_default();
    match value.to_str() {
        Some("" | "0") => false,
        Some("1") => true,
        _ => panic!("FORTI_VPN_UI_TESTS is {value:?}, not 1 or 0: run {RUN}"),
    }
}

fn run_cases(fixture: &Fixture) -> usize {
    let filter: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let mut failures = 0;
    for (name, case) in CASES {
        if !filter.is_empty() && !filter.iter().any(|wanted| name.contains(wanted.as_str())) {
            continue;
        }
        fixture.reset();
        let outcome = catch_unwind(AssertUnwindSafe(|| case(fixture)));
        fixture.close_windows();
        match outcome {
            Ok(()) => println!("test {name} ... ok"),
            Err(_) => {
                println!("test {name} ... FAILED");
                failures += 1;
            }
        }
    }
    println!("{failures} failed");
    failures
}

/// The application all cases share, and the windows a case opened.
struct Fixture {
    app: Rc<App>,
    /// The FortiClient of the application's own window.
    main_backend: Arc<FakeBackend>,
    config_home: PathBuf,
    quits: Rc<Cell<u32>>,
    windows: RefCell<Vec<Rc<MainWindow>>>,
}

impl Fixture {
    fn new(config_home: PathBuf) -> Self {
        let quits = Rc::new(Cell::new(0));
        let counter = Rc::clone(&quits);
        let main_backend = FakeBackend::new();
        let app = App::new(Config {
            application_id: "io.github.fortivpn.Utility.Tests".to_owned(),
            flags: gio::ApplicationFlags::NON_UNIQUE,
            backend: Arc::clone(&main_backend) as Arc<dyn Backend>,
            platform: ubuntu_gnome(),
            demo: false,
            tray: false,
            quit: Some(Rc::new(move || counter.set(counter.get() + 1))),
        });
        app.application
            .register(gio::Cancellable::NONE)
            .expect("register the test application");
        app.application.activate();
        pump_for(Duration::from_millis(200));
        Self {
            app,
            main_backend,
            config_home,
            quits,
            windows: RefCell::default(),
        }
    }

    /// Start every case with default preferences and appearance.
    fn reset(&self) {
        let _ = std::fs::remove_file(Preferences::default_path());
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::Default);
        self.quits.set(0);
    }

    /// A new main window on `backend`, after its first refresh.
    fn window(&self, backend: &Arc<FakeBackend>) -> Rc<MainWindow> {
        self.window_with(Arc::clone(backend) as Arc<dyn Backend>)
    }

    fn window_with(&self, backend: Arc<dyn Backend>) -> Rc<MainWindow> {
        let window = self.open(backend);
        settle(&window);
        window
    }

    /// A new main window on `backend`, while its first refresh runs.
    fn open(&self, backend: Arc<dyn Backend>) -> Rc<MainWindow> {
        let mut services: Services = self.app.services();
        services.backend = backend;
        self.show(services)
    }

    /// A new main window on `backend` on another system, after its first
    /// refresh.
    fn window_for(&self, backend: &Arc<FakeBackend>, platform: Platform) -> Rc<MainWindow> {
        let mut services: Services = self.app.services();
        services.backend = Arc::clone(backend) as Arc<dyn Backend>;
        services.platform = platform;
        let window = self.show(services);
        settle(&window);
        window
    }

    fn show(&self, services: Services) -> Rc<MainWindow> {
        let window = MainWindow::new(&self.app.application, services);
        window.present();
        self.windows.borrow_mut().push(Rc::clone(&window));
        window
    }

    fn close_windows(&self) {
        let windows = self.windows.take();
        for window in windows {
            window.shutdown();
        }
        pump_for(Duration::from_millis(50));
    }
}

fn ubuntu_gnome() -> Platform {
    let env = |name: &str| match name {
        "XDG_CURRENT_DESKTOP" => Some("ubuntu:GNOME".to_owned()),
        "XDG_SESSION_TYPE" => Some("wayland".to_owned()),
        _ => None,
    };
    Platform::from_parts(
        UBUNTU,
        &env,
        Sandbox::None,
        false,
        "x86_64",
        Elevation::Sudo,
        None,
    )
}

/// Arch Linux with GNOME and the `paru` AUR helper.
fn arch_gnome() -> Platform {
    let env = |name: &str| match name {
        "XDG_CURRENT_DESKTOP" => Some("GNOME".to_owned()),
        "XDG_SESSION_TYPE" => Some("wayland".to_owned()),
        _ => None,
    };
    Platform::from_parts(
        ARCH,
        &env,
        Sandbox::None,
        false,
        "x86_64",
        Elevation::Sudo,
        Some("paru"),
    )
}

/// Ubuntu inside Flatpak, where the host is assumed to run systemd, so the
/// setup dialog always shows the command that starts FortiClient's service.
fn ubuntu_flatpak() -> Platform {
    Platform {
        sandbox: Sandbox::Flatpak,
        ..ubuntu_gnome()
    }
}

// ---------------------------------------------------------------------------
// A FortiClient that lives in memory and records what it was asked to do.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveBehavior {
    Succeed,
    Fail,
    /// Ask for the certificate password; a dismissed prompt cancels.
    AskCertificatePassword,
    /// Wait until the operation is cancelled.
    WaitForCancel,
}

struct FakeState {
    status: Status,
    missing: bool,
    unavailable: bool,
    /// Status reads fail as they do while FortiClient's service is stopped.
    service_stopped: bool,
    /// FortiClient lists no profiles.
    no_profiles: bool,
    /// Status reads wait while this is set, as a slow FortiClient would.
    hold_status: bool,
    /// Profiles report a password saved in FortiClient.
    saved_password: bool,
    connect_error: Option<VpnError>,
    disconnect_error: Option<VpnError>,
    save: SaveBehavior,
    status_reads: u32,
    connects: Vec<(String, String, bool)>,
    disconnects: u32,
    saves: Vec<(ProfileSettings, Option<String>)>,
    removes: Vec<String>,
}

struct FakeBackend(Mutex<FakeState>);

impl FakeBackend {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(FakeState {
            status: Status::new(ConnectionState::Disconnected),
            missing: false,
            unavailable: false,
            service_stopped: false,
            no_profiles: false,
            hold_status: false,
            saved_password: false,
            connect_error: None,
            disconnect_error: None,
            save: SaveBehavior::Succeed,
            status_reads: 0,
            connects: Vec::new(),
            disconnects: 0,
            saves: Vec::new(),
            removes: Vec::new(),
        })))
    }

    fn with<T>(&self, change: impl FnOnce(&mut FakeState) -> T) -> T {
        change(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

fn office(saved_password: bool) -> Profile {
    let saved = if saved_password {
        "Enabled"
    } else {
        "Disabled"
    };
    Profile::with_fields(
        OFFICE,
        [
            ("Remote Gateway", "localhost:443"),
            ("Username", "worker"),
            ("Save Password", saved),
        ],
    )
}

impl Backend for FakeBackend {
    fn interactive_timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn status(&self) -> Result<Status, VpnError> {
        let deadline = Instant::now() + TIMEOUT;
        while self.with(|fake| fake.hold_status) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        self.with(|fake| {
            fake.status_reads += 1;
            if fake.missing {
                Err(VpnError::not_found("CLI missing"))
            } else if fake.service_stopped {
                Err(VpnError::failed(SERVICE_STOPPED))
            } else if fake.unavailable {
                Err(VpnError::failed("Service is unavailable"))
            } else {
                Ok(fake.status.clone())
            }
        })
    }

    fn list_profiles(&self) -> Result<Vec<Profile>, VpnError> {
        let listed = !self.with(|fake| fake.no_profiles);
        Ok(listed.then(|| Profile::new(OFFICE)).into_iter().collect())
    }

    fn view(&self, _profile: &Profile) -> Result<Profile, VpnError> {
        Ok(office(self.with(|fake| fake.saved_password)))
    }

    fn connect(
        &self,
        profile: &Profile,
        username: &str,
        remember: bool,
        context: &OperationContext<'_>,
    ) -> Result<Status, VpnError> {
        let error = self.with(|fake| {
            fake.connects
                .push((profile.name.clone(), username.to_owned(), remember));
            fake.connect_error.clone()
        });
        if let Some(error) = error {
            return Err(error);
        }
        if (context.ask)(PromptKind::Password)?.is_none() {
            return Err(VpnError::cancelled("Connection cancelled."));
        }
        Ok(self.with(|fake| {
            fake.status = Status::with_profile(ConnectionState::Connected, &profile.name);
            fake.status.clone()
        }))
    }

    fn disconnect(&self) -> Result<Status, VpnError> {
        self.with(|fake| {
            fake.disconnects += 1;
            if let Some(error) = fake.disconnect_error.clone() {
                return Err(error);
            }
            fake.status = Status::new(ConnectionState::Disconnected);
            Ok(fake.status.clone())
        })
    }

    fn save_profile(
        &self,
        settings: &ProfileSettings,
        existing: Option<&Profile>,
        context: &OperationContext<'_>,
    ) -> Result<Profile, VpnError> {
        let behavior = self.with(|fake| {
            fake.saves.push((
                settings.clone(),
                existing.map(|profile| profile.name.clone()),
            ));
            fake.save
        });
        match behavior {
            SaveBehavior::Succeed => Ok(Profile::new(settings.name.clone())),
            SaveBehavior::Fail => Err(VpnError::failed("Save rejected. Check the gateway.")),
            SaveBehavior::AskCertificatePassword => {
                match (context.ask)(PromptKind::CertificatePassword)? {
                    Some(_) => Ok(Profile::new(settings.name.clone())),
                    None => Err(VpnError::cancelled("Profile save cancelled.")),
                }
            }
            SaveBehavior::WaitForCancel => {
                context.cancel.wait_timeout(Duration::from_secs(2));
                Err(VpnError::cancelled("Profile save cancelled."))
            }
        }
    }

    fn remove(&self, profile: &Profile) -> Result<(), VpnError> {
        self.with(|fake| fake.removes.push(profile.name.clone()));
        Ok(())
    }

    fn information(&self) -> ClientInfo {
        ClientInfo {
            version: "7.4.3.5411".into(),
            path: "/usr/bin/forticlient".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers.

/// Run the main loop for `duration`, as the app would.
fn pump_for(duration: Duration) {
    let context = glib::MainContext::default();
    let end = Instant::now() + duration;
    while Instant::now() < end {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Run the main loop until `condition` holds; fail after [`TIMEOUT`].
fn pump_until(what: &str, condition: impl Fn() -> bool) {
    let context = glib::MainContext::default();
    let end = Instant::now() + TIMEOUT;
    loop {
        while context.pending() {
            context.iteration(false);
        }
        if condition() {
            return;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// libadwaita ignores Escape while a dialog is still opening.
const DIALOG_ANIMATION: Duration = Duration::from_millis(400);

/// Wait until no operation or refresh is running.
fn settle(window: &MainWindow) {
    pump_until("the workflow to settle", || {
        !window.is_refreshing() && window.with_state(|state| state.busy.is_none())
    });
    pump_for(Duration::from_millis(20));
}

/// Press Escape: offer it to the focused widget and then each ancestor, the
/// way a key press bubbles up, until a shortcut handles it.
fn press_escape(window: &MainWindow) -> bool {
    let mut current = gtk::prelude::RootExt::focus(&window.widgets.window);
    while let Some(widget) = current {
        if activate_escape(&widget) {
            return true;
        }
        current = widget.parent();
    }
    false
}

fn activate_escape(widget: &gtk::Widget) -> bool {
    let controllers = widget.observe_controllers();
    (0..controllers.n_items())
        .filter_map(|index| {
            controllers
                .item(index)
                .and_downcast::<gtk::ShortcutController>()
        })
        .flat_map(|controller| {
            (0..controller.n_items())
                .filter_map(move |index| controller.item(index).and_downcast::<gtk::Shortcut>())
        })
        .filter(|shortcut| {
            shortcut
                .trigger()
                .is_some_and(|trigger| trigger.to_str() == "Escape")
        })
        .any(|shortcut| {
            shortcut.action().is_some_and(|action| {
                action.activate(
                    gtk::ShortcutActionFlags::EXCLUSIVE,
                    widget,
                    shortcut.arguments().as_ref(),
                )
            })
        })
}

fn clipboard_text(widget: &impl IsA<gtk::Widget>) -> Option<String> {
    let provider = widget.clipboard().content()?;
    provider
        .value(glib::Type::STRING)
        .ok()?
        .get::<String>()
        .ok()
}

fn answer(receiver: &Receiver<Option<Secret>>) -> Result<Option<String>, TryRecvError> {
    receiver
        .try_recv()
        .map(|answer| answer.map(|secret| secret.as_str().to_owned()))
}

fn connected() -> Status {
    Status::with_profile(ConnectionState::Connected, OFFICE)
}

fn toast_title(toast: Option<adw::Toast>) -> Option<String> {
    toast.and_then(|toast| toast.title()).map(String::from)
}

/// The alert dialog the window shows, such as a confirmation.
fn alert(window: &MainWindow) -> adw::AlertDialog {
    window
        .widgets
        .window
        .visible_dialog()
        .and_downcast::<adw::AlertDialog>()
        .expect("an alert open")
}

/// Ask for a window whose content is `width` × `height` logical pixels.
fn resize(window: &MainWindow, width: i32, height: i32) {
    // Xvfb windows have a 5 px frame on each side.
    let native = &window.widgets.window;
    native.set_size_request(width + 10, height);
    native.set_default_size(width + 10, height);
}

/// Large text ("Sans 16") until dropped, even when a case fails.
struct LargeText {
    settings: gtk::Settings,
    font: Option<glib::GString>,
}

impl LargeText {
    fn new() -> Self {
        let settings = gtk::Settings::default().expect("GTK settings");
        let font = settings.gtk_font_name();
        settings.set_gtk_font_name(Some("Sans 16"));
        Self { settings, font }
    }
}

impl Drop for LargeText {
    fn drop(&mut self) {
        self.settings.set_gtk_font_name(self.font.as_deref());
    }
}

/// Assert that `widget` is shown, entirely inside `window`.
fn assert_inside(window: &MainWindow, widget: &impl IsA<gtk::Widget>) {
    let native = &window.widgets.window;
    let widget = widget.as_ref();
    let name = widget.type_().name();
    assert!(widget.is_mapped(), "{name} is not shown");
    let bounds = widget.compute_bounds(native).expect("bounds in the window");
    let (left, top) = (f64::from(bounds.x()), f64::from(bounds.y()));
    let right = left + f64::from(bounds.width());
    let bottom = top + f64::from(bounds.height());
    let (width, height) = (f64::from(native.width()), f64::from(native.height()));
    assert!(
        left >= 0.0 && top >= 0.0 && right <= width && bottom <= height,
        "{name} at ({left}, {top})–({right}, {bottom}) is outside {width} × {height}"
    );
}

/// The first label under `root` that shows `text`.
fn find_label(root: &impl IsA<gtk::Widget>, text: &str) -> Option<gtk::Label> {
    let root = root.as_ref();
    if let Some(label) = root.downcast_ref::<gtk::Label>()
        && label.text() == text
    {
        return Some(label.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(label) = find_label(&widget, text) {
            return Some(label);
        }
        child = widget.next_sibling();
    }
    None
}

// ---------------------------------------------------------------------------
// Closing and quitting.

fn close_hides_and_preserves_connection(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| fake.status = connected());
    let window = fixture.window(&fake);
    window.indicator_changed(true);
    window.widgets.window.close();
    assert!(!window.widgets.window.is_visible());
    assert_eq!(fake.with(|fake| fake.disconnects), 0);
    assert_eq!(window.state().status, connected());
    window.indicator_changed(false);
    assert!(window.widgets.window.is_visible());
}

fn no_tray_never_hides_window(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    let w = &window.widgets;
    // Not reported yet counts as no tray.
    assert_eq!(window.indicator(), None);
    w.window.close();
    assert!(w.window.is_visible());
    assert!(window.state().error.is_empty(), "not an operation error");
    assert!(w.notice_box.is_visible());
    assert_eq!(w.notice_title.text(), "Window stays open");
    assert!(w.notice_title.has_css_class("warning"));
    assert_eq!(w.warning_label.text(), NO_TRAY);
    assert!(!w.error_label.is_visible() && !w.retry_button.is_visible());

    window.indicator_changed(false);
    w.window.close();
    assert!(w.window.is_visible());
    assert_eq!(w.warning_label.text(), NO_TRAY);
    // The tray appears: the notice goes, and closing hides the window.
    window.indicator_changed(true);
    assert!(!w.notice_box.is_visible());
    w.window.close();
    assert!(!w.window.is_visible());
}

/// The notice about the window sits beside the operation's error, which
/// keeps its title and its retry.
fn refused_close_keeps_the_operation_error(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| fake.connect_error = Some(VpnError::failed("Rejected.")));
    let window = fixture.window(&fake);
    let w = &window.widgets;
    w.primary.emit_clicked();
    settle(&window);
    assert_eq!(w.notice_title.text(), "Connection issue");
    w.window.close();
    assert!(w.window.is_visible());
    assert_eq!(window.state().error, "Rejected.");
    assert_eq!(w.notice_title.text(), "Connection issue");
    assert!(w.notice_title.has_css_class("error"));
    assert_eq!(w.error_label.text(), "Rejected.");
    assert!(w.error_label.is_visible());
    assert_eq!(w.warning_label.text(), NO_TRAY);
    assert!(w.retry_button.is_visible());
    assert_eq!(w.retry_button.label().as_deref(), Some("Try Again"));
    window.indicator_changed(true);
    assert!(!w.warning_label.is_visible());
    assert_eq!(w.notice_title.text(), "Connection issue");
    assert!(w.retry_button.is_visible());
}

fn hiding_waits_for_operations_and_dialogs(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    let w = &window.widgets;
    window.indicator_changed(true);
    window.update_state(|state| state.busy = Some(Busy::Operation(Operation::Connecting)));
    w.window.close();
    assert!(w.window.is_visible());
    assert_eq!(w.notice_title.text(), "Window stays open");
    assert_eq!(
        w.warning_label.text(),
        "Finish or cancel the current operation before hiding the window."
    );
    window.update_state(|state| state.busy = None);
    assert!(!w.notice_box.is_visible(), "idle again");

    // libadwaita closes the topmost dialog first, one per request, and only
    // then does closing reach the window.
    window.show_settings();
    window.show_about();
    pump_for(DIALOG_ANIMATION);
    for open in [2, 1] {
        assert_eq!(w.window.dialogs().n_items(), open);
        w.window.close();
        pump_until("a dialog to close", || w.window.dialogs().n_items() < open);
        assert!(w.window.is_visible());
    }
    assert!(window.settings_dialog().is_none());
    assert!(!w.notice_box.is_visible(), "no notice about dialogs");
    w.window.close();
    assert!(!w.window.is_visible());
}

fn quit_disconnects_and_checks_status(fixture: &Fixture) {
    let fake = FakeBackend::new();
    let window = fixture.window(&fake);
    // FortiClient connected after the window last refreshed.
    fake.with(|fake| fake.status = connected());
    window.request_exit();
    pump_until("quit", || fixture.quits.get() == 1);
    assert_eq!(fake.with(|fake| fake.disconnects), 1);
    assert_eq!(
        fake.with(|fake| fake.status.state),
        ConnectionState::Disconnected
    );
}

fn failed_disconnect_keeps_app_open(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| {
        fake.status = connected();
        fake.disconnect_error = Some(VpnError::failed("failure"));
    });
    let window = fixture.window(&fake);
    window.request_exit();
    pump_until("the exit to give up", || !window.exit_requested());
    settle(&window);
    assert_eq!(fixture.quits.get(), 0);
    assert!(window.widgets.window.is_visible());
    assert_eq!(window.state().error, EXIT_FAILED);
    assert_eq!(
        window.widgets.retry_button.label().as_deref(),
        Some("Retry Disconnect")
    );
    // FortiClient answers, so there is nothing to quit regardless of.
    assert!(window.widgets.window.visible_dialog().is_none());
}

/// FortiClient answered once, then stopped: the VPN can be neither checked
/// nor ended, so the user decides.
fn unresponsive_client_offers_to_quit_anyway(fixture: &Fixture) {
    let fake = FakeBackend::new();
    let window = fixture.window(&fake);
    fake.with(|fake| fake.unavailable = true);
    let give_up = || {
        window.request_exit();
        pump_until("the exit to give up", || !window.exit_requested());
        settle(&window);
        pump_for(DIALOG_ANIMATION);
        alert(&window)
    };
    let dialog = give_up();
    assert_eq!(window.state().error, EXIT_FAILED);
    assert_eq!(
        dialog.heading().as_deref(),
        Some("Quit without disconnecting?")
    );
    assert_eq!(
        dialog.body(),
        "FortiClient is not responding, so Forti VPN cannot check or end the VPN connection."
    );
    assert_eq!(dialog.response_label("quit"), "Quit Anyway");
    assert_eq!(
        dialog.response_appearance("quit"),
        adw::ResponseAppearance::Destructive
    );
    dialog.emit_by_name::<()>("response", &[&"cancel"]);
    assert_eq!(fixture.quits.get(), 0);
    dialog.force_close();
    pump_for(DIALOG_ANIMATION);

    let dialog = give_up();
    dialog.emit_by_name::<()>("response", &[&"quit"]);
    assert_eq!(fixture.quits.get(), 1);
    assert_eq!(fake.with(|fake| fake.disconnects), 0);
}

/// FortiClient never answered, not even to the exit check: this app cannot
/// have connected, so it quits without a question.
fn never_answering_client_quits_without_asking(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| fake.unavailable = true);
    let window = fixture.window(&fake);
    assert!(!window.state().setup_required);
    window.request_exit();
    pump_until("quit", || fixture.quits.get() == 1);
    assert!(window.widgets.window.visible_dialog().is_none());
    assert_eq!(fake.with(|fake| fake.disconnects), 0);
}

// ---------------------------------------------------------------------------
// Settings and diagnostics.

fn settings_persist_and_diagnostics_copy_and_clear(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    window.indicator_changed(false);
    window.show_settings();
    let settings = window.settings_dialog().expect("settings open");
    assert_eq!(settings.tray_row.title(), "Top-bar icon");
    assert!(
        settings
            .tray_row
            .subtitle()
            .is_some_and(|text| text.starts_with("Unavailable"))
    );
    settings.close_behavior.set_selected(1);
    settings.appearance.set_selected(2);
    settings.remember.set_active(true);
    let values = window.preferences();
    assert_eq!(values.close_behavior, CloseBehavior::Quit);
    assert_eq!(values.appearance, Appearance::Dark);
    assert!(values.remember_password);
    assert_eq!(
        Preferences::load(Preferences::default_path()).values(),
        &values
    );
    assert!(adw::StyleManager::default().is_dark());
    assert!(window.widgets.remember.is_active());
    assert_eq!(window.widgets.note.text(), FOOTER_QUIT);
    window.update_state(|state| state.diagnostics = vec!["Sanitized event".to_owned()]);
    let buffer = settings.text.buffer();
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), false),
        "Sanitized event"
    );
    settings.copy_button.emit_clicked();
    assert_eq!(
        clipboard_text(&settings.dialog).as_deref(),
        Some("Sanitized event")
    );
    assert_eq!(
        toast_title(settings.last_toast()).as_deref(),
        Some("Diagnostics copied")
    );
    settings.clear_button.emit_clicked();
    assert!(window.state().diagnostics.is_empty());
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), false),
        ""
    );
    pump_until("the client version", || {
        settings.report_button.is_sensitive()
    });
    assert_eq!(
        settings.client_row.subtitle().as_deref(),
        Some("7.4.3.5411")
    );
    settings.report_button.emit_clicked();
    let report = clipboard_text(&settings.dialog).unwrap_or_default();
    assert!(report.contains("FortiClient: 7.4.3.5411"), "{report}");
    assert!(report.contains("System: Ubuntu 26.04 LTS"), "{report}");
    settings.appearance.set_selected(0);
    // Keys go to the focused widget; make sure it is in Settings.
    settings.close_behavior.grab_focus();
    // Escape first dismisses the queued toasts, one at a time, then closes
    // Settings; once it is closing, nothing handles Escape any more.
    for _ in 0..6 {
        if window.settings_dialog().is_none() || !press_escape(&window) {
            break;
        }
        pump_for(Duration::from_millis(300));
    }
    pump_until("settings to close", || window.settings_dialog().is_none());
}

fn settings_show_the_tray_setup_command(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    window.indicator_changed(false);
    window.show_settings();
    pump_for(DIALOG_ANIMATION);
    let settings = window.settings_dialog().expect("settings open");
    let expected = tray_help(window.platform())
        .command
        .expect("Ubuntu's GNOME has a setup command");
    let command = settings.tray_command.clone().expect("the command");
    let copy = settings.tray_copy.clone().expect("its copy button");
    // Shown in full, not only copied: users see what they would run.
    assert_eq!(command.text(), expected);
    assert!(command.is_selectable());
    assert!(command.has_css_class("monospace"));
    assert!(command.is_mapped() && copy.is_mapped());
    copy.emit_clicked();
    assert_eq!(clipboard_text(&copy).as_deref(), Some(expected.as_str()));
    assert_eq!(copy.label().as_deref(), Some("Command Copied"));

    window.indicator_changed(true);
    assert_eq!(
        settings.tray_row.subtitle().as_deref(),
        Some("Available in this desktop session")
    );
    assert!(!command.is_mapped() && !copy.is_mapped());
    window.indicator_changed(false);
    assert!(command.is_mapped());
}

/// What is known about the tray reaches the footer and Settings at once,
/// and Settings says what closing then does.
fn tray_reports_reach_the_footer_and_settings(fixture: &Fixture) {
    // The application's own window runs without a tray, and knows it.
    let main = fixture.app.window().expect("the application's window");
    assert_eq!(main.indicator(), Some(false));

    let window = fixture.window(&FakeBackend::new());
    let w = &window.widgets;
    assert_eq!(window.indicator(), None);
    assert_eq!(w.note.text(), FOOTER_HIDE, "still registering");
    window.show_settings();
    pump_for(DIALOG_ANIMATION);
    let settings = window.settings_dialog().expect("settings open");
    let subtitle = || settings.tray_row.subtitle().unwrap_or_default();
    let command = settings.tray_command.clone().expect("the command");
    assert_eq!(subtitle(), "Checking…");
    assert!(!command.is_mapped(), "no setup help before it is needed");

    window.indicator_changed(false);
    assert_eq!(w.note.text(), FOOTER_NO_TRAY);
    let help = tray_help(window.platform()).summary;
    let unavailable = |text: String| glib::markup_escape_text(&text).to_string();
    assert_eq!(
        subtitle(),
        unavailable(format!(
            "Unavailable — the window stays open when you close it. {help}"
        ))
    );
    assert!(command.is_mapped());
    settings.close_behavior.set_selected(1);
    assert_eq!(subtitle(), unavailable(format!("Unavailable. {help}")));
    assert_eq!(w.note.text(), FOOTER_QUIT);
    settings.close_behavior.set_selected(0);
    assert!(subtitle().starts_with("Unavailable — the window stays open"));

    window.indicator_changed(true);
    assert_eq!(subtitle(), "Available in this desktop session");
    assert_eq!(w.note.text(), FOOTER_HIDE);
}

/// Only a new default replaces the user's choice for this connection.
fn settings_changes_keep_the_remember_choice(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    let remember = &window.widgets.remember;
    assert!(!remember.is_active(), "the default preference");
    remember.set_active(true);
    window.show_settings();
    let settings = window.settings_dialog().expect("settings open");
    settings.appearance.set_selected(2);
    settings.close_behavior.set_selected(1);
    assert!(remember.is_active(), "other settings keep the choice");
    settings.remember.set_active(true);
    assert!(remember.is_active());
    remember.set_active(false);
    settings.appearance.set_selected(0);
    assert!(!remember.is_active(), "other settings keep the choice");
    settings.remember.set_active(false);
    settings.remember.set_active(true);
    assert!(remember.is_active(), "a new default applies");
    settings.remember.set_active(false);
    assert!(!remember.is_active(), "a new default applies");
}

fn notice_opens_diagnostics(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    window.update_state(|state| {
        state.error = LOGIN_BLOCKED_MESSAGE.to_owned();
        state.error_origin = Some(Operation::Connecting);
    });
    assert!(window.widgets.notice_box.is_visible());
    assert_eq!(
        window.widgets.notice_title.text(),
        "Login temporarily blocked"
    );
    assert!(window.widgets.notice_title.has_css_class("error"));
    assert!(!window.widgets.retry_button.is_visible());
    window.widgets.details_button.emit_clicked();
    let settings = window.settings_dialog().expect("settings open");
    assert_eq!(
        settings.dialog.visible_page().as_ref(),
        Some(&settings.diagnostics_page)
    );
    window.update_state(|state| {
        state.error.clear();
        state.status = connected();
        state.warnings = vec!["Your password could not be saved.".to_owned()];
    });
    assert_eq!(window.widgets.notice_title.text(), "Warning");
    assert!(window.widgets.notice_title.has_css_class("warning"));
    assert!(window.widgets.warning_label.is_visible());
    assert!(!window.widgets.error_label.is_visible());
    assert_eq!(window.widgets.status_label.text(), "Connected · Office VPN");
}

// ---------------------------------------------------------------------------
// The main window's layout.

fn first_check_shows_progress_until_the_first_refresh(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| {
        fake.unavailable = true;
        fake.hold_status = true;
    });
    let window = fixture.open(Arc::clone(&fake) as Arc<dyn Backend>);
    pump_for(Duration::from_millis(100));
    assert!(window.is_refreshing());
    let w = &window.widgets;
    // Nothing is known yet: no "unavailable" and no setup help.
    assert_eq!(w.status_label.text(), CHECKING);
    assert!(w.spinner.is_visible() && w.spinner.is_spinning());
    assert!(!w.status_icon.is_visible());
    assert!(w.status_badge.has_css_class("badge-progress"));
    assert!(!w.setup_help.is_visible());
    assert!(!w.operation_note.is_visible());

    fake.with(|fake| fake.hold_status = false);
    settle(&window);
    assert_eq!(w.status_label.text(), "FortiClient unavailable");
    assert!(!w.spinner.is_visible());
    assert!(w.status_badge.has_css_class("badge-unavailable"));
    assert!(!w.status_badge.has_css_class("badge-progress"));
    assert!(w.setup_help.is_visible());

    // Later refreshes keep showing what is known.
    fake.with(|fake| fake.hold_status = true);
    window.refresh();
    pump_for(Duration::from_millis(100));
    assert!(window.is_refreshing());
    assert_eq!(w.status_label.text(), "FortiClient unavailable");
    assert!(w.setup_help.is_visible());
    fake.with(|fake| fake.hold_status = false);
    settle(&window);
}

fn empty_profile_list_explains_what_to_do(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| fake.no_profiles = true);
    let window = fixture.window(&fake);
    let w = &window.widgets;
    let description = gtk::AccessibleProperty::Description;
    assert_eq!(
        w.profiles_group.description().as_deref(),
        Some("No VPN profiles yet. Create one with Manage Profile → New.")
    );
    assert!(!w.primary.is_sensitive());
    assert_eq!(
        w.primary.tooltip_text().as_deref(),
        Some("Create a VPN profile to connect.")
    );
    assert!(gtk::test_accessible_has_property(&w.primary, description));
    assert!(w.new_button.is_sensitive());

    fake.with(|fake| fake.no_profiles = false);
    window.refresh();
    settle(&window);
    // libadwaita reports a removed description as empty.
    assert_eq!(w.profiles_group.description().unwrap_or_default(), "");
    assert!(w.primary.is_sensitive());
    assert_eq!(w.primary.tooltip_text(), None);
    assert!(!gtk::test_accessible_has_property(&w.primary, description));
}

fn keyboard_opens_the_menu_and_connects_from_the_username(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| fake.connect_error = Some(VpnError::failed("Stop here.")));
    let window = fixture.window(&fake);
    let w = &window.widgets;
    assert!(w.menu_button.is_primary(), "F10 opens the main menu");
    let connects = || fake.with(|fake| fake.connects.clone());
    w.username.set_text("worker");
    w.username.emit_by_name::<()>("entry-activated", &[]);
    settle(&window);
    assert_eq!(
        connects(),
        [(OFFICE.to_owned(), "worker".to_owned(), false)]
    );
    // Not while Connect is unavailable.
    window.update_state(|state| state.profiles.clear());
    assert!(w.username.is_visible() && !w.primary.is_sensitive());
    w.username.emit_by_name::<()>("entry-activated", &[]);
    settle(&window);
    assert_eq!(connects().len(), 1);
}

/// The labels of the main menu's items.
fn menu_labels(button: &gtk::MenuButton) -> Vec<String> {
    let menu = button.menu_model().expect("a menu");
    (0..menu.n_items())
        .filter_map(|index| menu.item_attribute_value(index, "label", None))
        .filter_map(|label| label.get::<String>())
        .collect()
}

fn names_are_consistent(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    let w = &window.widgets;
    assert_eq!(
        menu_labels(&w.menu_button),
        [
            "Settings",
            "Set Up FortiClient",
            "About Forti VPN",
            "Disconnect and Quit"
        ]
    );
    assert_eq!(w.setup_help.label().as_deref(), Some("Set Up FortiClient"));
    window.show_setup();
    settle(&window);
    let setup = window.setup_dialog().expect("setup open");
    assert_eq!(setup.content.dialog.title(), "Set Up FortiClient");
    setup.content.dialog.force_close();
    window.show_about();
    pump_for(DIALOG_ANIMATION);
    let about = w
        .window
        .visible_dialog()
        .and_downcast::<adw::AboutDialog>()
        .expect("About open");
    assert_eq!(about.application_name(), "Forti VPN");
    about.force_close();
    w.delete_button.emit_clicked();
    pump_for(Duration::from_millis(50));
    let dialog = alert(&window);
    assert_eq!(dialog.heading().as_deref(), Some("Delete this profile?"));
    assert_eq!(
        dialog.body(),
        "“Office VPN” will be deleted from FortiClient. This cannot be undone."
    );
    dialog.force_close();
}

fn inputs_survive_renders_and_the_summary_shows_the_active_profile(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    let w = &window.widgets;
    let other = Profile::with_fields("Other VPN", [("Remote Gateway", "vpn.example.org:443")]);
    window.update_state(|state| state.profiles.push(other));
    w.username.set_text("edited username");
    w.remember.set_active(true);

    window.update_state(|state| state.busy = Some(Busy::Operation(Operation::Connecting)));
    assert!(!w.credentials.is_visible());
    assert!(!w.connection_summary.is_visible());
    assert!(w.operation_note.is_visible());
    assert!(w.cancel_button.is_sensitive());
    assert!(!w.manage_button.is_sensitive());

    window.update_state(|state| {
        state.busy = None;
        state.status = Status::with_profile(ConnectionState::Connected, "Other VPN");
    });
    assert!(w.connection_summary.is_visible());
    assert!(!w.credentials.is_visible());
    assert_eq!(w.connected_profile.subtitle().as_deref(), Some("Other VPN"));
    assert_eq!(
        w.connected_server.subtitle().as_deref(),
        Some("vpn.example.org:443")
    );
    assert!(!w.primary.has_css_class("suggested-action"));
    assert!(w.primary.has_css_class("destructive-action"));

    window.update_state(|state| state.busy = Some(Busy::Operation(Operation::Disconnecting)));
    assert!(!w.primary.is_sensitive());
    assert_eq!(w.primary.label().as_deref(), Some("Disconnect"));
    assert!(!w.cancel_button.is_visible());

    window.update_state(|state| {
        state.busy = None;
        state.status = Status::new(ConnectionState::Disconnected);
    });
    assert!(w.credentials.is_visible());
    assert!(!w.connection_summary.is_visible());
    assert!(w.primary.has_css_class("suggested-action"));
    assert!(!w.primary.has_css_class("destructive-action"));
    assert_eq!(w.username.text(), "edited username");
    assert!(w.remember.is_active());
    assert!(
        !window.preferences().remember_password,
        "a choice, not a setting"
    );
}

fn unconfirmed_attempt_offers_check_status_and_cancel_attempt(fixture: &Fixture) {
    let fake = FakeBackend::new();
    let window = fixture.window(&fake);
    // FortiClient rejects the attempt yet still reports it as connecting.
    fake.with(|fake| {
        fake.status = Status::with_profile(ConnectionState::Connecting, OFFICE);
        fake.connect_error = Some(VpnError::failed("FortiClient rejected the operation."));
    });
    let profile = window.selected().expect("a profile");
    window.operate(|workflow| workflow.connect(profile, String::new(), false));
    settle(&window);
    let w = &window.widgets;
    assert_eq!(window.state().status.state, ConnectionState::Connecting);
    assert_eq!(w.status_label.text(), "Connection not confirmed");
    assert_eq!(w.primary.label().as_deref(), Some("Check Status"));
    assert!(!w.primary.has_css_class("destructive-action"));
    assert!(w.cancel_button.is_visible());
    assert_eq!(w.cancel_button.label().as_deref(), Some("Cancel Attempt"));

    let reads = fake.with(|fake| fake.status_reads);
    w.primary.emit_clicked();
    settle(&window);
    assert_eq!(fake.with(|fake| fake.status_reads), reads + 1, "checked");
    assert_eq!(fake.with(|fake| fake.disconnects), 0);
    w.cancel_button.emit_clicked();
    settle(&window);
    assert_eq!(fake.with(|fake| fake.disconnects), 1, "cancelled");
    assert_eq!(window.state().status.state, ConnectionState::Disconnected);
}

fn minimum_height_follows_the_monitors_the_window_is_on(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    let native = &window.widgets.window;
    let surface = native.surface().expect("a shown window has a surface");
    let monitor = WidgetExt::display(native)
        .monitors()
        .item(0)
        .and_downcast::<gdk::Monitor>()
        .expect("a monitor");
    // Fitted to the monitor the window opened on.
    let (width, fitted) = native.size_request();
    native.set_size_request(width, 100);
    // With no monitor left there is nothing to fit.
    surface.emit_by_name::<()>("leave-monitor", &[&monitor]);
    assert_eq!(native.size_request(), (width, 100));
    // Entering one fits the height again and keeps the width.
    native.set_size_request(370, 100);
    surface.emit_by_name::<()>("enter-monitor", &[&monitor]);
    assert_eq!(native.size_request(), (370, fitted));
    surface.emit_by_name::<()>("enter-monitor", &[&monitor]);
    surface.emit_by_name::<()>("leave-monitor", &[&monitor]);
    native.set_size_request(width, 100);
    surface.emit_by_name::<()>("leave-monitor", &[&monitor]);
    assert_eq!(native.size_request(), (width, 100), "entered once");
}

fn narrow_windows_show_manage_profile_as_an_icon(fixture: &Fixture) {
    let _large = LargeText::new();
    let window = fixture.window(&FakeBackend::new());
    let manage = &window.widgets.manage_button;
    pump_for(Duration::from_millis(200));
    assert_eq!(manage.label().as_deref(), Some("Manage Profile"));
    assert_eq!(manage.icon_name(), None);

    resize(&window, 360, 650);
    pump_until("the narrow layout", || manage.icon_name().is_some());
    assert_eq!(manage.icon_name().as_deref(), Some("view-more-symbolic"));
    assert_eq!(manage.tooltip_text().as_deref(), Some("Manage Profile"));
    assert!(gtk::test_accessible_has_property(
        manage,
        gtk::AccessibleProperty::Label
    ));
    pump_for(Duration::from_millis(100));
    let title = find_label(&window.widgets.profiles_group, "VPN profile").expect("the title");
    assert!(!title.layout().is_ellipsized(), "the title is cut off");

    resize(&window, 480, 740);
    pump_until("the wide layout", || manage.label().is_some());
    assert_eq!(manage.label().as_deref(), Some("Manage Profile"));
}

// ---------------------------------------------------------------------------
// Authentication prompts.

fn prompt(window: &Rc<MainWindow>, kind: PromptKind) -> Receiver<Option<Secret>> {
    window.update_state(|state| state.busy = Some(Busy::AwaitingAuthentication));
    let (challenge, answers) = Challenge::new(kind);
    window.show_challenge(challenge);
    pump_for(DIALOG_ANIMATION);
    answers
}

fn authentication_submit_clears_secret(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    let answers = prompt(&window, PromptKind::Mfa);
    let dialog = window.challenge_dialog().expect("prompt open");
    assert!(dialog.dialog().focus().is_some());
    assert!(dialog.entry.is::<adw::PasswordEntryRow>());
    assert_eq!(dialog.entry.title(), "Verification code");
    dialog.entry.set_text("123456");
    dialog.proceed.emit_clicked();
    assert_eq!(answer(&answers), Ok(Some("123456".to_owned())));
    assert_eq!(dialog.entry.text(), "");
    window.update_state(|state| state.busy = None);
}

fn authentication_close_dismisses_prompt(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    let answers = prompt(&window, PromptKind::Password);
    let dialog = window.challenge_dialog().expect("prompt open");
    dialog.entry.set_text("typed but not sent");
    dialog.close();
    pump_until("the prompt to close", || {
        window.challenge_dialog().is_none()
    });
    assert_eq!(answer(&answers), Ok(None));
    assert_eq!(dialog.entry.text(), "");
    window.update_state(|state| state.busy = None);
}

fn authentication_enter_and_escape_clear_secrets(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    let answers = prompt(&window, PromptKind::Password);
    let dialog = window.challenge_dialog().expect("prompt open");
    assert_eq!(
        dialog.dialog().default_widget().as_ref(),
        Some(dialog.proceed.upcast_ref::<gtk::Widget>())
    );
    assert!(!dialog.proceed.is_sensitive());
    dialog.entry.set_text("temporary secret");
    assert!(dialog.proceed.is_sensitive());
    dialog.entry.emit_by_name::<()>("entry-activated", &[]);
    pump_until("Enter to close the prompt", || {
        window.challenge_dialog().is_none()
    });
    assert_eq!(answer(&answers), Ok(Some("temporary secret".to_owned())));
    assert_eq!(answer(&answers), Err(TryRecvError::Empty));
    assert_eq!(dialog.entry.text(), "");

    let answers = prompt(&window, PromptKind::Mfa);
    let dialog = window.challenge_dialog().expect("prompt open");
    dialog.entry.set_text("123456");
    assert!(press_escape(&window));
    pump_until("Escape to close the prompt", || {
        window.challenge_dialog().is_none()
    });
    assert_eq!(answer(&answers), Ok(None));
    assert_eq!(dialog.entry.text(), "");
    window.update_state(|state| state.busy = None);
}

/// A prompt is never left in a hidden window, and says what it asks for.
fn prompts_bring_the_window_forward(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    window.widgets.window.set_visible(false);
    let answers = prompt(&window, PromptKind::Username);
    assert!(window.widgets.window.is_visible());
    let dialog = window.challenge_dialog().expect("prompt open");
    assert!(!dialog.entry.is::<adw::PasswordEntryRow>());
    assert!(find_label(&dialog.content.body, "Enter your VPN username.").is_some());
    dialog.close();
    pump_until("the prompt to close", || {
        window.challenge_dialog().is_none()
    });
    assert_eq!(answer(&answers), Ok(None));
    let _answers = prompt(&window, PromptKind::VerifyPassword);
    let dialog = window.challenge_dialog().expect("prompt open");
    let instructions = "Enter the client certificate password again.";
    assert!(find_label(&dialog.content.body, instructions).is_some());
    window.update_state(|state| state.busy = None);
}

// ---------------------------------------------------------------------------
// The profile editor.

fn open_editor(window: &Rc<MainWindow>) -> Rc<test_support::ProfileEditor> {
    window.profile_editor(window.selected());
    pump_for(DIALOG_ANIMATION);
    window.profile_dialog().expect("editor open")
}

fn profile_validation_and_failed_save_preserve_form(fixture: &Fixture) {
    let fake = FakeBackend::new();
    let window = fixture.window(&fake);
    let editor = open_editor(&window);
    assert_eq!(editor.dialog().title(), "Edit “Office VPN”");
    assert_eq!(editor.auth.title(), "Authentication mode");
    assert_eq!(editor.cert.title(), "Certificate type");
    assert_eq!(
        editor.dialog().default_widget().as_ref(),
        Some(editor.save_button.upcast_ref::<gtk::Widget>())
    );
    editor.gateway.set_text("");
    editor.gateway.emit_by_name::<()>("entry-activated", &[]);
    assert!(editor.gateway.has_css_class("error"));
    assert!(editor.content.error.text().contains("gateway"));
    assert!(editor.dialog().focus().is_some());
    editor.gateway.set_text("vpn.example.org:443");
    fake.with(|fake| fake.save = SaveBehavior::Fail);
    editor.gateway.emit_by_name::<()>("entry-activated", &[]);
    assert!(editor.is_saving());
    assert!(!editor.dialog().can_close());
    settle(&window);
    assert!(!editor.is_saving());
    assert!(
        window
            .profile_dialog()
            .is_some_and(|open| Rc::ptr_eq(&open, &editor))
    );
    assert!(editor.dialog().can_close());
    assert_eq!(editor.gateway.text(), "vpn.example.org");
    assert_eq!(editor.port.text(), "443");
    assert_eq!(editor.save_button.label().as_deref(), Some("Retry Save"));
    assert!(editor.content.error.text().contains("Save rejected"));
    assert!(window.last_toast().is_none());

    fake.with(|fake| fake.save = SaveBehavior::Succeed);
    editor.save_button.emit_clicked();
    settle(&window);
    pump_until("the editor to close", || window.profile_dialog().is_none());
    let toast = window.last_toast();
    assert_eq!(toast_title(toast.clone()).as_deref(), Some("Profile saved"));
    window.refresh();
    settle(&window);
    assert_eq!(window.last_toast(), toast, "a refresh must not toast again");
}

fn profile_port_and_certificate_submission(fixture: &Fixture) {
    let fake = FakeBackend::new();
    let window = fixture.window(&fake);
    let editor = open_editor(&window);
    assert_eq!(editor.gateway.text(), "localhost");
    assert_eq!(editor.port.text(), "443");
    editor.port.set_text("70000");
    editor.save();
    assert!(editor.port.has_css_class("error"));
    assert!(!editor.is_saving());
    editor.port.set_text("8443");
    editor.cert.set_selected(1);
    assert!(editor.path.is_sensitive() && editor.browse.is_sensitive());
    assert_eq!(editor.path.title(), "Certificate file");
    let dismissed = glib::Error::new(gtk::DialogError::Dismissed, "dismissed");
    editor.certificate_chosen(1, Err(dismissed));
    assert!(!editor.content.error.text().contains("file picker"));
    let path = fixture.config_home.join("client certificate.p12");
    let path = path.to_str().expect("a UTF-8 path");
    editor.certificate_chosen(1, Ok(gio::File::for_path(path)));
    assert_eq!(editor.path.text(), path);
    editor.save();
    settle(&window);
    let saves = fake.with(|fake| fake.saves.clone());
    let [(settings, existing)] = saves.as_slice() else {
        panic!("expected one save, got {saves:?}");
    };
    assert_eq!(settings.gateway, "localhost:8443");
    assert_eq!(settings.certificate, path);
    assert_eq!(settings.certificate_kind, CertificateKind::Local);
    assert_eq!(settings.authentication, AuthenticationMode::Keep);
    assert_eq!(existing.as_deref(), Some(OFFICE));
}

fn escape_cancels_pending_save(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| fake.save = SaveBehavior::WaitForCancel);
    let window = fixture.window(&fake);
    let editor = open_editor(&window);
    editor.save_button.emit_clicked();
    pump_for(Duration::from_millis(50));
    assert!(!editor.dialog().can_close());
    assert!(press_escape(&window));
    pump_until("the save to stop", || !editor.is_saving());
    assert!(window.operation_cancelled());
    assert!(
        window
            .profile_dialog()
            .is_some_and(|open| Rc::ptr_eq(&open, &editor))
    );
    assert!(editor.dialog().can_close());
    settle(&window);
}

fn escape_cancels_save_prompt_and_keeps_form(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| fake.save = SaveBehavior::AskCertificatePassword);
    let window = fixture.window(&fake);
    let editor = open_editor(&window);
    editor.gateway.set_text("vpn.example.org");
    editor.save_button.emit_clicked();
    pump_until("the certificate prompt", || {
        window.challenge_dialog().is_some()
    });
    pump_for(DIALOG_ANIMATION);
    let prompt = window.challenge_dialog().expect("prompt open");
    assert_eq!(prompt.entry.title(), "Client certificate password");
    prompt.entry.set_text("temporary certificate password");
    assert!(press_escape(&window));
    pump_until("the save to stop", || !editor.is_saving());
    settle(&window);
    assert_eq!(prompt.entry.text(), "");
    assert_eq!(editor.gateway.text(), "vpn.example.org");
    assert!(editor.content.error.text().contains("cancelled"));
    assert!(press_escape(&window));
    pump_until("the editor to close", || window.profile_dialog().is_none());
}

fn force_closing_a_saving_editor_cancels_the_save(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| fake.save = SaveBehavior::WaitForCancel);
    let window = fixture.window(&fake);
    let editor = open_editor(&window);
    editor.save_button.emit_clicked();
    pump_for(Duration::from_millis(50));
    assert!(editor.is_saving());
    assert!(!editor.dialog().can_close());
    // libadwaita 1.5.0 can close a dialog that refuses to close.
    editor.dialog().force_close();
    pump_until("the editor to go", || window.profile_dialog().is_none());
    assert!(window.operation_cancelled(), "the save outlived its form");
    settle(&window);
    assert_eq!(fake.with(|fake| fake.saves.len()), 1);
    assert!(window.last_toast().is_none(), "nothing was saved");
}

fn new_profile_editor_starts_empty_and_focused(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    let edit = open_editor(&window);
    assert_eq!(edit.dialog().title(), "Edit “Office VPN”");
    assert_eq!(edit.gateway.text(), "localhost");
    edit.dialog().close();
    pump_until("the editor to close", || window.profile_dialog().is_none());

    window.profile_editor(None);
    pump_for(DIALOG_ANIMATION);
    let editor = window.profile_dialog().expect("editor open");
    assert!(!Rc::ptr_eq(&editor, &edit));
    assert_eq!(editor.dialog().title(), "New profile");
    assert_eq!(
        editor.dialog().default_widget().as_ref(),
        Some(editor.save_button.upcast_ref::<gtk::Widget>())
    );
    let focus = editor.dialog().focus().expect("a focused widget");
    assert!(focus.is_ancestor(&editor.name), "focus is not on the name");
    for row in [
        &editor.name,
        &editor.gateway,
        &editor.username,
        &editor.path,
    ] {
        assert_eq!(row.text(), "", "{}", row.title());
    }
    assert_eq!(editor.port.text(), "443", "the default port");
    assert!(editor.name.is_sensitive());
    assert_eq!((editor.auth.selected(), editor.cert.selected()), (0, 0));
    assert_eq!(editor.auth.title(), "Authentication mode");
    assert_eq!(editor.cert.title(), "Certificate type");
    editor.dialog().close();
    pump_until("the editor to close", || window.profile_dialog().is_none());
}

/// The strings a combo row offers, top to bottom.
fn choices(row: &adw::ComboRow) -> Vec<String> {
    let model = row
        .model()
        .and_downcast::<gtk::StringList>()
        .expect("a list of strings");
    (0..model.n_items())
        .filter_map(|index| model.string(index))
        .map(String::from)
        .collect()
}

fn new_profiles_list_each_choice_once(fixture: &Fixture) {
    let fake = FakeBackend::new();
    let window = fixture.window(&fake);
    window.profile_editor(None);
    pump_for(DIALOG_ANIMATION);
    let editor = window.profile_dialog().expect("editor open");
    assert_eq!(
        choices(&editor.auth),
        ["Prompt for credentials", "Save username", "Disabled"]
    );
    assert_eq!(
        choices(&editor.cert),
        ["None", "Local certificate (.p12)", "Smartcard (PKCS#11)"]
    );
    editor.name.set_text("Lab");
    editor.gateway.set_text("lab.example.org");
    assert!(!editor.username.is_sensitive());
    editor.auth.set_selected(1);
    assert!(editor.username.is_sensitive(), "Save username");
    editor.username.set_text("worker");
    assert!(!editor.path.is_sensitive());
    editor.cert.set_selected(2);
    assert!(editor.path.is_sensitive() && editor.browse.is_sensitive());
    assert_eq!(editor.path.title(), "PKCS#11 library");
    editor.path.set_text("/usr/lib/opensc-pkcs11.so");
    editor.save();
    settle(&window);
    let saves = fake.with(|fake| fake.saves.clone());
    let [(settings, existing)] = saves.as_slice() else {
        panic!("expected one save, got {saves:?}");
    };
    assert_eq!(settings.authentication, AuthenticationMode::Save);
    assert_eq!(settings.username, "worker");
    assert_eq!(settings.certificate_kind, CertificateKind::Smartcard);
    assert_eq!(existing, &None);
    pump_until("the editor to close", || window.profile_dialog().is_none());

    // An existing profile's setting can be kept, as the first choice.
    let edit = open_editor(&window);
    assert_eq!(
        choices(&edit.auth),
        [
            "Keep existing setting",
            "Prompt for credentials",
            "Save username",
            "Disabled"
        ]
    );
    assert_eq!(
        choices(&edit.cert),
        [
            "Keep existing setting",
            "Local certificate (.p12)",
            "Smartcard (PKCS#11)",
            "None"
        ]
    );
    edit.auth.set_selected(2);
    assert!(edit.username.is_sensitive(), "Save username");
    edit.dialog().close();
    pump_until("the editor to close", || window.profile_dialog().is_none());
}

fn narrow_large_text_keeps_dialog_actions_in_the_window(fixture: &Fixture) {
    let _large = LargeText::new();
    let window = fixture.window(&FakeBackend::new());
    resize(&window, 360, 650);
    pump_for(Duration::from_millis(300));

    window.profile_editor(None);
    pump_for(DIALOG_ANIMATION);
    let editor = window.profile_dialog().expect("editor open");
    editor.save_button.emit_clicked();
    pump_for(Duration::from_millis(300));
    assert!(
        !editor.content.error.text().is_empty(),
        "an empty form is invalid"
    );
    assert_inside(&window, &editor.cancel);
    assert_inside(&window, &editor.save_button);
    assert_inside(&window, &editor.content.error);
    editor.dialog().force_close();
    pump_until("the editor to close", || window.profile_dialog().is_none());

    window.show_setup();
    settle(&window);
    let setup = window.setup_dialog().expect("setup open");
    setup.show_help();
    let package = fixture
        .config_home
        .join(format!("forticlient_{}.deb", "x".repeat(140)));
    std::fs::write(&package, b"").expect("create the installer");
    setup.set_package(&package);
    pump_for(Duration::from_millis(300));
    let width = window.widgets.window.width();
    assert!(setup.content.dialog.width() <= width);
    let content = setup.content.dialog.child().expect("the dialog's content");
    let (minimum, ..) = content.measure(gtk::Orientation::Horizontal, -1);
    assert!(minimum <= width, "{minimum} > {width}");
    assert_inside(&window, &setup.check);
    assert_inside(&window, &setup.close_button);
    setup.close();
}

// ---------------------------------------------------------------------------
// The main window's actions.

fn retry_follows_status_and_never_replays_delete(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| fake.connect_error = Some(VpnError::failed("Rejected.")));
    let window = fixture.window(&fake);
    let retry = &window.widgets.retry_button;
    window.widgets.username.set_text("worker");
    window.update_state(|state| {
        state.error = "Operation failed.".to_owned();
        state.error_origin = Some(Operation::Connecting);
    });
    assert!(retry.is_visible());
    assert_eq!(retry.label().as_deref(), Some("Try Again"));
    retry.emit_clicked();
    settle(&window);
    assert_eq!(
        fake.with(|fake| fake.connects.clone()),
        [(OFFICE.to_owned(), "worker".to_owned(), false)]
    );

    window.update_state(|state| {
        state.status = connected();
        state.error = "Operation failed.".to_owned();
        state.error_origin = Some(Operation::Connecting);
    });
    assert!(!retry.is_visible());
    window.update_state(|state| state.error_origin = Some(Operation::Disconnecting));
    assert_eq!(retry.label().as_deref(), Some("Retry Disconnect"));
    retry.emit_clicked();
    settle(&window);
    assert_eq!(fake.with(|fake| fake.disconnects), 1);

    window.update_state(|state| {
        state.status = connected();
        state.error = "Operation failed.".to_owned();
        state.error_origin = Some(Operation::RemovingProfile);
    });
    assert_eq!(retry.label().as_deref(), Some("Manage Profile"));
    retry.emit_clicked();
    pump_for(Duration::from_millis(50));
    assert!(fake.with(|fake| fake.removes.is_empty()));
    assert!(window.widgets.manage_button.is_active());
    window.widgets.manage_button.popdown();

    let reads = fake.with(|fake| fake.status_reads);
    window.update_state(|state| {
        state.status = Status::unavailable();
        state.error = "Operation failed.".to_owned();
    });
    assert_eq!(retry.label().as_deref(), Some("Refresh"));
    retry.emit_clicked();
    settle(&window);
    assert_eq!(fake.with(|fake| fake.status_reads), reads + 1);
}

fn profile_menu_lock_and_details(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    let w = &window.widgets;
    window.update_state(|state| state.status = connected());
    assert!(w.view_button.is_sensitive());
    assert!(!w.edit_button.is_sensitive());
    assert!(!w.delete_button.is_sensitive());
    assert!(!w.password_button.is_sensitive());
    assert!(
        w.edit_button
            .tooltip_text()
            .is_some_and(|text| text.contains("Disconnect"))
    );
    assert_eq!(w.primary.label().as_deref(), Some("Disconnect"));
    assert!(w.primary.has_css_class("destructive-action"));
    assert!(w.connection_summary.is_visible());
    assert_eq!(
        w.connected_server.subtitle().as_deref(),
        Some("localhost:443")
    );
    w.manage_button.popup();
    pump_for(Duration::from_millis(50));
    w.view_button.grab_focus();
    assert!(w.view_button.has_focus());
    w.view_button.emit_clicked();
    pump_for(Duration::from_millis(50));
    assert!(!w.manage_button.is_active());
    let details = w.window.visible_dialog().expect("details open");
    assert_eq!(details.title(), "Profile details");
    details.force_close();
    window.update_state(|state| {
        state.status = Status::new(ConnectionState::Disconnected);
        state.profiles[0].managed = true;
    });
    assert!(
        w.delete_button
            .tooltip_text()
            .is_some_and(|text| text.contains("organization"))
    );
    assert!(!w.edit_button.is_sensitive());
}

fn update_saved_password_confirms_first(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| fake.connect_error = Some(VpnError::failed("Stop here.")));
    let window = fixture.window(&fake);
    window.widgets.username.set_text("worker");
    window.widgets.remember.set_active(false);
    window.widgets.password_button.emit_clicked();
    pump_for(Duration::from_millis(50));
    let dialog = alert(&window);
    assert_eq!(dialog.heading().as_deref(), Some("Update saved password?"));
    assert!(dialog.body().contains("as “worker”"));
    assert!(fake.with(|fake| fake.connects.is_empty()));
    dialog.emit_by_name::<()>("response", &[&"save"]);
    settle(&window);
    assert_eq!(
        fake.with(|fake| fake.connects.clone()),
        [(OFFICE.to_owned(), "worker".to_owned(), true)]
    );
    assert!(!window.widgets.remember.is_active());
    dialog.force_close();
}

fn update_saved_password_cancel_and_external_connection(fixture: &Fixture) {
    let fake = FakeBackend::new();
    let window = fixture.window(&fake);
    window.update_saved_password();
    pump_for(Duration::from_millis(50));
    let dialog = alert(&window);
    dialog.emit_by_name::<()>("response", &[&"cancel"]);
    pump_for(Duration::from_millis(50));
    assert!(fake.with(|fake| fake.connects.is_empty()));
    dialog.force_close();
    pump_for(DIALOG_ANIMATION);

    window.update_saved_password();
    pump_for(Duration::from_millis(50));
    let dialog = alert(&window);
    // FortiClient connected outside this app while the dialog was open.
    window.update_state(|state| state.status = connected());
    dialog.emit_by_name::<()>("response", &[&"save"]);
    pump_for(Duration::from_millis(50));
    assert!(fake.with(|fake| fake.connects.is_empty()));
    assert_eq!(
        toast_title(window.last_toast()).as_deref(),
        Some("Refresh profiles and disconnect the VPN before updating the saved password.")
    );
    dialog.force_close();
}

/// "Try Again" after a failed "Update Saved Password" updates it again: a
/// plain Connect would sign in with the password FortiClient saved.
fn try_again_repeats_a_failed_password_update(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| {
        fake.saved_password = true;
        fake.connect_error = Some(VpnError::failed("Stop here."));
    });
    let window = fixture.window(&fake);
    let w = &window.widgets;
    let last = || fake.with(|fake| fake.connects.last().cloned());
    let attempt =
        |username: &str, remember| Some((OFFICE.to_owned(), username.to_owned(), remember));
    let try_again = || {
        assert_eq!(w.retry_button.label().as_deref(), Some("Try Again"));
        w.retry_button.emit_clicked();
        settle(&window);
    };
    let update = || {
        w.password_button.emit_clicked();
        pump_for(Duration::from_millis(50));
        let dialog = alert(&window);
        dialog.emit_by_name::<()>("response", &[&"save"]);
        settle(&window);
        dialog.force_close();
        pump_for(DIALOG_ANIMATION);
    };
    w.username.set_text("  other  ");
    update();
    assert_eq!(last(), attempt("other", true));
    try_again();
    assert_eq!(
        last(),
        attempt("other", true),
        "the typed username, trimmed"
    );
    w.username.set_text("");
    try_again();
    assert_eq!(last(), attempt("worker", true), "the profile's username");
    assert_eq!(fake.with(|fake| fake.connects.len()), 3);

    // After a plain Connect, Try Again connects plainly too.
    w.username.set_text("worker");
    w.primary.emit_clicked();
    settle(&window);
    try_again();
    assert_eq!(fake.with(|fake| fake.connects.len()), 5);
    assert_eq!(last(), attempt("worker", false));

    // So it does once another profile was selected.
    update();
    assert_eq!(last(), attempt("worker", true));
    window.update_state(|state| state.profiles.push(Profile::new("Other VPN")));
    w.profile_row.set_selected(1);
    w.profile_row.set_selected(0);
    try_again();
    assert_eq!(last(), attempt("worker", false));
}

fn saved_password_is_used_without_asking(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| {
        fake.saved_password = true;
        fake.connect_error = Some(VpnError::failed("Stop here."));
    });
    let window = fixture.window(&fake);
    let w = &window.widgets;
    let remembered = || {
        fake.with(|fake| {
            fake.connects
                .iter()
                .map(|(.., remember)| *remember)
                .collect::<Vec<_>>()
        })
    };
    // Held on, and explained.
    assert!(w.remember.is_active());
    assert!(!w.remember.is_sensitive());
    let subtitle = w.remember.subtitle().unwrap_or_default();
    assert!(subtitle.contains("Update Saved Password"), "{subtitle}");

    // Connect and Try Again use FortiClient's saved password: remembering
    // anew would ask for a password.
    w.username.set_text("worker");
    w.primary.emit_clicked();
    settle(&window);
    assert_eq!(
        fake.with(|fake| fake.connects.clone()),
        [(OFFICE.to_owned(), "worker".to_owned(), false)]
    );
    assert_eq!(w.retry_button.label().as_deref(), Some("Try Again"));
    w.retry_button.emit_clicked();
    settle(&window);
    assert_eq!(remembered(), [false, false]);

    // Replacing it is the explicit action.
    w.password_button.emit_clicked();
    pump_for(Duration::from_millis(50));
    let dialog = alert(&window);
    dialog.emit_by_name::<()>("response", &[&"save"]);
    settle(&window);
    assert_eq!(remembered(), [false, false, true]);
    dialog.force_close();
    assert!(!window.preferences().remember_password, "never stored");

    // Without a saved password the switch shows the user's choice again.
    fake.with(|fake| fake.saved_password = false);
    window.refresh();
    settle(&window);
    assert!(w.remember.is_sensitive());
    assert!(!w.remember.is_active(), "the default preference");
    assert_eq!(
        w.remember.subtitle().as_deref(),
        Some("Stored by FortiClient, never by this app")
    );
    w.remember.set_active(true);
    w.primary.emit_clicked();
    settle(&window);
    assert_eq!(remembered(), [false, false, true, true]);
}

fn reduced_motion_finishes_fades(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    let settings = gtk::Settings::default().expect("GTK settings");
    let enabled = settings.is_gtk_enable_animations();
    settings.set_gtk_enable_animations(false);
    let w = &window.widgets;
    window.update_state(|state| state.status = connected());
    assert!(w.status_badge.has_css_class("badge-connected"));
    assert!(w.connection_glow.has_css_class("is-connected"));
    assert_eq!(w.connection_glow.opacity(), 1.0);
    assert_eq!(w.status_badge.opacity(), 1.0);
    window.update_state(|state| state.busy = Some(Busy::Operation(Operation::Disconnecting)));
    assert!(!w.connection_glow.has_css_class("is-connected"));
    assert!(w.spinner.is_visible() && !w.status_icon.is_visible());
    window.update_state(|state| {
        state.busy = None;
        state.status = Status::new(ConnectionState::Disconnected);
    });
    assert_eq!(w.connection_glow.opacity(), 0.0);
    assert_eq!(w.status_badge.opacity(), 1.0);
    assert!(w.status_badge.has_css_class("badge-idle"));
    assert!(!w.status_badge.has_css_class("badge-connected"));
    settings.set_gtk_enable_animations(enabled);
}

// ---------------------------------------------------------------------------
// Setup.

fn missing_cli_setup_and_check_again_recovery(fixture: &Fixture) {
    let fake = FakeBackend::new();
    let window = fixture.window(&fake);
    fake.with(|fake| fake.missing = true);
    window.refresh();
    settle(&window);
    window.show_setup();
    settle(&window);
    let w = &window.widgets;
    assert!(window.state().setup_required);
    assert!(w.setup_group.is_visible());
    assert!(!w.profiles_group.is_visible());
    assert!(!w.primary.is_visible());
    let setup = window.setup_dialog().expect("setup open");
    assert!(setup.check_status.text().contains("not been found"));
    assert_eq!(setup.pages.visible_child_name().as_deref(), Some("install"));
    assert!(!setup.command_panel.is_visible());
    pump_for(Duration::from_millis(100));
    let installer = setup
        .select_button
        .compute_bounds(&w.window)
        .expect("installer shown");
    let footer = setup
        .content
        .footer
        .compute_bounds(&w.window)
        .expect("footer shown");
    assert!(installer.y() + installer.height() <= footer.y());
    fake.with(|fake| fake.missing = false);
    setup.check.emit_clicked();
    settle(&window);
    assert!(!window.state().setup_required);
    assert!(w.primary.is_visible());
    assert!(setup.check_status.text().contains("ready"));
    assert_eq!(setup.pages.visible_child_name().as_deref(), Some("ready"));
    assert!(!setup.check.is_visible());
}

fn setup_checks_once_on_open_then_on_request(fixture: &Fixture) {
    let fake = FakeBackend::new();
    let window = fixture.window(&fake);
    let reads = fake.with(|fake| fake.status_reads);
    window.show_setup();
    settle(&window);
    assert_eq!(fake.with(|fake| fake.status_reads), reads + 1);
    window.show_setup();
    pump_for(Duration::from_millis(1500));
    assert_eq!(fake.with(|fake| fake.status_reads), reads + 1);
    let setup = window.setup_dialog().expect("setup open");
    setup.show_help();
    assert_eq!(setup.pages.visible_child_name().as_deref(), Some("install"));
    setup.check.emit_clicked();
    settle(&window);
    assert_eq!(fake.with(|fake| fake.status_reads), reads + 2);
}

fn installed_but_unavailable_client_shows_recovery(fixture: &Fixture) {
    let fake = FakeBackend::new();
    let window = fixture.window(&fake);
    fake.with(|fake| fake.unavailable = true);
    window.show_setup();
    settle(&window);
    let setup = window.setup_dialog().expect("setup open");
    assert!(!window.state().setup_required);
    assert_eq!(
        setup.pages.visible_child_name().as_deref(),
        Some("recovery")
    );
    assert!(setup.recovery_details.text().contains("CLI was found"));
    assert!(window.widgets.setup_help.is_visible());
    fake.with(|fake| fake.unavailable = false);
    setup.check.emit_clicked();
    settle(&window);
    assert_eq!(setup.pages.visible_child_name().as_deref(), Some("ready"));
}

fn stopped_service_recovery_puts_the_command_first(fixture: &Fixture) {
    let fake = FakeBackend::new();
    let window = fixture.window_for(&fake, ubuntu_flatpak());
    fake.with(|fake| fake.service_stopped = true);
    window.show_setup();
    settle(&window);
    let setup = window.setup_dialog().expect("setup open");
    let run = "Run this command in Terminal, then check again.";
    assert_eq!(
        setup.pages.visible_child_name().as_deref(),
        Some("recovery")
    );
    assert_eq!(
        setup.recovery_title.text(),
        "FortiClient’s service is stopped"
    );
    let service = setup.service.as_ref().expect("the service command");
    let section: gtk::Widget = service.container.clone().upcast();
    assert_eq!(setup.recovery_title.next_sibling(), Some(section.clone()));
    assert_eq!(service.description.text(), run);
    assert_eq!(setup.check_status.text(), run);
    assert!(!setup.recovery_details.is_visible());

    // Another failure keeps the usual explanation first.
    fake.with(|fake| {
        fake.service_stopped = false;
        fake.unavailable = true;
    });
    setup.check.emit_clicked();
    settle(&window);
    assert_eq!(setup.recovery_title.text(), "FortiClient needs attention");
    assert!(setup.recovery_details.text().contains("CLI was found"));
    assert_eq!(setup.recovery_details.next_sibling(), Some(section));
    assert!(
        service
            .description
            .text()
            .starts_with("FortiClient’s service may be stopped.")
    );
    assert!(setup.check_status.text().contains("not responding"));
}

/// The dialog opens on a control that works, never on Check Again while
/// its first check runs, and gives the focus back to Check Again.
fn setup_focus_starts_and_returns_on_working_controls(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| fake.missing = true);
    let window = fixture.window(&fake);
    fake.with(|fake| fake.hold_status = true);
    window.show_setup();
    pump_for(DIALOG_ANIMATION);
    let setup = window.setup_dialog().expect("setup open");
    let focus = || setup.content.dialog.focus();
    let check: gtk::Widget = setup.check.clone().upcast();
    assert!(!setup.check.is_sensitive(), "checking");
    assert_eq!(focus(), Some(setup.select_button.clone().upcast()));
    fake.with(|fake| fake.hold_status = false);
    settle(&window);

    fake.with(|fake| fake.hold_status = true);
    setup.check.emit_clicked();
    pump_for(Duration::from_millis(50));
    assert!(!setup.check.is_sensitive());
    setup.content.dialog.set_focus(gtk::Widget::NONE);
    fake.with(|fake| fake.hold_status = false);
    settle(&window);
    assert_eq!(focus(), Some(check));
    setup.close();
}

fn arch_setup_installs_from_the_aur(fixture: &Fixture) {
    let fake = FakeBackend::new();
    fake.with(|fake| fake.missing = true);
    let window = fixture.window_for(&fake, arch_gnome());
    window.show_setup();
    settle(&window);
    pump_for(DIALOG_ANIMATION);
    let setup = window.setup_dialog().expect("setup open");
    let source = &setup.download_section;
    assert!(find_label(source, "Install from the AUR").is_some());
    let builds = "This command builds and installs FortiClient from the AUR.";
    assert!(find_label(source, builds).is_some());
    assert!(find_label(source, "Download").is_none());
    assert!(find_label(source, "paru -S forticlient-vpn").is_some());
    let built = find_label(&setup.pages, "Already built the package?").expect("the label");
    assert_eq!(
        built.next_sibling(),
        Some(setup.select_button.clone().upcast())
    );
    // Copying the AUR command comes first.
    let copy = setup.repository_copy.clone().expect("its copy button");
    assert_eq!(setup.content.dialog.focus(), Some(copy.upcast()));
    setup.close();
}

fn setup_command_copy_and_fresh_setup_quits(fixture: &Fixture) {
    let window = fixture.window(&FakeBackend::new());
    window.show_setup();
    settle(&window);
    let setup = window.setup_dialog().expect("setup open");
    assert_eq!(setup.pages.visible_child_name().as_deref(), Some("ready"));
    setup.show_help();
    assert_eq!(setup.pages.visible_child_name().as_deref(), Some("install"));
    let installer = fixture.config_home.join("FortiClient installer.deb");
    std::fs::write(&installer, b"").expect("create the installer");
    setup.set_package(Path::new("relative.deb"));
    assert!(
        setup
            .content
            .error
            .text()
            .contains(".deb file from this computer")
    );
    setup.set_package(&installer);
    assert!(setup.command_panel.is_visible());
    assert!(!setup.download_section.is_visible());
    assert!(!setup.content.error.is_visible());
    setup.copy_button.emit_clicked();
    let copied = clipboard_text(&setup.copy_button).unwrap_or_default();
    assert_eq!(
        copied,
        format!("sudo apt install '{}'", installer.display())
    );
    assert_eq!(setup.copy_button.label().as_deref(), Some("Command Copied"));

    // A setup screen that never saw FortiClient quits without disconnecting.
    let missing = FakeBackend::new();
    missing.with(|fake| fake.missing = true);
    let fresh = fixture.window(&missing);
    assert!(fresh.state().setup_required);
    fresh.request_exit();
    assert_eq!(fixture.quits.get(), 1);
    assert_eq!(missing.with(|fake| fake.disconnects), 0);
}

fn refresh_only_on_launch_and_on_request(fixture: &Fixture) {
    let fake = FakeBackend::new();
    let window = fixture.window(&fake);
    assert_eq!(fake.with(|fake| fake.status_reads), 1);
    window.present();
    pump_for(Duration::from_millis(1500));
    assert_eq!(fake.with(|fake| fake.status_reads), 1);
    window.widgets.refresh_button.emit_clicked();
    settle(&window);
    assert_eq!(fake.with(|fake| fake.status_reads), 2);
    // Ctrl+R and F5 refresh the application's own window.
    let main = fixture.app.window().expect("the application's window");
    settle(&main);
    let reads = fixture.main_backend.with(|fake| fake.status_reads);
    fixture.app.application.activate_action("refresh", None);
    settle(&main);
    assert_eq!(
        fixture.main_backend.with(|fake| fake.status_reads),
        reads + 1
    );
    let accels = fixture.app.application.accels_for_action("app.refresh");
    assert_eq!(accels, ["<Control>r", "F5"]);
}

// ---------------------------------------------------------------------------
// Resources.

/// Every icon the UI names comes from the bundle when the desktop's theme
/// lacks it (KDE's Breeze and others do not fall back to Adwaita).
fn bundled_icons_resolve_without_a_theme(_: &Fixture) {
    // Only the system's hicolor fallback theme and this app's bundle; not
    // even the icons GTK bundles for its own widgets.
    let theme = gtk::IconTheme::new();
    theme.set_resource_path(&["/io/github/fortivpn/Utility/icons/"]);
    theme.set_theme_name(Some("hicolor"));
    let missing: Vec<&str> = [
        "open-menu-symbolic",
        "view-more-symbolic",
        "view-refresh-symbolic",
        "list-add-symbolic",
        "dialog-information-symbolic",
        "document-edit-symbolic",
        "dialog-password-symbolic",
        "user-trash-symbolic",
        "network-offline-symbolic",
        "emblem-ok-symbolic",
        "dialog-warning-symbolic",
        "network-vpn-symbolic",
        "network-server-symbolic",
        "folder-open-symbolic",
        "edit-copy-symbolic",
        "document-save-symbolic",
        "edit-clear-all-symbolic",
        "preferences-system-symbolic",
        "utilities-terminal-symbolic",
        "io.github.fortivpn.Utility",
        "io.github.fortivpn.Utility-connected",
        forti_vpn::tray::CONNECTED_ICON,
        forti_vpn::tray::DISCONNECTED_ICON,
    ]
    .into_iter()
    .filter(|name| !theme.has_icon(name))
    .collect();
    assert!(missing.is_empty(), "not bundled: {missing:?}");
    let style = gio::resources_lookup_data(
        "/io/github/fortivpn/Utility/style.css",
        gio::ResourceLookupFlags::NONE,
    )
    .expect("the stylesheet is bundled");
    assert!(String::from_utf8_lossy(&style).contains(".status-badge.badge-connected"));
}

// ---------------------------------------------------------------------------
// Previews of the demo app for a visual check.

fn previews(fixture: &Fixture) {
    let Some(directory) = std::env::var_os("FORTI_VPN_PREVIEW_DIR").map(PathBuf::from) else {
        println!("    (set FORTI_VPN_PREVIEW_DIR to render previews)");
        return;
    };
    std::fs::create_dir_all(&directory).expect("create the preview directory");
    let gtk_settings = gtk::Settings::default().expect("GTK settings");
    let font = gtk_settings.gtk_font_name();
    for (appearance, name) in [(Appearance::Light, "light"), (Appearance::Dark, "dark")] {
        for (suffix, width, large) in [("480", None, false), ("360-large", Some(360), true)] {
            let size = if large {
                Some("Sans 16")
            } else {
                font.as_deref()
            };
            gtk_settings.set_gtk_font_name(size);
            let previews = Previews {
                fixture,
                directory: directory.clone(),
                prefix: format!("{name}-{suffix}"),
                width,
            };
            previews.render(appearance);
        }
    }
    gtk_settings.set_gtk_font_name(font.as_deref());
}

/// One set of previews: an appearance at a width and text size.
struct Previews<'a> {
    fixture: &'a Fixture,
    directory: PathBuf,
    /// Starts every file name, e.g. `light-480`.
    prefix: String,
    width: Option<i32>,
}

impl Previews<'_> {
    fn render(&self, appearance: Appearance) {
        let window = self.window(Arc::new(DemoBackend::new()), None);
        window
            .update_preferences(|preferences| preferences.set_appearance(appearance))
            .expect("store the appearance");
        pump_for(Duration::from_millis(300));
        self.connection(&window);
        self.dialogs(&window);
        self.other_windows();
    }

    /// A new window on `backend`, for `platform` when given, at the width.
    fn window(&self, backend: Arc<dyn Backend>, platform: Option<Platform>) -> Rc<MainWindow> {
        let mut services: Services = self.fixture.app.services();
        services.backend = backend;
        if let Some(platform) = platform {
            services.platform = platform;
        }
        let window = self.fixture.show(services);
        settle(&window);
        if let Some(width) = self.width {
            resize(&window, width, 750);
        }
        pump_for(Duration::from_millis(400));
        window
    }

    fn save(&self, window: &MainWindow, state: &str) {
        let name = format!("{}-{state}.png", self.prefix);
        save_png(window, &self.directory.join(name));
    }

    /// Disconnected, signing in, connected, and a failed connection.
    fn connection(&self, window: &Rc<MainWindow>) {
        self.save(window, "disconnected");
        // The demo's first profile, as if FortiClient had saved its password.
        set_saved_password(window, "Enabled");
        pump_for(Duration::from_millis(300));
        self.save(window, "saved-password");
        set_saved_password(window, "Disabled");
        window.primary_action();
        pump_until("the password prompt", || {
            window.challenge_dialog().is_some()
        });
        pump_for(Duration::from_millis(400));
        self.save(window, "authentication");
        let respond = |text: &str| {
            let prompt = window.challenge_dialog().expect("prompt open");
            prompt.entry.set_text(text);
            prompt.proceed.emit_clicked();
        };
        respond("demo password");
        pump_until("the code prompt", || {
            window.with_state(|state| state.busy == Some(Busy::AwaitingAuthentication))
                && window.challenge_dialog().is_some()
        });
        respond("123456");
        settle(window);
        pump_for(Duration::from_millis(400));
        self.save(window, "connected");
        window.update_state(|state| {
            state.status = Status::new(ConnectionState::Disconnected);
            state.error = "FortiClient could not resolve the VPN server address. Check your \
                           network and profile."
                .to_owned();
            state.error_origin = Some(Operation::Connecting);
            state.warnings = vec!["Your password could not be saved.".to_owned()];
        });
        pump_for(Duration::from_millis(400));
        self.save(window, "error");
        window.update_state(|state| {
            state.error.clear();
            state.warnings.clear();
        });
    }

    /// Settings, Diagnostics, setup help, and the profile editor.
    fn dialogs(&self, window: &Rc<MainWindow>) {
        // As the application reports it when there is no tray.
        window.indicator_changed(false);
        window.show_settings();
        pump_for(Duration::from_millis(600));
        self.save(window, "settings");
        if let Some(settings) = window.settings_dialog() {
            settings.show_diagnostics();
        }
        pump_for(Duration::from_millis(600));
        self.save(window, "diagnostics");
        if let Some(settings) = window.settings_dialog() {
            settings.close();
        }
        pump_for(Duration::from_millis(400));
        window.show_setup();
        settle(window);
        if let Some(setup) = window.setup_dialog() {
            setup.show_help();
        }
        pump_for(Duration::from_millis(600));
        self.save(window, "setup");
        if let Some(setup) = window.setup_dialog() {
            setup.close();
        }
        pump_for(Duration::from_millis(400));
        window.update_state(|state| state.status = Status::new(ConnectionState::Disconnected));
        window.profile_editor(window.selected());
        pump_for(Duration::from_millis(600));
        self.save(window, "editor");
    }

    /// Windows in other situations: no FortiClient, no profiles, no tray,
    /// Arch Linux's setup, and a stopped service.
    fn other_windows(&self) {
        let missing = FakeBackend::new();
        missing.with(|fake| fake.missing = true);
        let window = self.window(Arc::clone(&missing) as Arc<dyn Backend>, None);
        self.save(&window, "get-started");
        let empty = FakeBackend::new();
        empty.with(|fake| fake.no_profiles = true);
        let window = self.window(empty, None);
        self.save(&window, "empty-profiles");
        let window = self.window(Arc::new(DemoBackend::new()), None);
        window.indicator_changed(false);
        window.widgets.window.close();
        pump_for(Duration::from_millis(400));
        self.save(&window, "no-tray");
        let window = self.window(missing, Some(arch_gnome()));
        self.setup(&window, "setup-arch");
        let stopped = FakeBackend::new();
        stopped.with(|fake| fake.service_stopped = true);
        let window = self.window(stopped, Some(ubuntu_flatpak()));
        self.setup(&window, "setup-stopped");
    }

    fn setup(&self, window: &Rc<MainWindow>, state: &str) {
        window.show_setup();
        settle(window);
        pump_for(Duration::from_millis(600));
        self.save(window, state);
        if let Some(setup) = window.setup_dialog() {
            setup.close();
        }
    }
}

/// Mark the window's first profile as one FortiClient saved a password
/// for (`Enabled`) or not (`Disabled`).
fn set_saved_password(window: &Rc<MainWindow>, saved: &str) {
    window.update_state(|state| {
        for (key, value) in &mut state.profiles[0].fields {
            if key == "Save Password" {
                saved.clone_into(value);
            }
        }
    });
}

fn save_png(window: &MainWindow, path: &Path) {
    let native = &window.widgets.window;
    let (width, height) = (native.width(), native.height());
    let paintable = gtk::WidgetPaintable::new(Some(native));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot.to_node().expect("the window drew something");
    let renderer = native.renderer().expect("the window has a renderer");
    let texture = renderer.render_texture(&node, None);
    texture.save_to_png(path).expect("write the preview");
    println!("    wrote {} ({width}×{height})", path.display());
}
