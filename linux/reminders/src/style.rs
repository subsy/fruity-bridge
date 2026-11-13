//! App stylesheet plus one generated CSS class per list colour (`.c-ff9500 { --list-color: … }`).

use std::cell::RefCell;
use std::collections::BTreeSet;

use gtk::gdk;

const CSS: &str = include_str!("style.css");

thread_local! {
    static COLORS: RefCell<(gtk::CssProvider, BTreeSet<String>)> = RefCell::new((gtk::CssProvider::new(), BTreeSet::new()));
}

pub fn load() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    COLORS.with(|c| {
        gtk::style_context_add_provider_for_display(
            &display,
            &c.borrow().0,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        )
    });
}

/// Normalises "#FF9500" to "ff9500"; `None` for anything that isn't a 6-digit hex colour.
fn hex(color: &str) -> Option<String> {
    let h = color.trim().trim_start_matches('#').to_ascii_lowercase();
    (h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit())).then_some(h)
}

/// CSS class that sets `--list-color` for a widget and its children. Lists without a usable
/// colour get the system accent.
pub fn color_class(color: Option<&str>) -> String {
    let Some(h) = color.and_then(hex) else {
        return "c-accent".into();
    };
    COLORS.with(|c| {
        let mut c = c.borrow_mut();
        if c.1.insert(h.clone()) {
            let css: String = c
                .1
                .iter()
                .map(|h| {
                    let (r, g, b) = (
                        u8::from_str_radix(&h[0..2], 16).unwrap_or(0),
                        u8::from_str_radix(&h[2..4], 16).unwrap_or(0),
                        u8::from_str_radix(&h[4..6], 16).unwrap_or(0),
                    );
                    format!(
                        ".c-{h} {{ --list-color: #{h}; --list-tint: rgba({r},{g},{b},0.14); }}\n"
                    )
                })
                .collect();
            c.0.load_from_string(&css);
        }
        format!("c-{h}")
    })
}

/// Applies a list colour to `widget`, replacing any earlier one.
pub fn set_color(widget: &impl gtk::prelude::IsA<gtk::Widget>, color: Option<&str>) {
    apply_class(widget, &color_class(color));
}

/// Applies a colour class (from `color_class`, or `c-neutral`/`c-muted`), replacing any earlier one.
pub fn apply_class(widget: &impl gtk::prelude::IsA<gtk::Widget>, class: &str) {
    use gtk::prelude::*;
    if widget.has_css_class(class) {
        return;
    }
    for old in widget.css_classes() {
        if old.starts_with("c-") {
            widget.remove_css_class(&old);
        }
    }
    widget.add_css_class(class);
}
