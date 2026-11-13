//! GTK pieces shared by the Reminders and Notes apps: the connection preferences dialog,
//! background work, the event-stream follower, screenshots and small app-shell helpers.

pub mod prefs;

use std::rc::{Rc, Weak};

use adw::prelude::*;
use fruity_bridge_client::{Config, Event, Subscription};
use gtk::{gio, glib};

/// Runs a blocking call (a bridge request, file I/O, image decoding) on GIO's worker pool and
/// resumes on the main loop with its result.
pub async fn bg<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    match gio::spawn_blocking(f).await {
        Ok(v) => v,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Follows the bridge's event stream and calls `handle` on the main loop for each event while
/// `target` (usually the window) is alive. Keep the returned subscription with the target;
/// dropping it stops the stream.
pub fn follow_events<T: 'static>(
    config: &Config,
    target: Weak<T>,
    handle: impl Fn(&Rc<T>, Event) + 'static,
) -> Subscription {
    let (tx, rx) = async_channel::unbounded::<Event>();
    let subscription = fruity_bridge_client::subscribe(config, move |ev| {
        let _ = tx.send_blocking(ev);
    });
    glib::spawn_future_local(async move {
        // Ends when the subscription is dropped (its thread drops `tx`) or the target goes.
        while let Ok(ev) = rx.recv().await {
            let Some(target) = target.upgrade() else { break };
            handle(&target, ev);
        }
    });
    subscription
}

/// Parses `1100x720`.
pub fn parse_size(s: &str) -> Option<(i32, i32)> {
    let (w, h) = s.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Renders a window to PNG without a compositor or a screen grab.
pub fn capture(window: &impl IsA<gtk::Window>, path: &str) -> Result<(), String> {
    let window = window.upcast_ref::<gtk::Window>();
    let (w, h) = (window.width(), window.height());
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let node = snapshot.to_node().ok_or("nothing was drawn")?;
    let renderer = window.renderer().ok_or("window has no renderer")?;
    let texture = renderer.render_texture(
        &node,
        Some(&gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32)),
    );
    texture.save_to_png(path).map_err(|e| e.to_string())
}

/// The About dialog both apps show.
pub fn about_dialog(name: &str, app_id: &str, version: &str, comments: &str) -> adw::AboutDialog {
    adw::AboutDialog::builder()
        .application_name(name)
        .application_icon(app_id)
        .version(version)
        .comments(comments)
        .license_type(gtk::License::MitX11)
        .build()
}

/// A shortcuts dialog with the app's own section followed by the General one.
pub fn shortcuts_dialog(app_name: &str, items: &[(&str, &str)]) -> adw::ShortcutsDialog {
    let dialog = adw::ShortcutsDialog::new();
    let own = adw::ShortcutsSection::new(Some(app_name));
    for (title, accel) in items {
        own.add(adw::ShortcutsItem::new(title, accel));
    }
    dialog.add(own);
    let general = adw::ShortcutsSection::new(Some("General"));
    for (title, accel) in [
        ("Preferences", "<Control>comma"),
        ("Keyboard Shortcuts", "<Control>question"),
        ("Quit", "<Control>q"),
    ] {
        general.add(adw::ShortcutsItem::new(title, accel));
    }
    dialog.add(general);
    dialog
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1100x720"), Some((1100, 720)));
        assert_eq!(parse_size("1100"), None);
        assert_eq!(parse_size("ax2"), None);
    }
}
