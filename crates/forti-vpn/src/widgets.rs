//! Small builders for the labelled controls used throughout the app: a port
//! of the Python `forti_vpn/widgets.py`.
use gtk::prelude::*;
use gtk::{AccessibleAnnouncementPriority, pango};

/// A wrapping, start-aligned label, as every text block in the app is.
pub(crate) fn label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .build()
}

pub(crate) fn styled_label(text: &str, class: &str) -> gtk::Label {
    let label = label(text);
    label.add_css_class(class);
    label
}

/// A centred label, for headings and status lines.
pub(crate) fn centered_label(text: &str, class: Option<&str>) -> gtk::Label {
    let label = label(text);
    label.set_xalign(0.5);
    if let Some(class) = class {
        label.add_css_class(class);
    }
    label
}

/// A selectable, wrapping label for messages users may want to copy.
pub(crate) fn message_label(class: Option<&str>) -> gtk::Label {
    let label = label("");
    label.set_selectable(true);
    if let Some(class) = class {
        label.add_css_class(class);
    }
    label
}

/// A selectable monospace label for a terminal command. Commands contain
/// long paths, so they may break anywhere.
pub(crate) fn command_label(command: &str) -> gtk::Label {
    let label = label(command);
    label.set_selectable(true);
    label.add_css_class("monospace");
    label.set_wrap_mode(pango::WrapMode::WordChar);
    // A hyphen added at a line break would read as part of the command.
    let attributes = pango::AttrList::new();
    attributes.insert(pango::AttrInt::new_insert_hyphens(false));
    label.set_attributes(Some(&attributes));
    label
}

/// A terminal command shown in full, with a button below that copies it.
pub(crate) struct CommandBlock {
    pub(crate) container: gtk::Box,
    pub(crate) label: gtk::Label,
    pub(crate) copy: gtk::Button,
}

impl CommandBlock {
    pub(crate) fn new(command: &str) -> Self {
        let label = command_label(command);
        let copy = button("Copy Command", None);
        copy.set_halign(gtk::Align::Start);
        let text = command.to_owned();
        copy.connect_clicked(move |button| {
            copy_with_feedback(button, &text, "Command copied");
        });
        let container = vertical_box(8);
        container.append(&label);
        container.append(&copy);
        Self {
            container,
            label,
            copy,
        }
    }
}

pub(crate) fn button(text: &str, class: Option<&str>) -> gtk::Button {
    let button = gtk::Button::with_label(text);
    if let Some(class) = class {
        button.add_css_class(class);
    }
    button
}

/// A button showing an icon and `text`. `description` (e.g. "Edit
/// profile") is its tooltip and accessible name, since the visible text
/// alone does not say what it acts on.
pub(crate) fn icon_button(text: &str, icon: &str, description: &str) -> gtk::Button {
    let content = gtk::Box::builder()
        .spacing(6)
        .halign(gtk::Align::Center)
        .build();
    content.append(&gtk::Image::from_icon_name(icon));
    content.append(&gtk::Label::new(Some(text)));
    let button = gtk::Button::builder()
        .child(&content)
        .tooltip_text(description)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(description)]);
    button
}

/// A flat icon-only button; `description` is its tooltip and accessible name.
pub(crate) fn flat_icon_button(icon: &str, description: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(description)
        .valign(gtk::Align::Center)
        .build();
    button.add_css_class("flat");
    button.update_property(&[gtk::accessible::Property::Label(description)]);
    button
}

/// A link whose text wraps, so long titles fit narrow windows.
pub(crate) fn link(title: &str, uri: &str) -> gtk::LinkButton {
    let link = gtk::LinkButton::with_label(uri, title);
    if let Some(label) = link.child().and_downcast::<gtk::Label>() {
        label.set_wrap(true);
        label.set_wrap_mode(pango::WrapMode::WordChar);
    }
    link
}

pub(crate) fn vertical_box(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

pub(crate) fn margins(widget: &impl IsA<gtk::Widget>, amount: i32) {
    widget.set_margin_top(amount);
    widget.set_margin_bottom(amount);
    widget.set_margin_start(amount);
    widget.set_margin_end(amount);
}

pub(crate) fn set_css_class(widget: &impl IsA<gtk::Widget>, class: &str, enabled: bool) {
    if enabled {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

/// Copy `text` to the clipboard, then relabel `button` and tell screen
/// reader users what happened.
pub(crate) fn copy_with_feedback(button: &gtk::Button, text: &str, announcement: &str) {
    button.clipboard().set_text(text);
    button.set_label("Command Copied");
    button.announce(announcement, AccessibleAnnouncementPriority::Medium);
}
