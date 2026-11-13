//! Right pane: the open note (reading or editing) and its header buttons.

use gtk::pango;
use gtk::prelude::*;

use crate::editor::Editor;

pub struct NoteView {
    pub page: adw::NavigationPage,
    pub title: adw::WindowTitle,
    pub banner: adw::Banner,
    pub stack: gtk::Stack,
    pub scroller: gtk::ScrolledWindow,
    pub date: gtk::Label,
    pub info: gtk::Box,
    pub info_label: gtk::Label,
    pub body: adw::Bin,
    pub editor: Editor,
    pub edit_button: gtk::Button,
    pub delete_button: gtk::Button,
    pub save_button: gtk::Button,
    pub cancel_button: gtk::Button,
}

impl NoteView {
    pub fn new() -> NoteView {
        let title = adw::WindowTitle::new("", "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let edit_button = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .action_name("win.edit")
            .tooltip_text("Edit (Ctrl+E)")
            .build();
        let delete_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .action_name("win.delete")
            .tooltip_text("Delete")
            .build();
        let save_button = gtk::Button::builder()
            .label("_Save")
            .use_underline(true)
            .action_name("win.save")
            .tooltip_text("Save (Ctrl+S)")
            .build();
        save_button.add_css_class("suggested-action");
        let cancel_button = gtk::Button::builder()
            .label("_Cancel")
            .use_underline(true)
            .action_name("win.cancel-edit")
            .build();
        header.pack_start(&cancel_button);
        header.pack_end(&save_button);
        header.pack_end(&edit_button);
        header.pack_end(&delete_button);

        let banner = adw::Banner::new("Can't reach your Mac — showing saved notes");

        // Reading view: date, optional info line, body.
        let date = gtk::Label::builder().xalign(0.5).build();
        date.add_css_class("caption");
        date.add_css_class("dim-label");
        date.add_css_class("numeric");
        let info_label = gtk::Label::builder().wrap(true).xalign(0.0).build();
        info_label.add_css_class("caption");
        let info_icon = gtk::Image::from_icon_name("info-outline-symbolic");
        let info = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::Center)
            .build();
        info.add_css_class("note-info");
        info.append(&info_icon);
        info.append(&info_label);
        let body = adw::Bin::new();
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(14)
            .margin_bottom(48)
            .margin_start(18)
            .margin_end(18)
            .build();
        column.append(&date);
        column.append(&info);
        column.append(&body);
        let clamp = adw::Clamp::builder()
            .maximum_size(720)
            .tightening_threshold(560)
            .child(&column)
            .build();
        // Natural height so pictures and tables get the height their width calls for.
        let viewport = gtk::Viewport::builder()
            .vscroll_policy(gtk::ScrollablePolicy::Natural)
            .child(&clamp)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&viewport)
            .build();
        scroller.add_css_class("view");

        let none = adw::StatusPage::builder()
            .icon_name("com.plgeek.FruityNotes-symbolic")
            .title("No Note Selected")
            .build();
        none.add_css_class("dim-status");
        none.add_css_class("compact");
        let locked = adw::StatusPage::builder()
            .icon_name("system-lock-screen-symbolic")
            .title("This Note Is Locked")
            .description("Open it on your iPhone or Mac to read it.")
            .build();
        locked.add_css_class("compact");
        let missing = adw::StatusPage::builder()
            .icon_name("network-offline-symbolic")
            .title("Not Saved on This Computer")
            .description("This note will appear when your Mac is reachable again.")
            .build();
        missing.add_css_class("compact");
        let spinner = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let editor = Editor::new();

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .build();
        stack.add_named(&none, Some("none"));
        stack.add_named(&spinner, Some("loading"));
        stack.add_named(&locked, Some("locked"));
        stack.add_named(&missing, Some("missing"));
        stack.add_named(&scroller, Some("note"));
        stack.add_named(&editor.scroller, Some("editor"));
        stack.set_visible_child_name("none");

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&banner);
        toolbar.set_content(Some(&stack));
        let page = adw::NavigationPage::builder()
            .title("Note")
            .tag("note")
            .child(&toolbar)
            .build();

        info_label.set_ellipsize(pango::EllipsizeMode::None);
        NoteView {
            page,
            title,
            banner,
            stack,
            scroller,
            date,
            info,
            info_label,
            body,
            editor,
            edit_button,
            delete_button,
            save_button,
            cancel_button,
        }
    }

    pub fn scroll_to_top(&self) {
        self.scroller.vadjustment().set_value(0.0);
    }
}
