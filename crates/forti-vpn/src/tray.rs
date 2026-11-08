//! System tray icon: a StatusNotifierItem with a DBusMenu, exported on the
//! session bus with GIO (works with KDE Plasma, the GNOME AppIndicator
//! extension, Cinnamon, XFCE, COSMIC, Budgie, Waybar…).
//!
//! A port of the Python `forti_vpn/indicator.py`, plus a connection-state
//! icon and XDG activation tokens. GDBus runs the tray's callbacks in the
//! main context that was the thread default when the tray was created, and
//! the tray runs [`TrayActions`] from idle callbacks of that same context.
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::glib::prelude::*;
use gtk::glib::variant::{DictEntry, ObjectPath};

/// Menu actions. All run on the GTK main thread.
#[derive(Clone)]
pub struct TrayActions {
    /// "Open Forti VPN", or activating the icon. Receives the XDG
    /// activation token the panel provided with `ProvideXdgActivationToken`,
    /// if any; pass it to `gtk::Window::set_startup_id` before `present()`
    /// so Wayland compositors raise the window.
    pub show: Rc<dyn Fn(Option<String>)>,
    /// "Settings", which also presents the window: receives the activation
    /// token as `show` does.
    pub settings: Rc<dyn Fn(Option<String>)>,
    /// Only invoked while the Disconnect item is enabled.
    pub disconnect: Rc<dyn Fn()>,
    /// "Disconnect and Quit".
    pub quit: Rc<dyn Fn()>,
    /// Called whenever a tray host appears (`true`) or disappears (`false`).
    /// Never reports `true` when a watcher exists without a panel host.
    pub availability: Rc<dyn Fn(bool)>,
}

/// Icon shown while connected (also the app's symbolic icon).
pub const CONNECTED_ICON: &str = "io.github.fortivpn.Utility-symbolic";
/// Icon shown otherwise (an open lock).
pub const DISCONNECTED_ICON: &str = "io.github.fortivpn.Utility-disconnected-symbolic";

const ITEM: &str = "org.kde.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/Menu";
const WATCHER_TIMEOUT_MS: i32 = 3000;
const INITIAL_STATUS: &str = "Checking VPN status";
const XML: &str = r#"<node>
<interface name="org.kde.StatusNotifierItem">
<method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method>
<method name="ProvideXdgActivationToken"><arg name="token" type="s" direction="in"/></method>
<property name="Category" type="s" access="read"/><property name="Id" type="s" access="read"/>
<property name="Title" type="s" access="read"/><property name="Status" type="s" access="read"/>
<property name="WindowId" type="u" access="read"/><property name="IconName" type="s" access="read"/>
<property name="IconThemePath" type="s" access="read"/><property name="IconPixmap" type="a(iiay)" access="read"/>
<property name="OverlayIconName" type="s" access="read"/><property name="OverlayIconPixmap" type="a(iiay)" access="read"/>
<property name="AttentionIconName" type="s" access="read"/><property name="AttentionIconPixmap" type="a(iiay)" access="read"/>
<property name="AttentionMovieName" type="s" access="read"/>
<property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
<property name="ItemIsMenu" type="b" access="read"/><property name="Menu" type="o" access="read"/>
<signal name="NewIcon"/><signal name="NewToolTip"/><signal name="NewTitle"/>
<signal name="NewStatus"><arg type="s"/></signal>
</interface>
<interface name="com.canonical.dbusmenu">
<method name="GetLayout"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="as" direction="in"/><arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/></method>
<method name="GetGroupProperties"><arg type="ai" direction="in"/><arg type="as" direction="in"/><arg type="a(ia{sv})" direction="out"/></method>
<method name="GetProperty"><arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
<method name="Event"><arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="in"/><arg type="u" direction="in"/></method>
<method name="EventGroup"><arg type="a(isvu)" direction="in"/><arg type="ai" direction="out"/></method>
<method name="AboutToShow"><arg type="i" direction="in"/><arg type="b" direction="out"/></method>
<method name="AboutToShowGroup"><arg type="ai" direction="in"/><arg type="ai" direction="out"/><arg type="ai" direction="out"/></method>
<property name="Version" type="u" access="read"/><property name="TextDirection" type="s" access="read"/>
<property name="Status" type="s" access="read"/><property name="IconThemePath" type="as" access="read"/>
<signal name="LayoutUpdated"><arg type="u"/><arg type="i"/></signal>
</interface></node>"#;

/// The tray item. Dropping it has the same effect as [`Tray::close`].
pub struct Tray {
    shared: Rc<Shared>,
    connection: gio::DBusConnection,
    exports: RefCell<Exports>,
}

/// What [`Tray::close`] has to undo.
#[derive(Default)]
struct Exports {
    objects: Vec<gio::RegistrationId>,
    host_signal: Option<gio::SignalSubscription>,
    /// gio does not export the type of a name watch, so this keeps the call
    /// that ends it.
    unwatch: Option<Box<dyn FnOnce()>>,
}

impl Tray {
    /// Export the item on the session bus and register with the
    /// StatusNotifierWatcher when one appears.
    pub fn new(actions: TrayActions) -> Result<Self, glib::Error> {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
        let node = gio::DBusNodeInfo::for_xml(XML)?;
        let tray = Self {
            shared: Rc::new(Shared::new(actions, icon_theme_path())?),
            connection,
            exports: RefCell::default(),
        };
        // On error, dropping `tray` unexports whatever was already exported.
        tray.export(&node)?;
        tray.watch_for_host();
        Ok(tray)
    }

    /// Update the status line, the Disconnect item, and the icon. Emits
    /// change signals only when something changed.
    pub fn update(&self, text: &str, disconnect_enabled: bool, connected: bool) {
        let Some(change) = self.shared.update(text, disconnect_enabled, connected) else {
            return;
        };
        if change.icon {
            self.emit(Interface::Item, "NewIcon", None);
        }
        self.emit(Interface::Item, "NewToolTip", None);
        let layout = (change.revision, 0i32).to_variant();
        self.emit(Interface::Menu, "LayoutUpdated", Some(&layout));
    }

    /// Whether a panel host currently shows the item.
    pub fn is_available(&self) -> bool {
        self.shared.available.get()
    }

    /// The bus name the item is exported on (for tests and diagnostics).
    pub fn bus_name(&self) -> Option<String> {
        self.connection.unique_name().map(String::from)
    }

    /// Unexport everything and release the [`TrayActions`]. Idempotent; no
    /// callbacks run afterwards.
    pub fn close(&self) {
        self.shared.close();
        // Empty once taken, which makes the rest idempotent.
        let exports = self.exports.take();
        if let Some(unwatch) = exports.unwatch {
            unwatch();
        }
        drop(exports.host_signal);
        for object in exports.objects {
            // This only fails for an object GDBus no longer has.
            let _ = self.connection.unregister_object(object);
        }
    }

    fn export(&self, node: &gio::DBusNodeInfo) -> Result<(), glib::Error> {
        for interface in [Interface::Item, Interface::Menu] {
            let info = node.lookup_interface(interface.name()).ok_or_else(|| {
                glib::Error::new(gio::DBusError::UnknownInterface, interface.name())
            })?;
            // These closures must always answer, so they hold `shared`
            // strongly; `close` releases them by unregistering the object.
            let methods = Rc::clone(&self.shared);
            let properties = Rc::clone(&self.shared);
            let object = self
                .connection
                .register_object(interface.path(), &info)
                .method_call(move |_, sender, _, _, method, parameters, invocation| {
                    let call = Call {
                        interface,
                        sender,
                        method,
                        parameters: &parameters,
                    };
                    methods.answer(&call, invocation);
                })
                .property(move |_, _, _, _, name| properties.property(interface, name))
                .build()?;
            self.exports.borrow_mut().objects.push(object);
        }
        Ok(())
    }

    fn watch_for_host(&self) {
        let shared = Rc::downgrade(&self.shared);
        let host_signal = self.connection.subscribe_to_signal(
            Some(WATCHER),
            Some(WATCHER),
            None,
            Some(WATCHER_PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let Some(shared) = shared.upgrade() else {
                    return;
                };
                match signal.signal_name {
                    "StatusNotifierHostRegistered" => shared.register(signal.connection),
                    "StatusNotifierHostUnregistered" => shared.set_available(false),
                    _ => {}
                }
            },
        );
        let appeared = Rc::downgrade(&self.shared);
        let vanished = Rc::downgrade(&self.shared);
        let watch = gio::bus_watch_name_on_connection(
            &self.connection,
            WATCHER,
            gio::BusNameWatcherFlags::NONE,
            move |connection, _, _| {
                if let Some(shared) = appeared.upgrade() {
                    shared.register(&connection);
                }
            },
            move |_, _| {
                if let Some(shared) = vanished.upgrade() {
                    shared.cancel_registration();
                    shared.set_available(false);
                }
            },
        );
        let mut exports = self.exports.borrow_mut();
        exports.host_signal = Some(host_signal);
        exports.unwatch = Some(Box::new(move || gio::bus_unwatch_name(watch)));
    }

    fn emit(&self, interface: Interface, signal: &str, parameters: Option<&glib::Variant>) {
        // This only fails once the connection is closed, when no host listens.
        let _ = self.connection.emit_signal(
            None,
            interface.path(),
            interface.name(),
            signal,
            parameters,
        );
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.close();
    }
}

/// The directory a panel host should search for this app's icons
/// (`IconThemePath`): the installed `share/icons` next to the executable's
/// prefix when present, else, in debug builds, the source tree's
/// `data/icons`, else empty (icons are then found in the system theme).
pub fn icon_theme_path() -> String {
    let icons = std::env::current_exe()
        .ok()
        .and_then(|exe| installed_icons(&exe));
    // Release binaries must neither embed nor advertise the build directory.
    #[cfg(debug_assertions)]
    let icons = icons.or_else(source_icons);
    icons
        // D-Bus strings are UTF-8; another path could not be sent intact.
        .and_then(|icons| icons.into_os_string().into_string().ok())
        .unwrap_or_default()
}

/// `<prefix>/share/icons` for an executable installed as `<prefix>/bin/<name>`.
fn installed_icons(exe: &Path) -> Option<PathBuf> {
    let icons = exe.parent()?.parent()?.join("share/icons");
    has_app_icon(&icons).then_some(icons)
}

/// `data/icons` in the source tree this debug binary was built from.
#[cfg(debug_assertions)]
fn source_icons() -> Option<PathBuf> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?;
    let icons = workspace.join("data/icons");
    has_app_icon(&icons).then_some(icons)
}

fn has_app_icon(icons: &Path) -> bool {
    icons
        .join("hicolor/symbolic/apps")
        .join(format!("{CONNECTED_ICON}.svg"))
        .is_file()
}

/// The two exported objects, one interface each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Interface {
    Item,
    Menu,
}

impl Interface {
    fn name(self) -> &'static str {
        match self {
            Self::Item => ITEM,
            Self::Menu => MENU,
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::Item => ITEM_PATH,
            Self::Menu => MENU_PATH,
        }
    }
}

/// A method call from a panel host.
#[derive(Debug, Clone, Copy)]
struct Call<'a> {
    interface: Interface,
    /// The caller's unique bus name; `None` on a peer-to-peer connection.
    sender: Option<&'a str>,
    method: &'a str,
    parameters: &'a glib::Variant,
}

/// A call into [`TrayActions`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Callback {
    /// Open the window, with the host's XDG activation token, which
    /// [`Shared::handle`] adds.
    Show(Option<String>),
    /// Open Settings, with the activation token like `Show`.
    Settings(Option<String>),
    Disconnect,
    Quit,
    Availability(bool),
}

impl Callback {
    /// Where a callback that presents the window keeps its activation token.
    fn token_mut(&mut self) -> Option<&mut Option<String>> {
        match self {
            Self::Show(token) | Self::Settings(token) => Some(token),
            Self::Disconnect | Self::Quit | Self::Availability(_) => None,
        }
    }
}

/// What a host's call asks of the tray besides its reply.
#[derive(Debug, Default, PartialEq, Eq)]
struct Requests {
    callbacks: Vec<Callback>,
    /// The call reports a user interaction: the icon was activated or
    /// scrolled, or a menu item clicked. Each one ends the life of the
    /// activation token provided before it.
    interaction: bool,
}

/// An XDG activation token and the host that provided it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ActivationToken {
    sender: Option<String>,
    token: String,
}

/// The DBusMenu items. Hosts address items by these ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuItem {
    Root = 0,
    Status = 1,
    Open = 2,
    Settings = 3,
    Disconnect = 4,
    Separator = 5,
    Quit = 6,
}

impl MenuItem {
    /// The root's children, top to bottom.
    const CHILDREN: [Self; 6] = [
        Self::Status,
        Self::Open,
        Self::Settings,
        Self::Disconnect,
        Self::Separator,
        Self::Quit,
    ];

    fn from_id(id: i32) -> Option<Self> {
        std::iter::once(Self::Root)
            .chain(Self::CHILDREN)
            .find(|item| item.id() == id)
    }

    fn id(self) -> i32 {
        self as i32
    }

    fn callback(self) -> Option<Callback> {
        match self {
            Self::Open => Some(Callback::Show(None)),
            Self::Settings => Some(Callback::Settings(None)),
            Self::Disconnect => Some(Callback::Disconnect),
            Self::Quit => Some(Callback::Quit),
            Self::Root | Self::Status | Self::Separator => None,
        }
    }
}

/// Hosts call these when the icon itself is clicked; all open the window.
fn activation(method: &str) -> Option<Callback> {
    matches!(method, "Activate" | "SecondaryActivate" | "ContextMenu")
        .then_some(Callback::Show(None))
}

/// A DBusMenu error, with the D-Bus error name hosts receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuError {
    UnknownMethod,
    InvalidArgs,
}

impl MenuError {
    fn name(self) -> &'static str {
        match self {
            Self::UnknownMethod => "com.canonical.dbusmenu.UnknownMethod",
            Self::InvalidArgs => "com.canonical.dbusmenu.InvalidArgs",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::UnknownMethod => "Unsupported menu method",
            Self::InvalidArgs => "Unknown menu item or property",
        }
    }
}

/// `a{sv}`
type Properties = Vec<DictEntry<&'static str, glib::Variant>>;
/// `(ia{sv}av)`: id, properties, children.
type Layout = (i32, Properties, Vec<glib::Variant>);
/// `a(iiay)`: width, height, ARGB32 data.
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;
/// `(isvu)`: item id, event, data, timestamp.
type MenuEvent = (i32, String, glib::Variant, u32);

/// What the item shows; everything hosts read derives from it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    text: String,
    disconnect_enabled: bool,
    connected: bool,
    /// The DBusMenu layout revision, bumped by every change.
    revision: u32,
}

/// What [`State::update`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Change {
    revision: u32,
    icon: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            text: INITIAL_STATUS.to_owned(),
            disconnect_enabled: false,
            connected: false,
            revision: 1,
        }
    }
}

impl State {
    fn icon(&self) -> &'static str {
        if self.connected {
            CONNECTED_ICON
        } else {
            DISCONNECTED_ICON
        }
    }

    /// Apply new values; `None` when nothing changed.
    fn update(&mut self, text: &str, disconnect_enabled: bool, connected: bool) -> Option<Change> {
        if self.text == text
            && self.disconnect_enabled == disconnect_enabled
            && self.connected == connected
        {
            return None;
        }
        let icon = self.connected != connected;
        text.clone_into(&mut self.text);
        self.disconnect_enabled = disconnect_enabled;
        self.connected = connected;
        self.revision = self.revision.wrapping_add(1);
        Some(Change {
            revision: self.revision,
            icon,
        })
    }

    fn is_enabled(&self, item: MenuItem) -> bool {
        match item {
            MenuItem::Status => false,
            MenuItem::Disconnect => self.disconnect_enabled,
            _ => true,
        }
    }

    /// Answer a `com.canonical.dbusmenu` method call, recording its clicks
    /// in `requests`.
    fn menu_call(
        &self,
        method: &str,
        parameters: &glib::Variant,
        requests: &mut Requests,
    ) -> Result<Option<glib::Variant>, MenuError> {
        let reply = match method {
            "GetLayout" => {
                let (parent, depth, names) = arguments::<(i32, i32, Vec<String>)>(parameters)?;
                let layout = self.layout(menu_item(parent)?, depth, &names);
                (self.revision, layout).to_variant()
            }
            "GetGroupProperties" => {
                let (ids, names) = arguments::<(Vec<i32>, Vec<String>)>(parameters)?;
                let groups = ids
                    .into_iter()
                    .map(|id| Ok((id, self.properties(menu_item(id)?, &names))))
                    .collect::<Result<Vec<_>, MenuError>>()?;
                (groups,).to_variant()
            }
            "GetProperty" => {
                let (id, name) = arguments::<(i32, String)>(parameters)?;
                let value = self
                    .menu_properties(menu_item(id)?)
                    .into_iter()
                    .find_map(|(key, value)| (key == name).then_some(value))
                    .ok_or(MenuError::InvalidArgs)?;
                (value,).to_variant()
            }
            "Event" => {
                let (id, event, ..) = arguments::<MenuEvent>(parameters)?;
                self.event(id, &event, requests);
                return Ok(None);
            }
            "EventGroup" => {
                let (events,) = arguments::<(Vec<MenuEvent>,)>(parameters)?;
                for (id, event, ..) in &events {
                    self.event(*id, event, requests);
                }
                (Vec::<i32>::new(),).to_variant()
            }
            "AboutToShow" => (false,).to_variant(),
            "AboutToShowGroup" => (Vec::<i32>::new(), Vec::<i32>::new()).to_variant(),
            _ => return Err(MenuError::UnknownMethod),
        };
        Ok(Some(reply))
    }

    /// Record a menu event. Only clicks matter: each one is an interaction,
    /// and runs the item's callback when the item is enabled and has one.
    fn event(&self, id: i32, event: &str, requests: &mut Requests) {
        if event != "clicked" {
            return;
        }
        requests.interaction = true;
        requests.callbacks.extend(self.clicked(id));
    }

    /// The callback for a click on item `id`, if the item is enabled and
    /// does something.
    fn clicked(&self, id: i32) -> Option<Callback> {
        let item = MenuItem::from_id(id)?;
        self.is_enabled(item).then(|| item.callback()).flatten()
    }

    /// An item as `(ia{sv}av)`. Only the root has children, listed unless
    /// `depth` is 0.
    fn layout(&self, item: MenuItem, depth: i32, names: &[String]) -> Layout {
        let children = if item == MenuItem::Root && depth != 0 {
            MenuItem::CHILDREN
                .iter()
                .map(|&child| self.layout(child, 0, names).to_variant())
                .collect()
        } else {
            Vec::new()
        };
        (item.id(), self.properties(item, names), children)
    }

    /// An item's properties, keeping only `names` unless it is empty.
    fn properties(&self, item: MenuItem, names: &[String]) -> Properties {
        self.menu_properties(item)
            .into_iter()
            .filter(|(key, _)| names.is_empty() || names.iter().any(|name| name == key))
            .map(|(key, value)| DictEntry::new(key, value))
            .collect()
    }

    fn menu_properties(&self, item: MenuItem) -> Vec<(&'static str, glib::Variant)> {
        let label = match item {
            MenuItem::Root => return vec![("children-display", "submenu".to_variant())],
            MenuItem::Separator => return vec![("type", "separator".to_variant())],
            // DBusMenu reads "_" as a mnemonic marker and "__" as a literal
            // underscore; the status names profiles, which may contain one.
            MenuItem::Status => self.text.replace('_', "__").to_variant(),
            MenuItem::Open => "Open Forti VPN".to_variant(),
            MenuItem::Settings => "Settings".to_variant(),
            MenuItem::Disconnect => "Disconnect".to_variant(),
            MenuItem::Quit => "Disconnect and Quit".to_variant(),
        };
        vec![
            ("label", label),
            ("visible", true.to_variant()),
            ("enabled", self.is_enabled(item).to_variant()),
        ]
    }
}

fn arguments<T: FromVariant>(parameters: &glib::Variant) -> Result<T, MenuError> {
    parameters.get().ok_or(MenuError::InvalidArgs)
}

fn menu_item(id: i32) -> Result<MenuItem, MenuError> {
    MenuItem::from_id(id).ok_or(MenuError::InvalidArgs)
}

/// State shared by the [`Tray`] and its D-Bus callbacks. [`Tray::close`]
/// releases every callback that holds it.
struct Shared {
    /// `None` once closed. The actions may hold the app, which holds the
    /// tray, so closing releases them at once: GDBus drops the callbacks
    /// that hold `Shared` only on a later main loop iteration.
    actions: RefCell<Option<TrayActions>>,
    /// Where GDBus runs this tray's callbacks: the thread default at creation.
    context: glib::MainContext,
    icon_theme_path: String,
    /// `MENU_PATH` as an object path (`o`).
    menu_path: glib::Variant,
    state: RefCell<State>,
    /// The token of the last `ProvideXdgActivationToken`, which hosts call
    /// right before an activation or menu click, until the next interaction
    /// takes or drops it.
    activation_token: Cell<Option<ActivationToken>>,
    available: Cell<bool>,
    closed: Cell<bool>,
    /// The latest registration with the watcher, possibly still running.
    registration: RefCell<Option<glib::JoinHandle<()>>>,
}

impl Shared {
    fn new(actions: TrayActions, icon_theme_path: String) -> Result<Self, glib::Error> {
        let menu_path = ObjectPath::try_from(MENU_PATH)
            .map_err(|error| glib::Error::new(gio::IOErrorEnum::InvalidArgument, &error.message))?;
        Ok(Self {
            actions: RefCell::new(Some(actions)),
            context: glib::MainContext::ref_thread_default(),
            icon_theme_path,
            menu_path: menu_path.to_variant(),
            state: RefCell::default(),
            activation_token: Cell::new(None),
            available: Cell::new(false),
            closed: Cell::new(false),
            registration: RefCell::new(None),
        })
    }

    /// Stop running callbacks and release the actions. Idempotent.
    fn close(&self) {
        self.closed.set(true);
        self.available.set(false);
        self.cancel_registration();
        self.activation_token.set(None);
        drop(self.actions.take());
    }

    /// Apply an update from the app; `None` when closed or unchanged.
    fn update(&self, text: &str, disconnect_enabled: bool, connected: bool) -> Option<Change> {
        if self.closed.get() {
            return None;
        }
        let mut state = self.state.borrow_mut();
        state.update(text, disconnect_enabled, connected)
    }

    fn answer(self: &Rc<Self>, call: &Call<'_>, invocation: gio::DBusMethodInvocation) {
        let mut requests = Requests::default();
        let reply = self.handle(call, &mut requests);
        for callback in requests.callbacks {
            self.defer(callback);
        }
        match reply {
            Ok(body) => invocation.return_value(body.as_ref()),
            Err(error) => invocation.return_dbus_error(error.name(), error.message()),
        }
    }

    /// Answer a method call, recording in `requests` the callbacks it asks
    /// for, with the activation token for the window they present.
    fn handle(
        &self,
        call: &Call<'_>,
        requests: &mut Requests,
    ) -> Result<Option<glib::Variant>, MenuError> {
        let reply = match call.interface {
            Interface::Item => {
                self.item_call(call, requests);
                Ok(None)
            }
            Interface::Menu => {
                let state = self.state.borrow();
                state.menu_call(call.method, call.parameters, requests)
            }
        };
        if requests.interaction {
            self.hand_over_token(call.sender, &mut requests.callbacks);
        }
        reply
    }

    /// An interaction uses up the last activation token: the first window it
    /// presents gets the token if the same host sent both, and otherwise the
    /// token is dropped, so it never raises the window for another click.
    /// Taken now rather than when the callback runs: hosts send a token and
    /// its activation without waiting, so the token for a second click may
    /// arrive before the first click's callback runs.
    fn hand_over_token(&self, sender: Option<&str>, callbacks: &mut [Callback]) {
        let token = self
            .activation_token
            .take()
            .filter(|token| token.sender.as_deref() == sender);
        if let Some(slot) = callbacks.iter_mut().find_map(Callback::token_mut) {
            *slot = token.map(|token| token.token);
        }
    }

    /// Handle an `org.kde.StatusNotifierItem` method call; none has a reply.
    fn item_call(&self, call: &Call<'_>, requests: &mut Requests) {
        if call.method == "ProvideXdgActivationToken" {
            // GDBus has checked the argument types against the XML, and a
            // host that could not get a token may send an empty one.
            let token = call
                .parameters
                .get::<(String,)>()
                .map(|(token,)| token)
                .filter(|token| !token.is_empty())
                .map(|token| ActivationToken {
                    sender: call.sender.map(str::to_owned),
                    token,
                });
            self.activation_token.set(token);
        } else {
            // Activate, SecondaryActivate, ContextMenu or Scroll.
            requests.interaction = true;
            requests.callbacks.extend(activation(call.method));
        }
    }

    fn property(&self, interface: Interface, name: &str) -> glib::Variant {
        match interface {
            Interface::Item => self.item_property(name),
            Interface::Menu => self.menu_property(name),
        }
    }

    fn item_property(&self, name: &str) -> glib::Variant {
        let state = self.state.borrow();
        match name {
            "Category" => "Communications".to_variant(),
            "Id" => "forti-vpn".to_variant(),
            "Title" => "Forti VPN".to_variant(),
            "Status" => "Active".to_variant(),
            "WindowId" => 0u32.to_variant(),
            "IconName" => state.icon().to_variant(),
            "IconThemePath" => self.icon_theme_path.to_variant(),
            "ToolTip" => {
                let tooltip = (
                    state.icon(),
                    Pixmaps::new(),
                    "Forti VPN",
                    state.text.as_str(),
                );
                tooltip.to_variant()
            }
            "ItemIsMenu" => false.to_variant(),
            "Menu" => self.menu_path.clone(),
            "IconPixmap" | "OverlayIconPixmap" | "AttentionIconPixmap" => {
                Pixmaps::new().to_variant()
            }
            // OverlayIconName, AttentionIconName and AttentionMovieName.
            _ => "".to_variant(),
        }
    }

    fn menu_property(&self, name: &str) -> glib::Variant {
        match name {
            "Version" => 3u32.to_variant(),
            "TextDirection" => "ltr".to_variant(),
            "Status" => "normal".to_variant(),
            "IconThemePath" => {
                let path = Some(self.icon_theme_path.as_str()).filter(|path| !path.is_empty());
                path.into_iter().collect::<Vec<_>>().to_variant()
            }
            // GDBus only asks for properties declared in the XML.
            _ => "".to_variant(),
        }
    }

    /// Run `callback` from an idle callback instead of inside the D-Bus
    /// callback that caused it: the D-Bus reply goes out first, and the
    /// callback may call back into the tray, or close it.
    fn defer(self: &Rc<Self>, callback: Callback) {
        let shared = Rc::downgrade(self);
        // Detached on purpose: `run` does nothing once the tray is closed.
        self.context
            .spawn_local_with_priority(glib::Priority::DEFAULT_IDLE, async move {
                if let Some(shared) = shared.upgrade() {
                    shared.run(callback);
                }
            });
    }

    fn run(&self, callback: Callback) {
        // Cloned, so no borrow is held while an action runs: it may close
        // the tray, which takes the actions. `None` once closed.
        let Some(actions) = self.actions.borrow().clone() else {
            return;
        };
        match callback {
            Callback::Show(token) => (actions.show)(token),
            Callback::Settings(token) => (actions.settings)(token),
            // The item may have been disabled since the click.
            Callback::Disconnect if self.state.borrow().disconnect_enabled => {
                (actions.disconnect)()
            }
            Callback::Disconnect => {}
            Callback::Quit => (actions.quit)(),
            Callback::Availability(available) => (actions.availability)(available),
        }
    }

    /// Register the item with the watcher on `connection`, then report
    /// whether a panel host shows it. Supersedes an unfinished registration.
    fn register(self: &Rc<Self>, connection: &gio::DBusConnection) {
        if self.closed.get() {
            return;
        }
        let shared = Rc::downgrade(self);
        let connection = connection.clone();
        let task = self.context.spawn_local(async move {
            let shown = register_item(&connection).await.unwrap_or(false);
            if let Some(shared) = shared.upgrade() {
                shared.set_available(shown);
            }
        });
        if let Some(previous) = self.registration.replace(Some(task)) {
            previous.abort();
        }
    }

    fn cancel_registration(&self) {
        if let Some(task) = self.registration.take() {
            task.abort();
        }
    }

    fn set_available(self: &Rc<Self>, available: bool) {
        if self.closed.get() {
            return;
        }
        self.available.set(available);
        self.defer(Callback::Availability(available));
    }
}

/// Register the item with the watcher, then ask whether a panel host is
/// registered too.
async fn register_item(connection: &gio::DBusConnection) -> Result<bool, glib::Error> {
    connection
        .call_future(
            Some(WATCHER),
            WATCHER_PATH,
            WATCHER,
            "RegisterStatusNotifierItem",
            Some(&(ITEM_PATH,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            WATCHER_TIMEOUT_MS,
        )
        .await?;
    // A watcher can exist without a panel host. Never hide into an absent host.
    let reply = connection
        .call_future(
            Some(WATCHER),
            WATCHER_PATH,
            PROPERTIES,
            "Get",
            Some(&(WATCHER, "IsStatusNotifierHostRegistered").to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            WATCHER_TIMEOUT_MS,
        )
        .await?;
    let host = reply
        .get::<(glib::Variant,)>()
        .and_then(|(value,)| value.get::<bool>());
    Ok(host == Some(true))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    type Log = Rc<RefCell<Vec<String>>>;

    /// The panel host the tests call from, as a unique bus name.
    const HOST: Option<&str> = Some(":1.42");

    /// Actions that log what ran, opening the window as `show` or
    /// `show <token>`, and Settings as `settings` or `settings <token>`.
    fn recorder() -> (TrayActions, Log) {
        let log = Log::default();
        let event = |name: &'static str| -> Rc<dyn Fn()> {
            let log = Rc::clone(&log);
            Rc::new(move || log.borrow_mut().push(name.to_owned()))
        };
        let presenter = |name: &'static str| -> Rc<dyn Fn(Option<String>)> {
            let log = Rc::clone(&log);
            Rc::new(move |token: Option<String>| {
                let entry =
                    token.map_or_else(|| name.to_owned(), |token| format!("{name} {token}"));
                log.borrow_mut().push(entry);
            })
        };
        let availability = Rc::clone(&log);
        let actions = TrayActions {
            show: presenter("show"),
            settings: presenter("settings"),
            disconnect: event("disconnect"),
            quit: event("quit"),
            availability: Rc::new(move |up| {
                availability.borrow_mut().push(format!("available {up}"))
            }),
        };
        (actions, log)
    }

    fn shared_with(icon_theme_path: &str) -> Rc<Shared> {
        Rc::new(Shared::new(recorder().0, icon_theme_path.to_owned()).unwrap())
    }

    fn connected() -> State {
        let mut state = State::default();
        state.update("Connected", true, true);
        state
    }

    /// A menu call that must not click anything; `()` for an empty reply.
    fn call(state: &State, method: &str, args: impl ToVariant) -> Result<glib::Variant, MenuError> {
        let mut requests = Requests::default();
        let reply = state.menu_call(method, &args.to_variant(), &mut requests)?;
        assert_eq!(requests, Requests::default(), "{method} clicked");
        Ok(reply.unwrap_or_else(|| ().to_variant()))
    }

    fn click(id: i32, event: &str) -> MenuEvent {
        (id, event.to_owned(), "unused".to_variant(), 0)
    }

    /// The callbacks a successful call from `sender` asks for, as GDBus
    /// hands it over.
    fn request_from(
        shared: &Shared,
        sender: Option<&str>,
        interface: Interface,
        method: &str,
        args: impl ToVariant,
    ) -> Vec<Callback> {
        let parameters = args.to_variant();
        let call = Call {
            interface,
            sender,
            method,
            parameters: &parameters,
        };
        let mut requests = Requests::default();
        let reply = shared.handle(&call, &mut requests);
        assert!(reply.is_ok(), "{method}: {reply:?}");
        requests.callbacks
    }

    /// The callbacks a successful call from [`HOST`] asks for.
    fn request(
        shared: &Shared,
        interface: Interface,
        method: &str,
        args: impl ToVariant,
    ) -> Vec<Callback> {
        request_from(shared, HOST, interface, method, args)
    }

    fn provide_token_from(shared: &Shared, sender: Option<&str>, token: &str) {
        let method = "ProvideXdgActivationToken";
        let callbacks = request_from(shared, sender, Interface::Item, method, (token,));
        assert!(callbacks.is_empty(), "{token}: {callbacks:?}");
    }

    fn provide_token(shared: &Shared, token: &str) {
        provide_token_from(shared, HOST, token);
    }

    fn show(token: Option<&str>) -> Vec<Callback> {
        vec![Callback::Show(token.map(str::to_owned))]
    }

    fn settings(token: Option<&str>) -> Callback {
        Callback::Settings(token.map(str::to_owned))
    }

    /// What an `Event` call asks for.
    fn event(state: &State, event: MenuEvent) -> Requests {
        let mut requests = Requests::default();
        let reply = state.menu_call("Event", &event.to_variant(), &mut requests);
        assert_eq!(reply, Ok(None));
        requests
    }

    /// The callbacks an `Event` call asks for.
    fn clicks(state: &State, click: MenuEvent) -> Vec<Callback> {
        event(state, click).callbacks
    }

    fn all() -> Vec<String> {
        Vec::new()
    }

    /// The revision and root of a `GetLayout` reply.
    fn layout_reply(reply: &glib::Variant) -> (u32, glib::Variant) {
        assert_eq!(reply.type_().as_str(), "(u(ia{sv}av))");
        (reply.child_value(0).get().unwrap(), reply.child_value(1))
    }

    /// The id, properties and children of a `(ia{sv}av)` layout.
    fn parts(layout: &glib::Variant) -> (i32, glib::VariantDict, Vec<glib::Variant>) {
        assert_eq!(layout.type_().as_str(), "(ia{sv}av)");
        let id = layout.child_value(0).get().unwrap();
        let properties = glib::VariantDict::new(Some(&layout.child_value(1)));
        // `Vec<Variant>` unboxes each `v` of the `av`.
        let children = layout.child_value(2).get().unwrap();
        (id, properties, children)
    }

    fn lookup<T: FromVariant + StaticVariantType>(
        properties: &glib::VariantDict,
        key: &str,
    ) -> Option<T> {
        properties.lookup(key).unwrap()
    }

    /// `(interface, property, signature)` for every property in `XML`.
    fn declared_properties() -> Vec<(&'static str, &'static str, &'static str)> {
        let attribute = |element: &'static str, name: &str| {
            let start = element.find(&format!("{name}=\"")).unwrap() + name.len() + 2;
            let end = start + element[start..].find('"').unwrap();
            &element[start..end]
        };
        let mut found = Vec::new();
        for interface in XML.split("<interface ").skip(1) {
            let name = attribute(interface, "name");
            for property in interface.split("<property ").skip(1) {
                found.push((
                    name,
                    attribute(property, "name"),
                    attribute(property, "type"),
                ));
            }
        }
        found
    }

    #[test]
    fn xml_declares_every_method_and_signal() {
        let node = gio::DBusNodeInfo::for_xml(XML).unwrap();
        let item = node.lookup_interface(ITEM).unwrap();
        let menu = node.lookup_interface(MENU).unwrap();
        let item_methods = [
            "Activate",
            "SecondaryActivate",
            "ContextMenu",
            "Scroll",
            "ProvideXdgActivationToken",
        ];
        for method in item_methods {
            assert!(item.lookup_method(method).is_some(), "{method}");
        }
        let menu_methods = ["GetLayout", "GetGroupProperties", "GetProperty", "Event"];
        for method in
            menu_methods
                .into_iter()
                .chain(["EventGroup", "AboutToShow", "AboutToShowGroup"])
        {
            assert!(menu.lookup_method(method).is_some(), "{method}");
        }
        assert!(item.lookup_signal("NewIcon").is_some());
        assert!(item.lookup_signal("NewToolTip").is_some());
        assert!(menu.lookup_signal("LayoutUpdated").is_some());
    }

    #[test]
    fn every_property_has_its_declared_type() {
        let declared = declared_properties();
        let count = |name| {
            declared
                .iter()
                .filter(|(interface, ..)| *interface == name)
                .count()
        };
        assert_eq!((count(ITEM), count(MENU)), (16, 4));
        for icon_theme_path in ["", "/usr/share/icons"] {
            let shared = shared_with(icon_theme_path);
            for connected in [false, true] {
                shared.update("Status", false, connected);
                for &(name, property, signature) in &declared {
                    let interface = if name == ITEM {
                        Interface::Item
                    } else {
                        Interface::Menu
                    };
                    let value = shared.property(interface, property);
                    assert_eq!(value.type_().as_str(), signature, "{name}.{property}");
                }
            }
        }
    }

    #[test]
    fn item_properties_describe_the_app_and_its_state() {
        let shared = shared_with("/opt/forti-vpn/share/icons");
        let property = |name| shared.property(Interface::Item, name);
        let text = |name| property(name).get::<String>().unwrap();
        assert_eq!(text("Category"), "Communications");
        assert_eq!(text("Id"), "forti-vpn");
        assert_eq!(text("Title"), "Forti VPN");
        assert_eq!(text("Status"), "Active");
        assert_eq!(text("IconName"), DISCONNECTED_ICON);
        assert_eq!(text("IconThemePath"), "/opt/forti-vpn/share/icons");
        for unused in ["OverlayIconName", "AttentionIconName", "AttentionMovieName"] {
            assert_eq!(text(unused), "", "{unused}");
        }
        for pixmap in ["IconPixmap", "OverlayIconPixmap", "AttentionIconPixmap"] {
            assert_eq!(property(pixmap).n_children(), 0, "{pixmap}");
        }
        assert_eq!(property("WindowId").get::<u32>(), Some(0));
        assert_eq!(property("ItemIsMenu").get::<bool>(), Some(false));
        let menu = property("Menu").get::<ObjectPath>().unwrap();
        assert_eq!(menu.as_str(), MENU_PATH);

        shared.update("Connected", true, true);
        assert_eq!(text("IconName"), CONNECTED_ICON);
        let tooltip = property("ToolTip")
            .get::<(String, Pixmaps, String, String)>()
            .unwrap();
        let (icon, pixmaps, title, body) = tooltip;
        assert_eq!(
            (icon.as_str(), title.as_str(), body.as_str()),
            (CONNECTED_ICON, "Forti VPN", "Connected")
        );
        assert!(pixmaps.is_empty());
    }

    #[test]
    fn menu_properties_describe_the_menu() {
        let shared = shared_with("/usr/share/icons");
        let property = |name| shared.property(Interface::Menu, name);
        assert_eq!(property("Version").get::<u32>(), Some(3));
        assert_eq!(property("TextDirection").str(), Some("ltr"));
        assert_eq!(property("Status").str(), Some("normal"));
        let paths = property("IconThemePath").get::<Vec<String>>().unwrap();
        assert_eq!(paths, ["/usr/share/icons"]);
        let paths = shared_with("").property(Interface::Menu, "IconThemePath");
        assert_eq!(paths.n_children(), 0, "no empty directory entry");
    }

    #[test]
    fn layout_lists_every_item_under_the_root() {
        let reply = call(&connected(), "GetLayout", (0i32, -1i32, all())).unwrap();
        let (revision, root) = layout_reply(&reply);
        assert_eq!(revision, 2);
        let (id, properties, children) = parts(&root);
        assert_eq!(id, 0);
        assert_eq!(
            lookup::<String>(&properties, "children-display").as_deref(),
            Some("submenu")
        );

        let items: Vec<_> = children.iter().map(parts).collect();
        let ids: Vec<i32> = items.iter().map(|(id, ..)| *id).collect();
        assert_eq!(ids, [1, 2, 3, 4, 5, 6]);
        let labels: Vec<Option<String>> = items
            .iter()
            .map(|(_, item, _)| lookup(item, "label"))
            .collect();
        let labels: Vec<Option<&str>> = labels.iter().map(Option::as_deref).collect();
        let expected = [
            Some("Connected"),
            Some("Open Forti VPN"),
            Some("Settings"),
            Some("Disconnect"),
        ];
        assert_eq!(labels[..4], expected);
        assert_eq!(labels[4..], [None, Some("Disconnect and Quit")]);
        let enabled: Vec<Option<bool>> = items
            .iter()
            .map(|(_, item, _)| lookup(item, "enabled"))
            .collect();
        assert_eq!(
            enabled,
            [
                Some(false),
                Some(true),
                Some(true),
                Some(true),
                None,
                Some(true)
            ]
        );
        assert_eq!(
            lookup::<String>(&items[4].1, "type").as_deref(),
            Some("separator")
        );
        assert!(items.iter().all(|(.., children)| children.is_empty()));
    }

    #[test]
    fn only_the_root_with_depth_has_children() {
        let state = connected();
        for (parent, depth) in [(0i32, 0i32), (2, -1), (6, 1)] {
            let reply = call(&state, "GetLayout", (parent, depth, all())).unwrap();
            let (id, _, children) = parts(&layout_reply(&reply).1);
            assert_eq!(id, parent);
            assert!(children.is_empty(), "parent {parent}, depth {depth}");
        }
    }

    #[test]
    fn layout_starts_with_the_initial_status() {
        let state = State::default();
        let reply = call(&state, "GetLayout", (1i32, 0i32, all())).unwrap();
        let (revision, status) = layout_reply(&reply);
        assert_eq!(revision, 1);
        assert_eq!(
            lookup::<String>(&parts(&status).1, "label").as_deref(),
            Some(INITIAL_STATUS)
        );
        let reply = call(&state, "GetLayout", (4i32, 0i32, all())).unwrap();
        let disconnect = parts(&layout_reply(&reply).1).1;
        assert_eq!(lookup::<bool>(&disconnect, "enabled"), Some(false));
    }

    #[test]
    fn group_properties_can_be_filtered() {
        let names = vec!["label".to_owned()];
        let reply = call(&connected(), "GetGroupProperties", (vec![2i32, 3], names)).unwrap();
        assert_eq!(reply.type_().as_str(), "(a(ia{sv}))");
        let text = reply.to_string();
        assert!(text.contains("Open Forti VPN"), "{text}");
        assert!(!text.contains("enabled"), "{text}");
        let (groups,) = reply.get::<(Vec<(i32, glib::VariantDict)>,)>().unwrap();
        assert_eq!(groups.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [2, 3]);
        assert_eq!(
            lookup::<String>(&groups[1].1, "label").as_deref(),
            Some("Settings")
        );

        let reply = call(&connected(), "GetGroupProperties", (vec![4i32], all())).unwrap();
        let (groups,) = reply.get::<(Vec<(i32, glib::VariantDict)>,)>().unwrap();
        let disconnect = &groups[0].1;
        assert_eq!(
            lookup::<String>(disconnect, "label").as_deref(),
            Some("Disconnect")
        );
        assert_eq!(lookup::<bool>(disconnect, "visible"), Some(true));
        assert_eq!(lookup::<bool>(disconnect, "enabled"), Some(true));
    }

    #[test]
    fn get_property_returns_one_value() {
        let value = |state: &State, id: i32, name: &str| {
            let reply = call(state, "GetProperty", (id, name)).unwrap();
            assert_eq!(reply.type_().as_str(), "(v)");
            reply.child_value(0).as_variant().unwrap()
        };
        let mut state = State::default();
        assert_eq!(value(&state, 4, "enabled").get::<bool>(), Some(false));
        state.update("Connected", true, true);
        assert_eq!(value(&state, 4, "enabled").get::<bool>(), Some(true));
        assert_eq!(value(&state, 1, "label").str(), Some("Connected"));
        assert_eq!(value(&state, 0, "children-display").str(), Some("submenu"));
    }

    #[test]
    fn unknown_items_and_properties_are_invalid_arguments() {
        let state = connected();
        let requests = [
            ("GetLayout", (7i32, -1i32, all()).to_variant()),
            ("GetLayout", (-1i32, -1i32, all()).to_variant()),
            ("GetGroupProperties", (vec![2i32, 9], all()).to_variant()),
            ("GetProperty", (9i32, "label").to_variant()),
            ("GetProperty", (2i32, "icon-name").to_variant()),
            ("GetProperty", (5i32, "label").to_variant()),
            ("GetLayout", ("wrong", "types").to_variant()),
        ];
        for (method, args) in requests {
            assert_eq!(
                call(&state, method, args).unwrap_err(),
                MenuError::InvalidArgs,
                "{method}"
            );
        }
        let error = MenuError::InvalidArgs;
        assert_eq!(error.name(), "com.canonical.dbusmenu.InvalidArgs");
        assert_eq!(error.message(), "Unknown menu item or property");
    }

    #[test]
    fn unknown_methods_are_rejected() {
        let error = call(&connected(), "Bogus", ()).unwrap_err();
        assert_eq!(error, MenuError::UnknownMethod);
        assert_eq!(error.name(), "com.canonical.dbusmenu.UnknownMethod");
        assert_eq!(error.message(), "Unsupported menu method");
    }

    #[test]
    fn clicks_map_to_callbacks() {
        let mut state = connected();
        let expected = [
            (2, Callback::Show(None)),
            (3, settings(None)),
            (4, Callback::Disconnect),
        ];
        for (id, callback) in expected.into_iter().chain([(6, Callback::Quit)]) {
            assert_eq!(clicks(&state, click(id, "clicked")), [callback]);
        }
        for (id, event) in [
            (0, "clicked"),
            (1, "clicked"),
            (5, "clicked"),
            (42, "clicked"),
            (2, "hovered"),
        ] {
            assert!(clicks(&state, click(id, event)).is_empty(), "{id} {event}");
        }
        state.update("Disconnected", false, false);
        assert!(
            clicks(&state, click(4, "clicked")).is_empty(),
            "Disconnect is disabled"
        );
    }

    #[test]
    fn only_clicks_are_interactions() {
        let state = connected();
        let clicked = event(&state, click(1, "clicked"));
        assert!(clicked.interaction, "even on an item that does nothing");
        assert!(clicked.callbacks.is_empty());
        for name in ["hovered", "opened", "closed"] {
            assert_eq!(event(&state, click(2, name)), Requests::default(), "{name}");
        }
    }

    #[test]
    fn event_groups_click_each_item_and_report_no_errors() {
        let events = vec![
            click(2, "clicked"),
            click(42, "clicked"),
            click(6, "clicked"),
        ];
        let mut requests = Requests::default();
        let reply = connected().menu_call("EventGroup", &(events,).to_variant(), &mut requests);
        let reply = reply.unwrap().unwrap();
        assert_eq!(requests.callbacks, [Callback::Show(None), Callback::Quit]);
        assert!(requests.interaction);
        assert_eq!(reply.type_().as_str(), "(ai)");
        assert_eq!(reply.child_value(0).n_children(), 0);
    }

    #[test]
    fn about_to_show_never_needs_an_update() {
        let state = State::default();
        let reply = call(&state, "AboutToShow", (0i32,)).unwrap();
        assert_eq!(reply.get::<(bool,)>(), Some((false,)));
        let reply = call(&state, "AboutToShowGroup", (vec![0i32, 1],)).unwrap();
        assert_eq!(reply.type_().as_str(), "(aiai)");
        assert_eq!(
            reply.get::<(Vec<i32>, Vec<i32>)>(),
            Some((Vec::new(), Vec::new()))
        );
    }

    #[test]
    fn activating_the_icon_opens_the_window() {
        for method in ["Activate", "SecondaryActivate", "ContextMenu"] {
            assert_eq!(activation(method), Some(Callback::Show(None)), "{method}");
        }
        assert_eq!(activation("Scroll"), None);
    }

    #[test]
    fn presenting_the_window_takes_the_latest_token() {
        let shared = shared_with("");
        shared.update("Connected", true, true);
        let item = |method: &str| request(&shared, Interface::Item, method, (0i32, 0i32));
        let menu = |id| request(&shared, Interface::Menu, "Event", click(id, "clicked"));

        provide_token(&shared, "old");
        provide_token(&shared, "new");
        assert_eq!(item("Activate"), show(Some("new")));
        assert_eq!(item("SecondaryActivate"), show(None), "used once");
        provide_token(&shared, "middle");
        assert_eq!(item("SecondaryActivate"), show(Some("middle")));
        provide_token(&shared, "context");
        assert_eq!(item("ContextMenu"), show(Some("context")));

        provide_token(&shared, "settings");
        assert_eq!(menu(3), [settings(Some("settings"))]);
        provide_token(&shared, "menu");
        assert_eq!(menu(2), show(Some("menu")));

        provide_token(&shared, "");
        assert_eq!(item("Activate"), show(None), "an empty token is none");
    }

    #[test]
    fn any_other_interaction_drops_the_token() {
        let shared = shared_with("");
        shared.update("Connected", true, true);
        let item = |method: &str| request(&shared, Interface::Item, method, (0i32, 0i32));
        let menu = |id| request(&shared, Interface::Menu, "Event", click(id, "clicked"));

        provide_token(&shared, "scroll");
        let scroll = request(&shared, Interface::Item, "Scroll", (1i32, "vertical"));
        assert!(scroll.is_empty(), "{scroll:?}");
        assert_eq!(item("Activate"), show(None));
        provide_token(&shared, "disconnect");
        assert_eq!(menu(4), [Callback::Disconnect]);
        assert_eq!(menu(3), [settings(None)]);
        provide_token(&shared, "status");
        assert!(menu(1).is_empty(), "the status line does nothing");
        assert_eq!(menu(2), show(None));

        // Reading the menu is no interaction: hosts may do it between the
        // token and the click.
        provide_token(&shared, "kept");
        request(&shared, Interface::Menu, "GetLayout", (0i32, -1i32, all()));
        request(&shared, Interface::Menu, "AboutToShow", (0i32,));
        request(&shared, Interface::Menu, "Event", click(0, "opened"));
        assert_eq!(menu(2), show(Some("kept")));
    }

    #[test]
    fn a_token_is_only_for_the_host_that_sent_it() {
        let shared = shared_with("");
        let other = Some(":1.7");
        let activate =
            |sender| request_from(&shared, sender, Interface::Item, "Activate", (0i32, 0i32));

        provide_token_from(&shared, other, "other");
        assert_eq!(activate(HOST), show(None));
        assert_eq!(
            activate(other),
            show(None),
            "dropped by the first activation"
        );
        provide_token_from(&shared, other, "own");
        assert_eq!(activate(other), show(Some("own")));
        provide_token_from(&shared, None, "peer");
        assert_eq!(activate(HOST), show(None), "not from a direct connection");
    }

    #[test]
    fn event_groups_give_the_token_to_the_first_window_they_present() {
        let shared = shared_with("");
        shared.update("Connected", true, true);
        let group =
            |events: Vec<MenuEvent>| request(&shared, Interface::Menu, "EventGroup", (events,));

        provide_token(&shared, "group");
        let clicks = group(vec![
            click(4, "clicked"),
            click(3, "clicked"),
            click(2, "clicked"),
        ]);
        let expected = [
            Callback::Disconnect,
            settings(Some("group")),
            Callback::Show(None),
        ];
        assert_eq!(clicks, expected);

        provide_token(&shared, "hover");
        assert!(group(vec![click(2, "hovered")]).is_empty());
        assert_eq!(group(vec![click(2, "clicked")]), show(Some("hover")));
        provide_token(&shared, "quit");
        assert_eq!(group(vec![click(6, "clicked")]), [Callback::Quit]);
        assert_eq!(group(vec![click(2, "clicked")]), show(None));
    }

    #[test]
    fn the_status_label_shows_underscores_literally() {
        let mut state = State::default();
        state.update("Connected · office_vpn", true, true);
        let reply = call(&state, "GetProperty", (1i32, "label")).unwrap();
        let label = reply.child_value(0).as_variant().unwrap();
        assert_eq!(label.str(), Some("Connected · office__vpn"));
        let reply = call(&state, "GetLayout", (0i32, -1i32, all())).unwrap();
        let (_, root) = layout_reply(&reply);
        let status = parts(&parts(&root).2[0]).1;
        assert_eq!(
            lookup::<String>(&status, "label").as_deref(),
            Some("Connected · office__vpn")
        );

        // Tooltips have no mnemonics.
        let shared = shared_with("");
        shared.update("Connected · office_vpn", true, true);
        let tooltip = shared.property(Interface::Item, "ToolTip");
        assert_eq!(tooltip.child_value(3).str(), Some("Connected · office_vpn"));
    }

    #[test]
    fn each_activation_keeps_the_token_sent_before_it() {
        let context = glib::MainContext::new();
        let test = || {
            let (actions, log) = recorder();
            let shared = Rc::new(Shared::new(actions, String::new()).unwrap());
            // Both clicks arrive before either callback runs.
            for token in ["first", "second"] {
                provide_token(&shared, token);
                for callback in request(&shared, Interface::Item, "Activate", (0i32, 0i32)) {
                    shared.defer(callback);
                }
            }
            assert_eq!(*log.borrow(), Vec::<String>::new());
            while context.iteration(false) {}
            assert_eq!(*log.borrow(), ["show first", "show second"]);
        };
        context.with_thread_default(test).unwrap();
    }

    #[test]
    fn closing_releases_the_actions_and_the_token() {
        let (actions, log) = recorder();
        let shared = Shared::new(actions, String::new()).unwrap();
        provide_token(&shared, "token");
        assert!(Rc::strong_count(&log) > 1, "the actions hold the log");
        shared.close();
        assert_eq!(Rc::strong_count(&log), 1);
        assert!(shared.actions.borrow().is_none());
        assert_eq!(shared.activation_token.take(), None);
        shared.close();
        assert!(shared.closed.get());
    }

    #[test]
    fn an_action_may_close_the_tray() {
        let context = glib::MainContext::new();
        let test = || {
            let target = Rc::new(RefCell::new(std::rc::Weak::<Shared>::new()));
            let (mut actions, log) = recorder();
            let (tray, quit) = (Rc::clone(&target), actions.quit);
            actions.quit = Rc::new(move || {
                if let Some(shared) = tray.borrow().upgrade() {
                    shared.close();
                }
                // Closing released the actions, yet this one keeps running.
                quit();
            });
            let shared = Rc::new(Shared::new(actions, String::new()).unwrap());
            *target.borrow_mut() = Rc::downgrade(&shared);
            shared.defer(Callback::Quit);
            shared.defer(settings(None));
            while context.iteration(false) {}
            assert_eq!(*log.borrow(), ["quit"], "nothing runs once closed");
            assert!(shared.actions.borrow().is_none());
        };
        context.with_thread_default(test).unwrap();
    }

    #[test]
    fn updates_report_what_changed() {
        let mut state = State::default();
        assert_eq!(state.icon(), DISCONNECTED_ICON);
        assert_eq!(state.update(INITIAL_STATUS, false, false), None);
        let change = |revision, icon| Some(Change { revision, icon });
        assert_eq!(state.update("Connecting", false, false), change(2, false));
        assert_eq!(state.update("Connected", true, true), change(3, true));
        assert_eq!(state.icon(), CONNECTED_ICON);
        assert_eq!(state.update("Connected", true, true), None);
        assert_eq!(state.update("Connected", false, true), change(4, false));
        assert_eq!(state.update("Connected", false, false), change(5, true));
        assert_eq!(state.icon(), DISCONNECTED_ICON);
    }

    #[test]
    fn callbacks_run_later_and_never_after_close() {
        let context = glib::MainContext::new();
        let test = || {
            let (actions, log) = recorder();
            let shared = Rc::new(Shared::new(actions, String::new()).unwrap());
            let run = || while context.iteration(false) {};
            shared.defer(Callback::Show(Some("token".to_owned())));
            shared.set_available(true);
            assert!(log.borrow().is_empty(), "callbacks wait for the main loop");
            assert!(shared.available.get());
            run();
            assert_eq!(*log.borrow(), ["show token", "available true"]);

            shared.update("Connected", true, true);
            shared.defer(Callback::Disconnect);
            shared.update("Disconnecting", false, true);
            shared.defer(settings(Some("panel")));
            run();
            let expected = ["show token", "available true", "settings panel"];
            assert_eq!(*log.borrow(), expected);

            shared.defer(Callback::Quit);
            shared.close();
            shared.set_available(false);
            run();
            assert_eq!(*log.borrow(), expected);
            assert!(!shared.available.get());
            assert_eq!(shared.update("Connected", true, true), None, "closed");
        };
        context.with_thread_default(test).unwrap();
    }

    #[test]
    fn callbacks_may_call_back_into_the_tray() {
        let context = glib::MainContext::new();
        let test = || {
            let target = Rc::new(RefCell::new(std::rc::Weak::<Shared>::new()));
            let (mut actions, _) = recorder();
            let tray = Rc::clone(&target);
            actions.disconnect = Rc::new(move || {
                if let Some(shared) = tray.borrow().upgrade() {
                    shared.update("Disconnecting", false, true);
                }
            });
            let shared = Rc::new(Shared::new(actions, String::new()).unwrap());
            *target.borrow_mut() = Rc::downgrade(&shared);
            shared.update("Connected", true, true);
            shared.defer(Callback::Disconnect);
            while context.iteration(false) {}
            assert_eq!(shared.state.borrow().text, "Disconnecting");
        };
        context.with_thread_default(test).unwrap();
    }

    #[test]
    fn installed_icons_sit_beside_the_executable_prefix() {
        let prefix = tempfile::tempdir().unwrap();
        let apps = prefix.path().join("share/icons/hicolor/symbolic/apps");
        fs::create_dir_all(&apps).unwrap();
        let exe = prefix.path().join("bin/forti-vpn");
        assert_eq!(installed_icons(&exe), None, "no icon installed yet");
        fs::write(apps.join(format!("{CONNECTED_ICON}.svg")), "<svg/>").unwrap();
        assert_eq!(
            installed_icons(&exe),
            Some(prefix.path().join("share/icons"))
        );
        assert_eq!(installed_icons(Path::new("/forti-vpn")), None);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_builds_fall_back_to_the_source_tree() {
        let found = PathBuf::from(icon_theme_path());
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/icons");
        assert_eq!(
            found.canonicalize().unwrap(),
            source.canonicalize().unwrap()
        );
        assert!(
            !found.to_string_lossy().contains(".."),
            "{}",
            found.display()
        );
        let apps = found.join("hicolor/symbolic/apps");
        for icon in [CONNECTED_ICON, DISCONNECTED_ICON] {
            assert!(apps.join(format!("{icon}.svg")).is_file(), "{icon}");
        }
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn release_builds_never_advertise_the_source_tree() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let source = crates.parent().unwrap().join("data/icons");
        assert!(
            source.is_dir(),
            "a debug build would use {}",
            source.display()
        );
        assert_ne!(PathBuf::from(icon_theme_path()), source);
    }
}
