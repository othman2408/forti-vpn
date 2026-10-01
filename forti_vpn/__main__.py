"""Give actionable dependency instructions even before GTK can start."""
try:
    import gi
    gi.require_version('Gtk', '4.0')
    gi.require_version('Adw', '1')
    from gi.repository import Gtk, Adw
except (ImportError, ValueError):
    raise SystemExit('GUI dependencies are missing. Install them with:\n'
                     'sudo apt install python3 python3-gi gir1.2-gtk-4.0 gir1.2-adw-1\n'
                     'Then launch Forti VPN again.') from None

if not (hasattr(Adw, 'Dialog') and hasattr(Adw, 'PreferencesDialog')
        and hasattr(Adw.PreferencesGroup, 'set_header_suffix')
        and hasattr(Gtk, 'FileDialog') and hasattr(Gtk, 'AccessibleAnnouncementPriority')):
    raise SystemExit('Your GTK/libadwaita packages are too old for this app.\n'
                     'Use Ubuntu 26.04 or a system with compatible GTK 4 and libadwaita packages.')

from .ui import main

raise SystemExit(main())
