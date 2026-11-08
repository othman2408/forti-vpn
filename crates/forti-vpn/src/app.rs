//! Application start-up: a port of `Application` and `main()` in the Python
//! `forti_vpn/ui.py`.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Once};

use adw::prelude::*;
use forti_vpn_core::backend::Backend;
use forti_vpn_core::cli::{Cli, CliConfig};
use forti_vpn_core::demo::{DEMO_VARIABLE, DemoBackend};
use forti_vpn_core::platform::Platform;
use forti_vpn_core::preferences::Preferences;
use forti_vpn_core::{APP_ID, running_as_root};
use gtk::{gio, glib};

use crate::tray::{Tray, TrayActions};
use crate::window::{MainWindow, Services};

/// Where the bundled `style.css` (loaded by `AdwApplication`) and `icons/`
/// (added to the icon theme by `GtkApplication`) live. Set explicitly so an
/// application ID other than [`APP_ID`], as in tests, finds them too.
const RESOURCE_BASE_PATH: &str = "/io/github/fortivpn/Utility";
const DISABLE_TRAY_VARIABLE: &str = "FORTI_VPN_DISABLE_INDICATOR";

pub(crate) fn run() -> glib::ExitCode {
    if running_as_root() {
        eprintln!("Run Forti VPN as your regular desktop user, not with sudo.");
        return glib::ExitCode::FAILURE;
    }
    install_panic_hook();
    // X11 window managers match WM_CLASS against the desktop file name.
    glib::set_prgname(Some(APP_ID));
    register_resources();
    let app = App::new(Config::from_environment());
    signals::quit_on_termination(&app.application);
    // Like Python's `run(None)`: the app takes no command-line options.
    app.application.run_with_args::<&str>(&[])
}

/// In release builds a panic prints only where it happened: the message
/// could contain profile names or other user data.
fn install_panic_hook() {
    if cfg!(debug_assertions) {
        return;
    }
    std::panic::set_hook(Box::new(|info| match info.location() {
        Some(location) => eprintln!(
            "Forti VPN stopped unexpectedly at {}:{}.",
            location.file(),
            location.line()
        ),
        None => eprintln!("Forti VPN stopped unexpectedly."),
    }));
}

/// Register the bundled stylesheet and icons once per process.
pub(crate) fn register_resources() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        // Only a corrupt binary fails here; the app still works unstyled.
        if let Err(error) = gio::resources_register_include!("forti-vpn.gresource") {
            eprintln!("Forti VPN could not load its icons and styles: {error}");
        }
    });
}

/// How an [`App`] is set up; tests replace the backend and quitting.
pub struct Config {
    pub application_id: String,
    pub flags: gio::ApplicationFlags,
    pub backend: Arc<dyn Backend>,
    pub platform: Platform,
    /// The backend is the demo.
    pub demo: bool,
    /// Show a tray icon.
    pub tray: bool,
    /// Called instead of quitting the application, when set.
    pub quit: Option<Rc<dyn Fn()>>,
}

impl Config {
    fn from_environment() -> Self {
        let demo = std::env::var_os(DEMO_VARIABLE).is_some_and(|value| value == "1");
        let backend: Arc<dyn Backend> = if demo {
            Arc::new(DemoBackend::new())
        } else {
            Arc::new(Cli::new(CliConfig::from_environment()))
        };
        Self {
            application_id: APP_ID.to_owned(),
            flags: gio::ApplicationFlags::empty(),
            backend,
            platform: Platform::detect(),
            demo,
            // Python treated an empty value as unset.
            tray: std::env::var_os(DISABLE_TRAY_VARIABLE).is_none_or(|value| value.is_empty()),
            quit: None,
        }
    }
}

/// The `adw::Application` and the one main window it shows.
pub struct App {
    pub application: adw::Application,
    config: Config,
    window: RefCell<Option<Rc<MainWindow>>>,
    /// Keeps the application running while its window is hidden in the
    /// tray; released at shutdown.
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
}

impl App {
    pub fn new(config: Config) -> Rc<Self> {
        let application = adw::Application::builder()
            .application_id(config.application_id.as_str())
            .flags(config.flags)
            .resource_base_path(RESOURCE_BASE_PATH)
            .build();
        let app = Rc::new(Self {
            application,
            config,
            window: RefCell::default(),
            hold: RefCell::default(),
        });
        app.connect_signals();
        app
    }

    pub fn window(&self) -> Option<Rc<MainWindow>> {
        self.window.borrow().clone()
    }

    /// The services a window of this application uses.
    pub fn services(&self) -> Services {
        let quit = self.config.quit.clone().unwrap_or_else(|| {
            let application = self.application.downgrade();
            Rc::new(move || {
                if let Some(application) = application.upgrade() {
                    application.quit();
                }
            })
        });
        Services {
            backend: Arc::clone(&self.config.backend),
            platform: self.config.platform.clone(),
            preferences: Preferences::load(Preferences::default_path()),
            quit,
            demo: self.config.demo,
        }
    }

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.application.connect_startup(move |_| {
            if let Some(app) = weak.upgrade() {
                app.startup();
            }
        });
        let weak = Rc::downgrade(self);
        self.application.connect_activate(move |_| {
            if let Some(app) = weak.upgrade() {
                app.activate();
            }
        });
        let weak = Rc::downgrade(self);
        self.application.connect_shutdown(move |_| {
            if let Some(app) = weak.upgrade() {
                app.shutdown();
            }
        });
    }

    fn startup(self: &Rc<Self>) {
        self.hold.replace(Some(self.application.hold()));
        gtk::Window::set_default_icon_name(APP_ID);
        self.add_actions();
    }

    fn add_actions(self: &Rc<Self>) {
        let actions: [(&str, WindowAction); 5] = [
            ("settings", MainWindow::show_settings),
            ("setup", MainWindow::show_setup),
            ("about", MainWindow::show_about),
            ("quit", MainWindow::request_exit),
            ("refresh", MainWindow::refresh),
        ];
        for (name, method) in actions {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(window) = weak.upgrade().and_then(|app| app.window()) {
                    method(&window);
                }
            });
            self.application.add_action(&action);
        }
        self.application
            .set_accels_for_action("app.settings", &["<Primary>comma"]);
        self.application
            .set_accels_for_action("app.quit", &["<Primary>q"]);
        self.application
            .set_accels_for_action("app.refresh", &["<Primary>r", "F5"]);
    }

    fn activate(self: &Rc<Self>) {
        let window = self.window().unwrap_or_else(|| self.open_window());
        window.present();
    }

    fn open_window(&self) -> Rc<MainWindow> {
        let window = MainWindow::new(&self.application, self.services());
        self.window.replace(Some(Rc::clone(&window)));
        match self.tray(&window) {
            // The tray reports whether a host shows it.
            Some(tray) => window.attach_tray(tray),
            None => window.indicator_changed(false),
        }
        window
    }

    /// The tray icon, unless it is turned off or there is no session bus
    /// to export it on: then there is no tray to hide into.
    fn tray(&self, window: &Rc<MainWindow>) -> Option<Tray> {
        if !self.config.tray {
            return None;
        }
        Tray::new(tray_actions(window)).ok()
    }

    fn shutdown(&self) {
        let window = self.window.take();
        if let Some(window) = window {
            window.shutdown();
        }
        self.hold.take();
    }
}

/// A main-window method that menus, shortcuts, and the tray invoke.
type WindowAction = fn(&Rc<MainWindow>);

fn tray_actions(window: &Rc<MainWindow>) -> TrayActions {
    let call = |method: WindowAction| -> Rc<dyn Fn()> {
        let weak = Rc::downgrade(window);
        Rc::new(move || {
            if let Some(window) = weak.upgrade() {
                method(&window);
            }
        })
    };
    let show = Rc::downgrade(window);
    let settings = Rc::downgrade(window);
    let availability = Rc::downgrade(window);
    TrayActions {
        show: Rc::new(move |token| {
            if let Some(window) = show.upgrade() {
                window.present_with_token(token);
            }
        }),
        settings: Rc::new(move |token| {
            if let Some(window) = settings.upgrade() {
                window.show_settings_with_token(token);
            }
        }),
        disconnect: call(MainWindow::disconnect),
        quit: call(MainWindow::request_exit),
        availability: Rc::new(move |available| {
            if let Some(window) = availability.upgrade() {
                window.indicator_changed(available);
            }
        }),
    }
}

/// SIGINT and SIGTERM quit the application cleanly, closing the tray and
/// leaving an established VPN connection alone.
mod signals {
    use adw::prelude::*;
    use gtk::glib::ControlFlow;
    use rustix::process::Signal;

    /// Must be called on the thread that runs the default main context,
    /// where GLib then dispatches the signals.
    pub(super) fn quit_on_termination(application: &adw::Application) {
        for signal in [Signal::INT, Signal::TERM] {
            let application = application.downgrade();
            // The sources last as long as the process.
            glib_unix::unix_signal_add_local(signal.as_raw(), move || {
                if let Some(application) = application.upgrade() {
                    application.quit();
                }
                // Keep the source: a later signal quits again.
                ControlFlow::Continue
            });
        }
    }
}
