"""Small shared helpers for native labeled controls."""
from gi.repository import Gtk


def label(text, css=None):
    widget = Gtk.Label(label=text, wrap=True, xalign=0)
    if css:
        widget.add_css_class(css)
    return widget


def button(text, callback, css=None):
    widget = Gtk.Button(label=text)
    widget.connect('clicked', callback)
    if css:
        widget.add_css_class(css)
    return widget


def icon_button(text, icon, callback):
    widget = button(text, callback)
    content = Gtk.Box(spacing=6, halign=Gtk.Align.CENTER)
    content.append(Gtk.Image(icon_name=icon))
    content.append(Gtk.Label(label=text))
    widget.set_child(content)
    widget.set_tooltip_text(text + ' profile')
    widget.update_property([Gtk.AccessibleProperty.LABEL], [text + ' profile'])
    return widget


def margins(widget, amount=18):
    for side in ('top', 'bottom', 'start', 'end'):
        getattr(widget, 'set_margin_' + side)(amount)


