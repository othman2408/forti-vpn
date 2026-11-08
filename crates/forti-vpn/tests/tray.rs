//! Real session-bus protocol checks for the tray, with callbacks that never
//! touch a VPN: a port of `tests/test_indicator.py`, plus the watcher, icon,
//! activation token and close behaviour.
//!
//! The tests export tray items and take the StatusNotifierWatcher name, which
//! a desktop's panel would notice, so they are opt-in and run only on a
//! private session bus:
//!
//! ```text
//! FORTI_VPN_DBUS_TESTS=1 dbus-run-session -- cargo test -p forti-vpn --test tray
//! ```
//!
//! With `FORTI_VPN_DBUS_TESTS` unset, empty or 0 they are skipped; any value
//! but those and 1 fails, so a typo cannot skip them. With 1 they fail rather
//! than pass without testing anything when there is no session bus, and they
//! refuse to run on the desktop's own session bus
//! (`unix:path=$XDG_RUNTIME_DIR/bus`), so the fake watcher never replaces the
//! real panel's. The tests that start the fake watcher also fail when another
//! watcher owns its name. Add `G_DEBUG=fatal-warnings` to fail on GLib
//! warnings too. GTK is never initialised: the tray only needs GIO. A second
//! bus connection plays the panel host, and a fake
//! `org.kde.StatusNotifierWatcher` plays the panel.
use std::cell::{Cell, OnceCell, RefCell};
use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::{Mutex, Once, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use forti_vpn::tray::{CONNECTED_ICON, DISCONNECTED_ICON, Tray, TrayActions};
use gtk::gio;
use gtk::glib;
use gtk::glib::prelude::*;

const ITEM: &str = "org.kde.StatusNotifierItem";
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
const MENU_PATH: &str = "/Menu";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const WATCHER_XML: &str = r#"<node><interface name="org.kde.StatusNotifierWatcher">
<method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method>
<property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
<signal name="StatusNotifierHostRegistered"/><signal name="StatusNotifierHostUnregistered"/>
</interface></node>"#;
const TIMEOUT: Duration = Duration::from_secs(5);
/// How to run these tests.
const RUN: &str = "FORTI_VPN_DBUS_TESTS=1 dbus-run-session -- cargo test -p forti-vpn --test tray";

type Reply = Result<glib::Variant, glib::Error>;
/// A reply that may not have arrived yet.
type Pending = Rc<RefCell<Option<Reply>>>;

/// Every tray in this process exports the same object paths on the same
/// shared session connection, so the tests take turns.
static TURN: Mutex<()> = Mutex::new(());
static SKIP_NOTICE: Once = Once::new();

/// Whether the bus tests were asked for, with a private session bus to run
/// on. Panics when they were asked for without one.
fn bus_tests_requested() -> bool {
    let value = std::env::var_os("FORTI_VPN_DBUS_TESTS").unwrap_or_default();
    match value.to_str() {
        Some("" | "0") => {
            SKIP_NOTICE.call_once(|| {
                // Past the harness's output capture, so that a green run
                // says it tested nothing.
                let _ = writeln!(io::stderr(), "D-Bus tray tests skipped (opt-in): {RUN}");
            });
            false
        }
        Some("1") => {
            let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
            assert!(
                !address.is_empty(),
                "FORTI_VPN_DBUS_TESTS=1, but DBUS_SESSION_BUS_ADDRESS is unset: run {RUN}"
            );
            // Without XDG_RUNTIME_DIR, the desktop bus is still at the
            // systemd default for this user.
            let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
                .filter(|dir| !dir.is_empty())
                .unwrap_or_else(|| {
                    format!("/run/user/{}", rustix::process::getuid().as_raw()).into()
                });
            assert!(
                !is_desktop_bus(&address, Some(&runtime_dir)),
                "refusing to run on the desktop's session bus ({address}), where the fake \
                 watcher would replace the panel's: run {RUN}"
            );
            true
        }
        _ => panic!("FORTI_VPN_DBUS_TESTS is {value:?}, not 1 or 0: run {RUN}"),
    }
}

/// Whether the D-Bus `address` is the desktop session's own bus,
/// `unix:path=$XDG_RUNTIME_DIR/bus`. An address lists alternatives separated
/// by `;`, each a transport with `key=value` options separated by `,`.
fn is_desktop_bus(address: &str, runtime_dir: Option<&OsStr>) -> bool {
    let Some(runtime_dir) = runtime_dir.filter(|dir| !dir.is_empty()) else {
        return false;
    };
    let desktop = Path::new(runtime_dir).join("bus");
    address
        .split(';')
        .filter_map(|entry| entry.strip_prefix("unix:"))
        .flat_map(|options| options.split(','))
        .filter_map(|option| option.strip_prefix("path="))
        .any(|path| Path::new(path) == desktop)
}

/// Run `test` against the session bus, in a main context of its own that is
/// the thread default, as the GTK main context is for the app.
fn with_session_bus(test: impl FnOnce(&Bus)) {
    if !bus_tests_requested() {
        return;
    }
    let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            let bus = Bus {
                context: context.clone(),
                host: connect(),
            };
            test(&bus);
            // Let GDBus release what the closed trays left behind.
            bus.drain();
        })
        .expect("own a fresh main context");
}

/// A private connection to the session bus, separate from the tray's.
fn connect() -> gio::DBusConnection {
    let address = gio::dbus_address_get_for_bus_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .expect("session bus address");
    let flags = gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
        | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION;
    gio::DBusConnection::for_address_sync(&address, flags, None, gio::Cancellable::NONE)
        .expect("connect to the session bus")
}

struct Bus {
    context: glib::MainContext,
    /// Plays the panel host that talks to the tray.
    host: gio::DBusConnection,
}

impl Bus {
    fn tray(&self) -> (Tray, Recorder) {
        let recorder = Recorder::default();
        let tray = Tray::new(recorder.actions()).expect("export the tray");
        (tray, recorder)
    }

    /// Call the tray as a panel host does, iterating the main context the way
    /// the GTK main loop would until the reply and everything it queued ran.
    fn call(
        &self,
        tray: &Tray,
        path: &str,
        interface: &str,
        method: &str,
        parameters: impl ToVariant,
    ) -> Reply {
        let destination = tray.bus_name().expect("the tray has a bus name");
        self.call_to(&destination, path, interface, method, parameters)
    }

    fn call_to(
        &self,
        destination: &str,
        path: &str,
        interface: &str,
        method: &str,
        parameters: impl ToVariant,
    ) -> Reply {
        let reply = self.send(
            destination,
            path,
            interface,
            method,
            &parameters.to_variant(),
        );
        self.pump_until(method, || reply.borrow().is_some());
        self.drain();
        reply.take().expect("a reply")
    }

    /// Send `calls` to the tray back to back, as panels send a token and the
    /// activation it is for, then wait for every reply and what they queued.
    fn pipeline(&self, tray: &Tray, calls: &[(&str, &str, &str, glib::Variant)]) -> Vec<Reply> {
        let destination = tray.bus_name().expect("the tray has a bus name");
        let pending: Vec<Pending> = calls
            .iter()
            .map(|(path, interface, method, parameters)| {
                self.send(&destination, path, interface, method, parameters)
            })
            .collect();
        self.pump_until("every reply", || {
            pending.iter().all(|reply| reply.borrow().is_some())
        });
        self.drain();
        let replies = pending.iter().map(|reply| reply.take().expect("a reply"));
        replies.collect()
    }

    /// Call a method without waiting for its reply.
    fn send(
        &self,
        destination: &str,
        path: &str,
        interface: &str,
        method: &str,
        parameters: &glib::Variant,
    ) -> Pending {
        let reply = Pending::default();
        let slot = Rc::clone(&reply);
        self.host.call(
            Some(destination),
            path,
            interface,
            method,
            Some(parameters),
            None,
            gio::DBusCallFlags::NONE,
            3000,
            gio::Cancellable::NONE,
            move |result| *slot.borrow_mut() = Some(result),
        );
        reply
    }

    fn menu(&self, tray: &Tray, method: &str, args: impl ToVariant) -> Reply {
        self.call(tray, MENU_PATH, MENU, method, args)
    }

    fn item(&self, tray: &Tray, method: &str, args: impl ToVariant) -> Reply {
        self.call(tray, ITEM_PATH, ITEM, method, args)
    }

    fn item_property(&self, tray: &Tray, name: &str) -> glib::Variant {
        let reply = self.call(tray, ITEM_PATH, PROPERTIES, "Get", (ITEM, name));
        let value = reply.expect(name).child_value(0);
        value.as_variant().expect("a (v) reply")
    }

    fn icon(&self, tray: &Tray) -> String {
        let icon = self.item_property(tray, "IconName");
        icon.str().expect("IconName is a string").to_owned()
    }

    /// A round trip to the tray's connection: signals it sent before this
    /// call have arrived once it returns.
    fn ping(&self, tray: &Tray) {
        let reply = self.call(tray, "/", "org.freedesktop.DBus.Peer", "Ping", ());
        reply.expect("Ping");
    }

    /// Record the signals the tray emits, as `LayoutUpdated(revision, parent)`
    /// or by name.
    fn signals(&self, tray: &Tray) -> (gio::SignalSubscription, Rc<RefCell<Vec<String>>>) {
        let signals = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&signals);
        let subscription = self.host.subscribe_to_signal(
            tray.bus_name().as_deref(),
            None,
            None,
            None,
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let entry = match signal.parameters.get::<(u32, i32)>() {
                    Some((revision, parent)) => {
                        format!("{}({revision}, {parent})", signal.signal_name)
                    }
                    None => signal.signal_name.to_owned(),
                };
                log.borrow_mut().push(entry);
            },
        );
        // The match rule is in place once a later call on this connection returns.
        self.ping(tray);
        (subscription, signals)
    }

    fn pump_until(&self, what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            if !self.context.iteration(false) {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// Iterate for a while, for checks that something does not happen.
    fn pump_for(&self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            if !self.context.iteration(false) {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn drain(&self) {
        while self.context.iteration(false) {}
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.host.close_sync(gio::Cancellable::NONE);
    }
}

/// [`TrayActions`] that record what ran, opening the window as `show` or
/// `show <token>`, and Settings as `settings` or `settings <token>`.
#[derive(Default)]
struct Recorder {
    events: Rc<RefCell<Vec<String>>>,
    availability: Rc<RefCell<Vec<bool>>>,
}

impl Recorder {
    fn actions(&self) -> TrayActions {
        let availability = Rc::clone(&self.availability);
        TrayActions {
            show: self.presenter("show"),
            settings: self.presenter("settings"),
            disconnect: self.event("disconnect"),
            quit: self.event("quit"),
            availability: Rc::new(move |available| availability.borrow_mut().push(available)),
        }
    }

    fn event(&self, name: &'static str) -> Rc<dyn Fn()> {
        let events = Rc::clone(&self.events);
        Rc::new(move || events.borrow_mut().push(name.to_owned()))
    }

    /// An action that presents the window, recorded as `name` or
    /// `name <token>`.
    fn presenter(&self, name: &'static str) -> Rc<dyn Fn(Option<String>)> {
        let events = Rc::clone(&self.events);
        Rc::new(move |token: Option<String>| {
            let event = token.map_or_else(|| name.to_owned(), |token| format!("{name} {token}"));
            events.borrow_mut().push(event);
        })
    }

    fn events(&self) -> Vec<String> {
        self.events.borrow().clone()
    }

    fn availability(&self) -> Vec<bool> {
        self.availability.borrow().clone()
    }

    /// Whether actions from [`Recorder::actions`] are still alive.
    fn is_held(&self) -> bool {
        Rc::strong_count(&self.events) > 1 || Rc::strong_count(&self.availability) > 1
    }
}

/// A StatusNotifierWatcher that is there, or not, like a panel.
struct FakeWatcher {
    connection: gio::DBusConnection,
    object: Option<gio::RegistrationId>,
    host: Rc<Cell<bool>>,
    /// `(sender, service)` of every RegisterStatusNotifierItem call.
    items: Rc<RefCell<Vec<(String, String)>>>,
}

impl FakeWatcher {
    /// Own the watcher name. Fails when something else, such as a real
    /// panel, owns it: the test would otherwise check nothing.
    fn start(host: bool) -> Self {
        let connection = connect();
        let node = gio::DBusNodeInfo::for_xml(WATCHER_XML).expect("watcher XML");
        let info = node.lookup_interface(WATCHER).expect("watcher interface");
        let host = Rc::new(Cell::new(host));
        let items = Rc::new(RefCell::new(Vec::new()));
        let registered = Rc::clone(&items);
        let shown = Rc::clone(&host);
        let object = connection
            .register_object(WATCHER_PATH, &info)
            .method_call(move |_, sender, _, _, _, parameters, invocation| {
                let service = parameters.get::<(String,)>().map(|(service,)| service);
                let sender = sender.unwrap_or_default().to_owned();
                registered
                    .borrow_mut()
                    .push((sender, service.unwrap_or_default()));
                invocation.return_value(None);
            })
            .property(move |_, _, _, _, _| shown.get().to_variant())
            .build()
            .expect("export the watcher");
        let watcher = Self {
            connection,
            object: Some(object),
            host,
            items,
        };
        let do_not_queue = 4u32;
        let reply = watcher
            .connection
            .call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "RequestName",
                Some(&(WATCHER, do_not_queue).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                3000,
                gio::Cancellable::NONE,
            )
            .expect("RequestName");
        let primary_owner = 1u32;
        assert_eq!(
            reply.get::<(u32,)>(),
            Some((primary_owner,)),
            "another StatusNotifierWatcher owns {WATCHER}: run {RUN}"
        );
        watcher
    }

    fn set_host(&self, registered: bool) {
        self.host.set(registered);
        let signal = if registered {
            "StatusNotifierHostRegistered"
        } else {
            "StatusNotifierHostUnregistered"
        };
        self.connection
            .emit_signal(None, WATCHER_PATH, WATCHER, signal, None)
            .expect("emit the host signal");
    }

    /// The services `tray` registered. Any item on the bus may register once
    /// the watcher appears, so only the tray's own registrations count.
    fn items_of(&self, tray: &Tray) -> Vec<String> {
        let name = tray.bus_name().expect("the tray has a bus name");
        let items = self.items.borrow();
        let own = items.iter().filter(|(sender, _)| *sender == name);
        own.map(|(_, service)| service.clone()).collect()
    }
}

impl Drop for FakeWatcher {
    /// Leave like a panel that exits: the name goes with the connection.
    fn drop(&mut self) {
        if let Some(object) = self.object.take() {
            let _ = self.connection.unregister_object(object);
        }
        let _ = self.connection.close_sync(gio::Cancellable::NONE);
    }
}

/// `Event` arguments for a click on menu item `id`.
fn click(id: i32) -> (i32, &'static str, glib::Variant, u32) {
    (id, "clicked", "unused".to_variant(), 0)
}

fn all() -> Vec<String> {
    Vec::new()
}

#[test]
fn the_desktop_session_bus_is_recognised() {
    let runtime = Some(OsStr::new("/run/user/1000"));
    assert!(is_desktop_bus("unix:path=/run/user/1000/bus", runtime));
    assert!(is_desktop_bus(
        "unix:abstract=/tmp/x;unix:path=/run/user/1000/bus,guid=0123",
        runtime
    ));
    for private in [
        "unix:path=/tmp/dbus-AbCdEf,guid=0123",
        "unix:abstract=/tmp/dbus-AbCdEf",
        "unix:path=/run/user/1000/bus.private",
        "tcp:host=localhost,port=4242",
    ] {
        assert!(!is_desktop_bus(private, runtime), "{private}");
    }
    assert!(!is_desktop_bus("unix:path=/run/user/1000/bus", None));
    assert!(!is_desktop_bus("unix:path=/bus", Some(OsStr::new(""))));
}

#[test]
fn layout_and_status_properties() {
    with_session_bus(|bus| {
        let (tray, _) = bus.tray();
        tray.update("Connected", true, true);
        let layout = bus
            .menu(&tray, "GetLayout", (0i32, -1i32, all()))
            .unwrap()
            .to_string();
        assert!(layout.contains("Disconnect and Quit"), "{layout}");
        assert!(layout.contains("Connected"), "{layout}");
        let properties = bus
            .call(&tray, ITEM_PATH, PROPERTIES, "GetAll", (ITEM,))
            .unwrap();
        let properties = properties.to_string();
        assert!(properties.contains(CONNECTED_ICON), "{properties}");
        assert!(properties.contains("Connected"), "{properties}");
        assert!(properties.contains("objectpath '/Menu'"), "{properties}");
    });
}

#[test]
fn menu_callbacks_and_activation() {
    with_session_bus(|bus| {
        let (tray, recorder) = bus.tray();
        tray.update("Connected", true, true);
        let expected = [(2, "show"), (3, "settings"), (4, "disconnect"), (6, "quit")];
        for (id, event) in expected {
            bus.menu(&tray, "Event", click(id)).unwrap();
            assert_eq!(recorder.events().last().map(String::as_str), Some(event));
        }
        tray.update("Disconnected", false, false);
        recorder.events.borrow_mut().clear();
        bus.menu(&tray, "Event", click(4)).unwrap();
        assert_eq!(recorder.events(), Vec::<String>::new());
        bus.item(&tray, "Activate", (0i32, 0i32)).unwrap();
        assert_eq!(recorder.events(), ["show"]);
    });
}

#[test]
fn filtered_properties() {
    with_session_bus(|bus| {
        let (tray, _) = bus.tray();
        tray.update("Connected", true, true);
        let reply = bus.menu(&tray, "GetGroupProperties", (vec![2i32, 3], vec!["label"]));
        let output = reply.unwrap().to_string();
        assert!(output.contains("Open Forti VPN"), "{output}");
        assert!(!output.contains("enabled"), "{output}");
    });
}

#[test]
fn every_icon_activation_opens_the_window_and_scrolling_does_nothing() {
    with_session_bus(|bus| {
        let (tray, recorder) = bus.tray();
        for method in ["Activate", "SecondaryActivate", "ContextMenu"] {
            bus.item(&tray, method, (10i32, 20i32)).unwrap();
        }
        bus.item(&tray, "Scroll", (1i32, "vertical")).unwrap();
        assert_eq!(recorder.events(), ["show", "show", "show"]);
    });
}

#[test]
fn presenting_the_window_passes_on_the_hosts_activation_token() {
    with_session_bus(|bus| {
        let (tray, recorder) = bus.tray();
        let provide = |token: &str| {
            let reply = bus.item(&tray, "ProvideXdgActivationToken", (token,));
            reply.expect("ProvideXdgActivationToken");
        };
        let activate = |method: &str| bus.item(&tray, method, (0i32, 0i32)).expect(method);

        provide("stale");
        provide("plasma");
        activate("Activate");
        activate("SecondaryActivate");
        provide("middle");
        activate("SecondaryActivate");
        provide("settings");
        bus.menu(&tray, "Event", click(3)).unwrap();
        provide("scroll");
        bus.item(&tray, "Scroll", (1i32, "vertical")).unwrap();
        bus.menu(&tray, "Event", click(2)).unwrap();
        provide("");
        activate("ContextMenu");
        assert_eq!(
            recorder.events(),
            [
                "show plasma",
                "show",
                "show middle",
                "settings settings",
                "show",
                "show"
            ]
        );

        let reply = bus.item(&tray, "ProvideXdgActivationToken", (42i32,));
        let error = reply.expect_err("a token is a string");
        let name = gio::DBusError::remote_error(&error);
        assert_eq!(
            name.as_deref(),
            Some("org.freedesktop.DBus.Error.InvalidArgs"),
            "{error}"
        );
    });
}

#[test]
fn a_token_stays_with_the_activation_it_was_sent_for() {
    with_session_bus(|bus| {
        let (tray, recorder) = bus.tray();
        let token = |token: &str| {
            let args = (token,).to_variant();
            (ITEM_PATH, ITEM, "ProvideXdgActivationToken", args)
        };
        let calls = [
            token("first"),
            (ITEM_PATH, ITEM, "Activate", (0i32, 0i32).to_variant()),
            token("second"),
            (MENU_PATH, MENU, "Event", click(2).to_variant()),
        ];
        for reply in bus.pipeline(&tray, &calls) {
            reply.expect("a reply");
        }
        assert_eq!(recorder.events(), ["show first", "show second"]);
    });
}

#[test]
fn event_groups_pass_the_token_to_the_first_window_they_present() {
    with_session_bus(|bus| {
        let (tray, recorder) = bus.tray();
        tray.update("Connected · office_vpn", true, true);
        let provide = |token: &str| {
            let reply = bus.item(&tray, "ProvideXdgActivationToken", (token,));
            reply.expect("ProvideXdgActivationToken");
        };
        let group = |ids: &[i32]| {
            let events: Vec<_> = ids.iter().map(|&id| click(id)).collect();
            let reply = bus
                .menu(&tray, "EventGroup", (events,))
                .expect("EventGroup");
            assert_eq!(reply.type_().as_str(), "(ai)");
        };

        provide("group");
        group(&[4, 3, 2]);
        provide("quit");
        group(&[6]);
        group(&[2]);
        assert_eq!(
            recorder.events(),
            ["disconnect", "settings group", "show", "quit", "show"]
        );
        let label = bus.menu(&tray, "GetProperty", (1i32, "label")).unwrap();
        let label = label.child_value(0).as_variant().expect("a (v) reply");
        assert_eq!(label.str(), Some("Connected · office__vpn"));
    });
}

#[test]
fn a_token_from_another_host_is_dropped() {
    with_session_bus(|bus| {
        let (tray, recorder) = bus.tray();
        let destination = tray.bus_name().expect("the tray has a bus name");
        let other = Bus {
            context: bus.context.clone(),
            host: connect(),
        };
        let token = ("other",);
        let reply = other.call_to(
            &destination,
            ITEM_PATH,
            ITEM,
            "ProvideXdgActivationToken",
            token,
        );
        reply.expect("ProvideXdgActivationToken");
        bus.item(&tray, "Activate", (0i32, 0i32)).unwrap();
        other
            .call_to(&destination, ITEM_PATH, ITEM, "Activate", (0i32, 0i32))
            .unwrap();
        assert_eq!(recorder.events(), ["show", "show"]);
    });
}

#[test]
fn menu_errors_use_dbusmenu_names() {
    with_session_bus(|bus| {
        let (tray, _) = bus.tray();
        let replies = [
            bus.menu(&tray, "GetProperty", (42i32, "label")),
            bus.menu(&tray, "GetProperty", (2i32, "icon-name")),
            bus.menu(&tray, "GetLayout", (9i32, -1i32, all())),
            bus.menu(&tray, "GetGroupProperties", (vec![2i32, 9], all())),
        ];
        for reply in replies {
            let error = reply.expect_err("an unknown item or property");
            let name = gio::DBusError::remote_error(&error);
            assert_eq!(
                name.as_deref(),
                Some("com.canonical.dbusmenu.InvalidArgs"),
                "{error}"
            );
        }
        let reply = bus
            .call(&tray, MENU_PATH, PROPERTIES, "GetAll", (MENU,))
            .unwrap();
        let properties = reply.to_string();
        assert!(properties.contains("'Version': <uint32 3>"), "{properties}");
        assert!(
            properties.contains("'TextDirection': <'ltr'>"),
            "{properties}"
        );
    });
}

#[test]
fn availability_follows_the_watcher_and_its_host() {
    with_session_bus(|bus| {
        let (tray, recorder) = bus.tray();
        let log = &recorder;
        let reports = |count| move || log.availability().len() == count;
        bus.pump_until("a first report", reports(1));
        let watcher = FakeWatcher::start(false);
        assert_eq!(recorder.availability(), [false], "no watcher yet");

        bus.pump_until("registration", reports(2));
        assert_eq!(watcher.items_of(&tray), [ITEM_PATH]);
        assert!(
            !tray.is_available(),
            "a watcher without a host is not enough"
        );

        watcher.set_host(true);
        bus.pump_until("a host", reports(3));
        assert!(tray.is_available());
        assert_eq!(
            watcher.items_of(&tray),
            [ITEM_PATH, ITEM_PATH],
            "registers again"
        );

        watcher.set_host(false);
        bus.pump_until("the host to leave", reports(4));
        assert!(!tray.is_available());

        watcher.set_host(true);
        bus.pump_until("the host to return", reports(5));
        assert!(tray.is_available());

        drop(watcher);
        bus.pump_until("the watcher to vanish", reports(6));
        assert!(!tray.is_available());
        assert_eq!(
            recorder.availability(),
            [false, false, true, false, true, false]
        );
    });
}

#[test]
fn new_icon_is_emitted_when_the_connection_changes() {
    with_session_bus(|bus| {
        let (tray, _) = bus.tray();
        let (_subscription, signals) = bus.signals(&tray);
        assert_eq!(bus.icon(&tray), DISCONNECTED_ICON);

        tray.update("Connected", true, true);
        assert_eq!(bus.icon(&tray), CONNECTED_ICON);
        assert_eq!(
            signals.take(),
            ["NewIcon", "NewToolTip", "LayoutUpdated(2, 0)"]
        );
        let tooltip = bus.item_property(&tray, "ToolTip");
        assert_eq!(tooltip.child_value(0).str(), Some(CONNECTED_ICON));

        tray.update("Connected", true, true);
        bus.ping(&tray);
        assert_eq!(signals.take(), Vec::<String>::new(), "nothing changed");

        tray.update("Disconnecting", false, true);
        assert_eq!(bus.icon(&tray), CONNECTED_ICON);
        assert_eq!(signals.take(), ["NewToolTip", "LayoutUpdated(3, 0)"]);

        tray.update("Disconnected", false, false);
        assert_eq!(bus.icon(&tray), DISCONNECTED_ICON);
        assert_eq!(
            signals.take(),
            ["NewIcon", "NewToolTip", "LayoutUpdated(4, 0)"]
        );
        let layout = bus.menu(&tray, "GetLayout", (0i32, 0i32, all())).unwrap();
        assert_eq!(layout.child_value(0).get::<u32>(), Some(4));
    });
}

#[test]
fn close_is_idempotent_and_silences_the_tray() {
    with_session_bus(|bus| {
        let (tray, recorder) = bus.tray();
        bus.pump_until("a first report", || !recorder.availability().is_empty());
        let reports = recorder.availability();
        let (_subscription, signals) = bus.signals(&tray);

        tray.close();
        tray.close();
        assert!(!tray.is_available());
        assert!(!recorder.is_held(), "closing released the actions");
        let icon = bus.call(&tray, ITEM_PATH, PROPERTIES, "Get", (ITEM, "IconName"));
        assert!(icon.is_err(), "the item is gone");
        assert!(
            bus.menu(&tray, "Event", click(2)).is_err(),
            "the menu is gone"
        );
        tray.update("Connected", true, true);
        bus.ping(&tray);
        assert_eq!(signals.take(), Vec::<String>::new());

        let watcher = FakeWatcher::start(true);
        watcher.set_host(true);
        bus.pump_for(Duration::from_millis(300));
        assert_eq!(
            watcher.items_of(&tray),
            Vec::<String>::new(),
            "a closed tray does not register"
        );
        drop(watcher);
        bus.pump_for(Duration::from_millis(100));
        assert_eq!(recorder.availability(), reports);
        assert_eq!(recorder.events(), Vec::<String>::new());

        // The object paths are free again.
        let (again, _) = bus.tray();
        again.close();
    });
}

#[test]
fn an_action_may_close_the_tray() {
    with_session_bus(|bus| {
        let recorder = Recorder::default();
        let slot = Rc::new(OnceCell::<Tray>::new());
        let mut actions = recorder.actions();
        let (target, quit) = (Rc::downgrade(&slot), actions.quit);
        actions.quit = Rc::new(move || {
            if let Some(tray) = target.upgrade().as_deref().and_then(OnceCell::get) {
                tray.close();
            }
            quit();
        });
        let tray = slot.get_or_init(|| Tray::new(actions).expect("export the tray"));
        let name = tray.bus_name().expect("the tray has a bus name");

        bus.menu(tray, "Event", click(6)).unwrap();
        assert_eq!(recorder.events(), ["quit"]);
        assert!(!recorder.is_held(), "closing released the actions");
        let icon = bus.call_to(&name, ITEM_PATH, PROPERTIES, "Get", (ITEM, "IconName"));
        assert!(icon.is_err(), "the item is gone");
    });
}

#[test]
fn dropping_the_tray_unexports_it() {
    with_session_bus(|bus| {
        let (tray, recorder) = bus.tray();
        let name = tray.bus_name().unwrap();
        // Keep the shared connection alive, so only the tray goes away.
        let session = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).unwrap();
        drop(tray);
        let icon = bus.call_to(&name, ITEM_PATH, PROPERTIES, "Get", (ITEM, "IconName"));
        assert!(icon.is_err(), "the item is gone");
        let (again, _) = bus.tray();
        assert_eq!(
            again.bus_name().as_deref(),
            session.unique_name().as_deref()
        );
        assert_eq!(bus.icon(&again), DISCONNECTED_ICON);
        assert_eq!(recorder.events(), Vec::<String>::new());
    });
}
