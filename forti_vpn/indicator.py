"""GTK 4 compatible StatusNotifierItem and DBusMenu on the session bus."""
from gi.repository import Gio, GLib

ITEM = 'org.kde.StatusNotifierItem'
MENU = 'com.canonical.dbusmenu'
WATCHER = 'org.kde.StatusNotifierWatcher'
ITEM_PATH = '/StatusNotifierItem'
MENU_PATH = '/Menu'
XML = '''<node>
<interface name="org.kde.StatusNotifierItem">
<method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
<method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method>
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
</interface></node>'''


class Indicator:
    def __init__(self, show, settings, disconnect, quit_app, availability, icon_dir):
        self.callbacks = {2: show, 3: settings, 4: disconnect, 6: quit_app}
        self.availability = availability
        self.icon_dir = icon_dir
        self.available = False
        self.closed = False
        self.text = 'Checking VPN status'
        self.disconnect_enabled = False
        self.revision = 1
        self.connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        node = Gio.DBusNodeInfo.new_for_xml(XML)
        self.registrations = [self.connection.register_object(path, iface, self.method, self.property, None)
                              for path, iface in zip((ITEM_PATH, MENU_PATH), node.interfaces)]
        self.host_signal = self.connection.signal_subscribe(WATCHER, WATCHER, None, '/StatusNotifierWatcher', None,
                                                             Gio.DBusSignalFlags.NONE, self.host_changed)
        self.watch = Gio.bus_watch_name_on_connection(self.connection, WATCHER, Gio.BusNameWatcherFlags.NONE,
                                                      self.appeared, self.vanished)

    def appeared(self, connection, name, owner):
        connection.call(WATCHER, '/StatusNotifierWatcher', WATCHER, 'RegisterStatusNotifierItem',
                        GLib.Variant('(s)', (ITEM_PATH,)), None, Gio.DBusCallFlags.NONE, 3000, None,
                        self.registered)

    def registered(self, connection, result):
        try:
            connection.call_finish(result)
            # A watcher can exist without a panel host. Never hide into an absent host.
            connection.call(WATCHER, '/StatusNotifierWatcher', 'org.freedesktop.DBus.Properties', 'Get',
                            GLib.Variant('(ss)', (WATCHER, 'IsStatusNotifierHostRegistered')), None,
                            Gio.DBusCallFlags.NONE, 3000, None, self.host_checked)
        except GLib.Error:
            self.set_available(False)

    def host_checked(self, connection, result):
        try:
            reply = connection.call_finish(result).unpack()[0]
            self.set_available(bool(reply))
        except GLib.Error:
            self.set_available(False)

    def host_changed(self, connection, sender, path, interface, signal, parameters):
        if signal == 'StatusNotifierHostUnregistered':
            self.set_available(False)
        elif signal == 'StatusNotifierHostRegistered':
            self.appeared(connection, WATCHER, sender)

    def vanished(self, *_):
        self.set_available(False)

    def set_available(self, value):
        if self.closed:
            return
        self.available = value
        self.availability(value)

    def menu_properties(self, ident):
        labels = {1: self.text, 2: 'Open Forti VPN', 3: 'Settings', 4: 'Disconnect', 6: 'Disconnect and Quit'}
        if ident == 0:
            return {'children-display': GLib.Variant('s', 'submenu')}
        if ident == 5:
            return {'type': GLib.Variant('s', 'separator')}
        return {'label': GLib.Variant('s', labels[ident]), 'visible': GLib.Variant('b', True),
                'enabled': GLib.Variant('b', ident != 1 and (ident != 4 or self.disconnect_enabled))}

    def layout(self, ident=0, depth=-1, names=()):
        props = self.menu_properties(ident)
        if names:
            props = {key: value for key, value in props.items() if key in names}
        children = [GLib.Variant('(ia{sv}av)', self.layout(i, 0, names)) for i in range(1, 7)] if ident == 0 and depth != 0 else []
        return ident, props, children

    def property(self, connection, sender, path, interface, name):
        if interface == MENU:
            return {'Version': GLib.Variant('u', 3), 'TextDirection': GLib.Variant('s', 'ltr'),
                    'Status': GLib.Variant('s', 'normal'), 'IconThemePath': GLib.Variant('as', [self.icon_dir])}.get(name)
        icon = 'io.github.fortivpn.Utility-symbolic'
        values = {'Category': ('s', 'Communications'), 'Id': ('s', 'forti-vpn'),
                  'Title': ('s', 'Forti VPN'), 'Status': ('s', 'Active'), 'WindowId': ('u', 0),
                  'IconName': ('s', icon), 'IconThemePath': ('s', self.icon_dir),
                  'ToolTip': ('(sa(iiay)ss)', (icon, [], 'Forti VPN', self.text)),
                  'ItemIsMenu': ('b', False), 'Menu': ('o', MENU_PATH)}
        if name.endswith('Pixmap'):
            return GLib.Variant('a(iiay)', [])
        signature, value = values.get(name, ('s', ''))
        return GLib.Variant(signature, value)

    def event(self, ident, event):
        callback = self.callbacks.get(ident)
        if event == 'clicked' and callback and (ident != 4 or self.disconnect_enabled):
            callback()

    def method(self, connection, sender, path, interface, method, params, invocation):
        args = params.unpack()
        try:
            if interface == ITEM:
                if method in ('Activate', 'SecondaryActivate', 'ContextMenu'):
                    self.callbacks[2]()
                invocation.return_value(None)
            elif method == 'GetLayout':
                invocation.return_value(GLib.Variant('(u(ia{sv}av))', (self.revision, self.layout(*args))))
            elif method == 'GetGroupProperties':
                ids, names = args
                properties = [(i, self.layout(i, 0, names)[1]) for i in ids]
                invocation.return_value(GLib.Variant('(a(ia{sv}))', (properties,)))
            elif method == 'GetProperty':
                invocation.return_value(GLib.Variant('(v)', (self.menu_properties(args[0])[args[1]],)))
            elif method == 'Event':
                self.event(args[0], args[1])
                invocation.return_value(None)
            elif method == 'EventGroup':
                for ident, event, data, timestamp in args[0]:
                    self.event(ident, event)
                invocation.return_value(GLib.Variant('(ai)', ([],)))
            elif method == 'AboutToShow':
                invocation.return_value(GLib.Variant('(b)', (False,)))
            elif method == 'AboutToShowGroup':
                invocation.return_value(GLib.Variant('(aiai)', ([], [])))
            else:
                invocation.return_dbus_error('com.canonical.dbusmenu.UnknownMethod', 'Unsupported menu method')
        except (KeyError, ValueError, TypeError):
            invocation.return_dbus_error('com.canonical.dbusmenu.InvalidArgs', 'Unknown menu item or property')

    def update(self, text, disconnect_enabled):
        if self.closed or (text, disconnect_enabled) == (self.text, self.disconnect_enabled):
            return
        self.text, self.disconnect_enabled = text, disconnect_enabled
        self.revision += 1
        self.connection.emit_signal(None, ITEM_PATH, ITEM, 'NewToolTip', None)
        self.connection.emit_signal(None, MENU_PATH, MENU, 'LayoutUpdated', GLib.Variant('(ui)', (self.revision, 0)))

    def close(self):
        self.closed = True
        Gio.bus_unwatch_name(self.watch)
        self.connection.signal_unsubscribe(self.host_signal)
        for registration in self.registrations:
            self.connection.unregister_object(registration)
