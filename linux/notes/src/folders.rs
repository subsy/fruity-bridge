//! Folder sidebar: nested folders with counts, Recently Deleted last.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use fruity_bridge_client::notes::Folder;
use gtk::pango;
use gtk::prelude::*;

use crate::state::{FolderRow, ordered_folders};

pub struct FolderSidebar {
    pub page: adw::NavigationPage,
    list: gtk::ListBox,
    rows: RefCell<Vec<FolderRow>>,
    suppress: Cell<bool>,
}

impl FolderSidebar {
    pub fn new(menu: &gtk::gio::MenuModel) -> Rc<FolderSidebar> {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        list.add_css_class("navigation-sidebar");
        list.update_property(&[gtk::accessible::Property::Label("Folders")]);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();

        let header = adw::HeaderBar::new();
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(menu)
            .primary(true)
            .tooltip_text("Main Menu")
            .build();
        header.pack_end(&menu_button);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&scroller));
        let page = adw::NavigationPage::builder()
            .title("Notes")
            .tag("folders")
            .child(&toolbar)
            .build();

        let this = Rc::new(FolderSidebar {
            page,
            list,
            rows: RefCell::default(),
            suppress: Cell::new(false),
        });
        let weak = Rc::downgrade(&this);
        this.list.set_header_func(move |row, _before| {
            let Some(this) = weak.upgrade() else { return };
            let rows = this.rows.borrow();
            let header = rows
                .get(row.index() as usize)
                .and_then(|r| r.account_header.clone());
            row.set_header(
                header
                    .map(|h| {
                        let l = gtk::Label::builder()
                            .label(h)
                            .xalign(0.0)
                            .margin_start(12)
                            .margin_top(12)
                            .margin_bottom(4)
                            .build();
                        l.add_css_class("heading");
                        l.add_css_class("dim-label");
                        l
                    })
                    .as_ref(),
            );
        });
        this
    }

    /// Calls `f(folder_id)` when the user picks a folder; `activated` when they click the
    /// already-selected one (used to navigate forward on narrow layouts).
    pub fn connect_selected(
        self: &Rc<Self>,
        f: impl Fn(String) + 'static,
        activated: impl Fn() + 'static,
    ) {
        let weak = Rc::downgrade(self);
        self.list.connect_row_selected(move |_, row| {
            let Some(this) = weak.upgrade() else { return };
            if this.suppress.get() {
                return;
            }
            if let Some(row) = row {
                let id = this
                    .rows
                    .borrow()
                    .get(row.index() as usize)
                    .map(|r| r.folder.id.clone());
                if let Some(id) = id {
                    f(id);
                }
            }
        });
        self.list.connect_row_activated(move |_, _| activated());
    }

    pub fn set_folders(&self, folders: &[Folder], selected: Option<&str>) {
        let rows = ordered_folders(folders);
        if *self.rows.borrow() == rows {
            self.select(selected);
            return;
        }
        self.suppress.set(true);
        self.list.remove_all();
        for r in &rows {
            self.list.append(&row_widget(r));
        }
        *self.rows.borrow_mut() = rows;
        self.suppress.set(false);
        self.select(selected);
    }

    pub fn select(&self, id: Option<&str>) {
        self.suppress.set(true);
        let index = id.and_then(|id| self.rows.borrow().iter().position(|r| r.folder.id == id));
        match index.and_then(|i| self.list.row_at_index(i as i32)) {
            Some(row) => self.list.select_row(Some(&row)),
            None => self.list.unselect_all(),
        }
        self.suppress.set(false);
    }
}

fn row_widget(r: &FolderRow) -> gtk::ListBoxRow {
    let f = &r.folder;
    let icon = if f.is_trash {
        "user-trash-symbolic"
    } else if f.is_smart {
        "folder-saved-search-symbolic"
    } else {
        "folder-symbolic"
    };
    let b = gtk::Box::builder()
        .spacing(10)
        .margin_start(r.depth as i32 * 16)
        .build();
    b.append(&gtk::Image::from_icon_name(icon));
    let name = gtk::Label::builder()
        .label(&f.name)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    b.append(&name);
    if let Some(n) = f.note_count {
        let count = gtk::Label::new(Some(&n.to_string()));
        count.add_css_class("dim-label");
        count.add_css_class("numeric");
        b.append(&count);
    }
    let row = gtk::ListBoxRow::builder().child(&b).build();
    row.set_tooltip_text(Some(&f.name));
    row.update_property(&[gtk::accessible::Property::Label(&match f.note_count {
        Some(n) => format!("{}, {} notes", f.name, n),
        None => f.name.clone(),
    })]);
    row
}
