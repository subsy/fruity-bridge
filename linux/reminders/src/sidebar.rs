//! Sidebar: smart-list tiles (Today, Scheduled, All, Completed) and the account's lists.

use std::cell::RefCell;
use std::rc::Rc;

use fruity_bridge_client::reminders::List;
use chrono::Datelike;
use gtk::prelude::*;

use crate::state::{Counts, View};
use crate::style;

pub type OnSelect = Rc<dyn Fn(View)>;
type ListShape = (String, String, Option<String>, Option<String>);

struct Tile {
    view: View,
    title: &'static str,
    button: gtk::Button,
    count: gtk::Label,
}

struct ListRow {
    id: String,
    row: gtk::ListBoxRow,
    count: gtk::Label,
    listbox: gtk::ListBox,
}

pub struct Sidebar {
    pub widget: gtk::ScrolledWindow,
    tiles: Vec<Tile>,
    today_number: gtk::Label,
    lists_box: gtk::Box,
    rows: RefCell<Vec<ListRow>>,
    /// (id, title, colour, account) of the lists currently shown, to skip needless rebuilds.
    shape: RefCell<Vec<ListShape>>,
    on_select: OnSelect,
}

impl Sidebar {
    pub fn new(on_select: OnSelect) -> Rc<Self> {
        let grid = gtk::Grid::builder()
            .column_spacing(10)
            .row_spacing(10)
            .column_homogeneous(true)
            .css_classes(["smart-tiles"])
            .build();

        let today_number = gtk::Label::new(None);
        let mut tiles = Vec::new();
        let specs: [(View, &str, &str, Option<&str>); 4] = [
            (View::Today, "Today", "tile-today", None),
            (
                View::Scheduled,
                "Scheduled",
                "tile-scheduled",
                Some("x-office-calendar-symbolic"),
            ),
            (
                View::All,
                "All",
                "tile-all",
                Some("view-list-bullet-symbolic"),
            ),
            (
                View::Completed,
                "Completed",
                "tile-completed",
                Some("object-select-symbolic"),
            ),
        ];
        for (i, (view, title, class, icon)) in specs.into_iter().enumerate() {
            let icon_box: gtk::Widget = match icon {
                Some(name) => gtk::Image::builder()
                    .icon_name(name)
                    .pixel_size(16)
                    .build()
                    .upcast(),
                None => today_number.clone().upcast(),
            };
            icon_box.add_css_class("tile-icon");
            icon_box.set_halign(gtk::Align::Start);
            icon_box.set_valign(gtk::Align::Start);
            let count = gtk::Label::builder()
                .css_classes(["tile-count"])
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .hexpand(true)
                .build();
            let label = gtk::Label::builder()
                .label(title)
                .css_classes(["tile-title"])
                .xalign(0.0)
                .margin_top(4)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            let inner = gtk::Grid::new();
            inner.attach(&icon_box, 0, 0, 1, 1);
            inner.attach(&count, 1, 0, 1, 1);
            inner.attach(&label, 0, 1, 2, 1);
            let button = gtk::Button::builder()
                .child(&inner)
                .css_classes(["smart-tile", class])
                .build();
            button.update_property(&[gtk::accessible::Property::Label(title)]);
            grid.attach(&button, (i % 2) as i32, (i / 2) as i32, 1, 1);
            tiles.push(Tile {
                view,
                title,
                button,
                count,
            });
        }

        let lists_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_bottom(12)
            .build();
        content.append(&grid);
        content.append(&lists_box);

        let widget = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&content)
            .vexpand(true)
            .build();

        let sidebar = Rc::new(Sidebar {
            widget,
            tiles,
            today_number,
            lists_box,
            rows: RefCell::new(Vec::new()),
            shape: RefCell::new(Vec::new()),
            on_select,
        });
        for tile in &sidebar.tiles {
            let view = tile.view.clone();
            let cb = sidebar.on_select.clone();
            tile.button.connect_clicked(move |_| cb(view.clone()));
        }
        sidebar
    }

    /// Refreshes counts (`None` before anything has loaded), rebuilds list rows when the lists
    /// changed, and marks `view` selected.
    pub fn update(&self, lists: &[List], counts: Option<Counts>, view: &View) {
        self.today_number
            .set_label(&chrono::Local::now().day().to_string());
        for tile in &self.tiles {
            let n = counts.and_then(|c| match tile.view {
                View::Today => Some(c.today),
                View::Scheduled => Some(c.scheduled),
                View::All => Some(c.all),
                _ => None,
            });
            tile.count
                .set_label(&n.map(|n| n.to_string()).unwrap_or_default());
            let a11y = match n {
                Some(n) => format!("{}, {n}", tile.title),
                None => tile.title.to_string(),
            };
            tile.button
                .update_property(&[gtk::accessible::Property::Label(&a11y)]);
        }

        let shape: Vec<_> = lists
            .iter()
            .map(|l| {
                (
                    l.id.clone(),
                    l.title.clone(),
                    l.color.clone(),
                    l.account.clone(),
                )
            })
            .collect();
        if *self.shape.borrow() != shape {
            self.rebuild(lists);
            *self.shape.borrow_mut() = shape;
        }
        for row in self.rows.borrow().iter() {
            if let Some(list) = lists.iter().find(|l| l.id == row.id) {
                row.count.set_label(&match list.open_count {
                    Some(n) if n > 0 => n.to_string(),
                    _ => String::new(),
                });
            }
        }
        self.select(view);
    }

    fn select(&self, view: &View) {
        for tile in &self.tiles {
            if &tile.view == view {
                tile.button.add_css_class("selected");
            } else {
                tile.button.remove_css_class("selected");
            }
        }
        let rows = self.rows.borrow();
        let wanted = match view {
            View::List(id) => rows.iter().find(|r| &r.id == id),
            _ => None,
        };
        for r in rows.iter() {
            let selected = wanted.is_some_and(|w| w.row == r.row);
            if selected && !r.row.is_selected() {
                r.listbox.select_row(Some(&r.row));
            } else if !selected && r.row.is_selected() {
                r.listbox.unselect_row(&r.row);
            }
        }
    }

    fn rebuild(&self, lists: &[List]) {
        while let Some(child) = self.lists_box.first_child() {
            self.lists_box.remove(&child);
        }
        let mut rows = Vec::new();
        let mut accounts: Vec<Option<&str>> = Vec::new();
        for l in lists {
            if !accounts.contains(&l.account.as_deref()) {
                accounts.push(l.account.as_deref());
            }
        }
        let single = accounts.len() <= 1;
        for account in accounts {
            let heading = if single {
                "My Lists"
            } else {
                account.unwrap_or("Other")
            };
            let label = gtk::Label::builder()
                .label(heading)
                .xalign(0.0)
                .css_classes(["sidebar-heading"])
                .build();
            self.lists_box.append(&label);

            let listbox = gtk::ListBox::builder()
                .css_classes(["navigation-sidebar"])
                .selection_mode(gtk::SelectionMode::Single)
                .build();
            listbox.update_property(&[gtk::accessible::Property::Label(heading)]);
            for list in lists.iter().filter(|l| l.account.as_deref() == account) {
                let (row, count) = list_row(list);
                listbox.append(&row);
                rows.push(ListRow {
                    id: list.id.clone(),
                    row,
                    count,
                    listbox: listbox.clone(),
                });
            }
            let cb = self.on_select.clone();
            listbox
                .connect_row_activated(move |_, row| cb(View::List(row.widget_name().to_string())));
            self.lists_box.append(&listbox);
        }
        if lists.is_empty() {
            let empty = gtk::Label::builder()
                .label("No lists yet")
                .xalign(0.0)
                .css_classes(["sidebar-heading"])
                .build();
            self.lists_box.append(&empty);
        }
        *self.rows.borrow_mut() = rows;
    }
}

fn list_row(list: &List) -> (gtk::ListBoxRow, gtk::Label) {
    let dot = gtk::Image::builder()
        .icon_name("view-list-bullet-symbolic")
        .pixel_size(14)
        .css_classes(["list-icon"])
        .valign(gtk::Align::Center)
        .build();
    style::set_color(&dot, list.color.as_deref());

    let title = gtk::Label::builder()
        .label(&list.title)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let count = gtk::Label::builder().css_classes(["list-count"]).build();
    let hbox = gtk::Box::builder()
        .spacing(10)
        .margin_top(3)
        .margin_bottom(3)
        .build();
    hbox.append(&dot);
    hbox.append(&title);
    if !list.editable {
        let lock = gtk::Image::builder()
            .icon_name("changes-prevent-symbolic")
            .pixel_size(12)
            .css_classes(["dim-label"])
            .tooltip_text("Read only")
            .build();
        hbox.append(&lock);
    }
    hbox.append(&count);
    let row = gtk::ListBoxRow::builder().child(&hbox).build();
    row.set_widget_name(&list.id);
    row.update_property(&[gtk::accessible::Property::Label(&list.title)]);
    (row, count)
}
