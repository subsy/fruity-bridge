//! Note blocks → widgets.
//!
//! Two layers: a pure one (`segments`, `markup`, `marker_text`) that groups blocks and produces
//! Pango markup, which is unit-tested, and a GTK one (`build`) that turns segments into wrapped
//! labels (one per paragraph, lists with hanging indents), code and quote boxes, image slots,
//! attachment cards and table grids.

use adw::prelude::*;
use fruity_bridge_client::notes::{Attachment, Block, Note, Run};
use gtk::{gdk, glib, pango};

// ---------------------------------------------------------------------------------------------
// Pure layer

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParaKind {
    Title,
    Heading,
    Subheading,
    Body,
    List(u32),
    Code,
    Quote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker {
    Bullet,
    Dash,
    Number(u32),
    Check(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Para {
    pub kind: ParaKind,
    pub marker: Option<Marker>,
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    /// Title, headings, body and list paragraphs.
    Text(Vec<Para>),
    /// Consecutive monospace blocks.
    Code(Vec<Para>),
    /// Consecutive quote blocks.
    Quote(Vec<Para>),
    Image(Attachment),
    File(Attachment),
    Table(Vec<Vec<String>>),
}

/// Groups blocks into display segments. Adds a title from `fallback_title` when the note has none.
pub fn segments(blocks: &[Block], fallback_title: &str) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let has_title = matches!(blocks.first(), Some(Block::Title { .. }));
    if !has_title && !fallback_title.is_empty() {
        out.push(Segment::Text(vec![Para {
            kind: ParaKind::Title,
            marker: None,
            runs: vec![Run {
                text: fallback_title.into(),
                ..Default::default()
            }],
        }]));
    }
    // Running numbers per indent level for numbered items without an explicit number.
    let mut counters: Vec<u32> = Vec::new();
    for block in blocks {
        let para = |kind, marker, runs: &Vec<Run>| Para {
            kind,
            marker,
            runs: runs.clone(),
        };
        let (container, p): (u8, Option<Para>) = match block {
            Block::Title { runs } => (0, Some(para(ParaKind::Title, None, runs))),
            Block::Heading { runs } => (0, Some(para(ParaKind::Heading, None, runs))),
            Block::Subheading { runs } => (0, Some(para(ParaKind::Subheading, None, runs))),
            Block::Body { runs } => (0, Some(para(ParaKind::Body, None, runs))),
            Block::Bullet { indent, runs } => (
                0,
                Some(para(ParaKind::List(*indent), Some(Marker::Bullet), runs)),
            ),
            Block::Dash { indent, runs } => (
                0,
                Some(para(ParaKind::List(*indent), Some(Marker::Dash), runs)),
            ),
            Block::Numbered {
                indent,
                number,
                runs,
            } => {
                let level = *indent as usize;
                counters.truncate(level + 1);
                counters.resize(level + 1, 0);
                counters[level] = if *number > 0 {
                    *number
                } else {
                    counters[level] + 1
                };
                let n = counters[level];
                (
                    0,
                    Some(para(ParaKind::List(*indent), Some(Marker::Number(n)), runs)),
                )
            }
            Block::Checklist {
                indent,
                checked,
                runs,
            } => (
                0,
                Some(para(
                    ParaKind::List(*indent),
                    Some(Marker::Check(*checked)),
                    runs,
                )),
            ),
            Block::Monospace { runs } => (1, Some(para(ParaKind::Code, None, runs))),
            Block::Quote { runs } => (2, Some(para(ParaKind::Quote, None, runs))),
            Block::Attachment { attachment } => {
                let shows_picture = matches!(attachment.kind.as_str(), "image" | "drawing")
                    && attachment.url.is_some();
                out.push(if shows_picture {
                    Segment::Image(attachment.clone())
                } else {
                    Segment::File(attachment.clone())
                });
                (9, None)
            }
            Block::Table { rows } => {
                out.push(Segment::Table(rows.clone()));
                (9, None)
            }
            Block::Unknown => (9, None),
        };
        match block {
            Block::Numbered { indent, .. } => counters.truncate(*indent as usize + 1),
            Block::Bullet { indent, .. }
            | Block::Dash { indent, .. }
            | Block::Checklist { indent, .. } => counters.truncate(*indent as usize),
            _ => counters.clear(),
        }
        let Some(p) = p else { continue };
        match (out.last_mut(), container) {
            (Some(Segment::Text(ps)), 0)
            | (Some(Segment::Code(ps)), 1)
            | (Some(Segment::Quote(ps)), 2) => ps.push(p),
            (_, 0) => out.push(Segment::Text(vec![p])),
            (_, 1) => out.push(Segment::Code(vec![p])),
            _ => out.push(Segment::Quote(vec![p])),
        }
    }
    out
}

/// Pango markup for a paragraph's runs. Checked checklist items are struck through and dimmed.
pub fn markup(runs: &[Run], checked: bool) -> String {
    let mut out = String::new();
    for run in runs.iter().filter(|r| !r.text.is_empty()) {
        let mut open = String::new();
        let mut close = Vec::new();
        if let Some(url) = run.link.as_deref().filter(|u| !u.is_empty()) {
            open.push_str(&format!("<a href=\"{}\">", glib::markup_escape_text(url)));
            close.push("</a>");
        }
        for (on, o, c) in [
            (run.bold, "<b>", "</b>"),
            (run.italic, "<i>", "</i>"),
            (run.underline, "<u>", "</u>"),
            (run.strikethrough && !checked, "<s>", "</s>"),
        ] {
            if on {
                open.push_str(o);
                close.push(c);
            }
        }
        out.push_str(&open);
        out.push_str(&glib::markup_escape_text(&run.text));
        close.reverse();
        out.extend(close);
    }
    if checked && !out.is_empty() {
        out = format!("<span strikethrough=\"true\" fgalpha=\"55%\">{out}</span>");
    }
    out
}

/// Text for a list marker; `None` for checks (drawn) and plain paragraphs.
pub fn marker_text(marker: &Marker) -> Option<String> {
    match marker {
        Marker::Bullet => Some("•".into()),
        Marker::Dash => Some("–".into()),
        Marker::Number(n) => Some(format!("{n}.")),
        Marker::Check(_) => None,
    }
}

/// All runs joined.
#[cfg(test)]
pub fn plain(runs: &[Run]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

pub fn attachment_icon(kind: &str) -> &'static str {
    match kind {
        "image" | "drawing" => "image-x-generic-symbolic",
        "pdf" => "x-office-document-symbolic",
        "audio" => "audio-x-generic-symbolic",
        "video" => "video-x-generic-symbolic",
        "link" => "web-browser-symbolic",
        "table" => "x-office-spreadsheet-symbolic",
        _ => "text-x-generic-symbolic",
    }
}

pub fn attachment_label(kind: &str) -> &'static str {
    match kind {
        "image" => "Image",
        "drawing" => "Drawing",
        "pdf" => "PDF document",
        "audio" => "Audio recording",
        "video" => "Video",
        "link" => "Link",
        "table" => "Table",
        "file" => "File",
        _ => "Attachment",
    }
}

// ---------------------------------------------------------------------------------------------
// GTK layer

pub struct Rendered {
    pub root: gtk::Box,
    /// Image placeholders to fill once bytes arrive.
    pub images: Vec<(Attachment, adw::Bin)>,
    /// Cards for attachments the bridge can serve; clicking opens the file.
    pub files: Vec<(Attachment, gtk::Button)>,
}

/// Indent per list level, in pixels.
const LIST_STEP: i32 = 24;

/// A small round check glyph: outline when open, filled accent with a tick when done.
fn check_glyph(checked: bool) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(20)
        .content_height(20)
        .valign(gtk::Align::Start)
        .build();
    area.add_css_class("note-check");
    area.set_draw_func(move |w, cr, width, height| {
        let sm = adw::StyleManager::default();
        let (cx, cy) = (width as f64 / 2.0, height as f64 / 2.0);
        let r = 8.5;
        if checked {
            let a = sm.accent_color_rgba();
            cr.set_source_rgba(a.red() as f64, a.green() as f64, a.blue() as f64, 1.0);
            cr.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
            cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
            cr.set_line_width(1.9);
            cr.set_line_cap(gtk::cairo::LineCap::Round);
            cr.set_line_join(gtk::cairo::LineJoin::Round);
            cr.move_to(cx - 3.8, cy + 0.2);
            cr.line_to(cx - 1.1, cy + 2.9);
            cr.line_to(cx + 3.9, cy - 2.7);
            let _ = cr.stroke();
        } else {
            let fg = w.color();
            cr.set_source_rgba(fg.red() as f64, fg.green() as f64, fg.blue() as f64, 0.4);
            cr.set_line_width(1.5);
            cr.arc(cx, cy, r - 0.75, 0.0, std::f64::consts::TAU);
            let _ = cr.stroke();
        }
    });
    area.update_property(&[gtk::accessible::Property::Label(if checked {
        "Done"
    } else {
        "Not done"
    })]);
    area
}

fn kind_class(kind: ParaKind) -> &'static str {
    match kind {
        ParaKind::Title => "note-title",
        ParaKind::Heading => "note-heading",
        ParaKind::Subheading => "note-subheading",
        ParaKind::Body => "note-para",
        ParaKind::List(_) => "note-list-item",
        ParaKind::Code => "note-code-text",
        ParaKind::Quote => "note-quote-text",
    }
}

fn text_label(markup_text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .use_markup(true)
        .label(markup_text)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .xalign(0.0)
        .hexpand(true)
        .selectable(true)
        .build();
    label.add_css_class(class);
    label
}

/// One paragraph: a wrapped label, with a marker column for list items (hanging indent).
fn para_widget(p: &Para) -> gtk::Widget {
    let checked = matches!(p.marker, Some(Marker::Check(true)));
    let mut text = markup(&p.runs, checked);
    if text.is_empty() {
        text = " ".into();
    }
    let label = text_label(&text, kind_class(p.kind));
    let Some(marker) = &p.marker else {
        return label.upcast();
    };
    let indent = match p.kind {
        ParaKind::List(n) => n as i32,
        _ => 0,
    };
    let row = gtk::Box::builder()
        .spacing(8)
        .margin_start(indent * LIST_STEP)
        .build();
    row.add_css_class("note-list-row");
    match marker_text(marker) {
        Some(m) => {
            let ml = gtk::Label::builder()
                .label(m)
                .xalign(1.0)
                .valign(gtk::Align::Baseline)
                .width_request(16)
                .build();
            ml.add_css_class("note-marker");
            ml.add_css_class(kind_class(p.kind));
            label.set_valign(gtk::Align::Baseline);
            row.append(&ml);
        }
        None => row.append(&check_glyph(checked)),
    }
    row.append(&label);
    row.upcast()
}

fn image_slot() -> adw::Bin {
    let spinner = adw::Spinner::builder()
        .width_request(32)
        .height_request(32)
        .hexpand(true)
        .vexpand(true)
        .build();
    let placeholder = gtk::Box::builder()
        .height_request(200)
        .hexpand(true)
        .build();
    placeholder.add_css_class("note-image-placeholder");
    placeholder.append(&spinner);
    adw::Bin::builder().child(&placeholder).build()
}

/// Shows a loaded image in its slot, scaled to the reading width (small images stay 1:1).
pub fn set_image(slot: &adw::Bin, texture: Option<&gdk::Texture>, att: &Attachment) {
    match texture {
        Some(tex) => {
            let pic = gtk::Picture::for_paintable(tex);
            pic.set_can_shrink(true);
            pic.set_content_fit(gtk::ContentFit::Contain);
            pic.set_halign(if tex.width() >= 480 {
                gtk::Align::Fill
            } else {
                gtk::Align::Start
            });
            pic.add_css_class("note-image");
            pic.set_overflow(gtk::Overflow::Hidden);
            pic.set_alternative_text(att.filename.as_deref());
            slot.set_child(Some(&pic));
        }
        None => slot.set_child(Some(&file_card(att, false).0)),
    }
}

/// A link card's web address, if it has one worth opening.
pub fn link_href(att: &Attachment) -> Option<&str> {
    (att.kind == "link")
        .then_some(att.href.as_deref())
        .flatten()
        .filter(|h| !h.trim().is_empty())
}

fn named(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.trim().is_empty())
}

fn card_title(att: &Attachment) -> &str {
    if att.kind == "link" {
        named(&att.title)
            .or(link_href(att))
            .unwrap_or(attachment_label(&att.kind))
    } else {
        named(&att.filename).unwrap_or(attachment_label(&att.kind))
    }
}

fn card_subtitle(att: &Attachment) -> &str {
    match link_href(att) {
        // The title already shows the address when there's no page title.
        Some(href) if named(&att.title).is_some() => href,
        _ => attachment_label(&att.kind),
    }
}

fn file_card(att: &Attachment, openable: bool) -> (gtk::Widget, Option<gtk::Button>) {
    let row = gtk::Box::builder()
        .spacing(14)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(14)
        .margin_end(14)
        .build();
    let icon = gtk::Image::from_icon_name(attachment_icon(&att.kind));
    icon.set_pixel_size(24);
    icon.add_css_class("note-file-icon");
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    let name = gtk::Label::builder()
        .label(card_title(att))
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::Middle)
        .build();
    name.add_css_class("heading");
    let kind = gtk::Label::builder()
        .label(card_subtitle(att))
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::Middle)
        .build();
    kind.add_css_class("caption");
    kind.add_css_class("dim-label");
    text.append(&name);
    text.append(&kind);
    row.append(&icon);
    row.append(&text);
    if openable {
        let open = gtk::Image::from_icon_name("adw-external-link-symbolic");
        open.add_css_class("dim-label");
        row.append(&open);
        let button = gtk::Button::builder().child(&row).build();
        button.add_css_class("card");
        button.add_css_class("note-file");
        button.set_tooltip_text(Some("Open"));
        (button.clone().upcast(), Some(button))
    } else {
        let frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        frame.add_css_class("card");
        frame.add_css_class("note-file");
        frame.append(&row);
        (frame.upcast(), None)
    }
}

fn table(rows: &[Vec<String>]) -> gtk::Widget {
    let grid = gtk::Grid::new();
    let ncols = rows.iter().map(Vec::len).max().unwrap_or(0);
    for (r, row) in rows.iter().enumerate() {
        for c in 0..ncols {
            let text = row.get(c).map(String::as_str).unwrap_or("");
            let cell = gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .yalign(0.0)
                .wrap(true)
                .wrap_mode(pango::WrapMode::WordChar)
                .max_width_chars(28)
                .hexpand(true)
                .selectable(true)
                .build();
            cell.add_css_class("note-cell");
            if r + 1 == rows.len() {
                cell.add_css_class("last-row");
            }
            if c + 1 == ncols {
                cell.add_css_class("last-col");
            }
            grid.attach(&cell, c as i32, r as i32, 1, 1);
        }
    }
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("note-table");
    frame.set_overflow(gtk::Overflow::Hidden);
    frame.append(&grid);
    frame.set_halign(gtk::Align::Start);
    frame.upcast()
}

/// Builds the widgets for a note body.
pub fn build(note: &Note) -> Rendered {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    root.add_css_class("note-body");
    let mut images = Vec::new();
    let mut files = Vec::new();
    for seg in segments(&note.blocks, &note.summary.title) {
        match seg {
            Segment::Text(paras) => {
                for p in &paras {
                    root.append(&para_widget(p));
                }
            }
            Segment::Code(paras) => {
                let text: Vec<String> = paras.iter().map(|p| markup(&p.runs, false)).collect();
                let label = text_label(&text.join("\n"), "note-code-text");
                label.add_css_class("monospace");
                label.set_wrap_mode(pango::WrapMode::Char);
                let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
                b.add_css_class("note-code");
                b.append(&label);
                root.append(&b);
            }
            Segment::Quote(paras) => {
                let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
                b.add_css_class("note-quote");
                for p in &paras {
                    b.append(&para_widget(p));
                }
                root.append(&b);
            }
            Segment::Image(att) => {
                let slot = image_slot();
                slot.add_css_class("note-block");
                root.append(&slot);
                images.push((att, slot));
            }
            Segment::File(att) => {
                let openable = att.url.is_some() || link_href(&att).is_some();
                let (w, button) = file_card(&att, openable);
                w.add_css_class("note-block");
                root.append(&w);
                if let Some(b) = button {
                    files.push((att, b));
                }
            }
            Segment::Table(rows) => {
                let t = table(&rows);
                t.add_css_class("note-block");
                root.append(&t);
            }
        }
    }
    Rendered {
        root,
        images,
        files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(text: &str) -> Vec<Run> {
        vec![Run {
            text: text.into(),
            ..Default::default()
        }]
    }

    fn kinds(segs: &[Segment]) -> Vec<&'static str> {
        segs.iter()
            .map(|s| match s {
                Segment::Text(_) => "text",
                Segment::Code(p) if p.len() == 2 => "code2",
                Segment::Code(_) => "code",
                Segment::Quote(_) => "quote",
                Segment::Image(_) => "image",
                Segment::File(_) => "file",
                Segment::Table(_) => "table",
            })
            .collect()
    }

    fn att(id: &str, kind: &str, url: Option<&str>) -> Block {
        Block::Attachment {
            attachment: Attachment {
                id: id.into(),
                kind: kind.into(),
                url: url.map(Into::into),
                ..Default::default()
            },
        }
    }

    #[test]
    fn link_cards_show_title_and_address() {
        let card = Attachment {
            id: "l".into(),
            kind: "link".into(),
            href: Some("https://example.com/rivers".into()),
            title: Some("The Shape of Rivers".into()),
            ..Default::default()
        };
        assert_eq!(card_title(&card), "The Shape of Rivers");
        assert_eq!(card_subtitle(&card), "https://example.com/rivers");
        let bare = Attachment {
            title: None,
            ..card.clone()
        };
        assert_eq!(card_title(&bare), "https://example.com/rivers");
        assert_eq!(card_subtitle(&bare), "Link");
        let file = Attachment {
            kind: "file".into(),
            ..card
        };
        assert_eq!(link_href(&file), None);
    }

    #[test]
    fn groups_blocks_into_segments() {
        let blocks = vec![
            Block::Title { runs: r("T") },
            Block::Body { runs: r("b") },
            Block::Monospace { runs: r("x = 1") },
            Block::Monospace { runs: r("y = 2") },
            Block::Quote { runs: r("q") },
            Block::Table {
                rows: vec![vec!["a".into()]],
            },
            att("1", "image", Some("/u")),
            // An image without a URL can't be fetched, so it shows as a card.
            att("2", "image", None),
            att("3", "drawing", Some("/d")),
            att("4", "pdf", Some("/p")),
            Block::Unknown,
            Block::Body { runs: r("after") },
        ];
        let segs = segments(&blocks, "T");
        assert_eq!(
            kinds(&segs),
            vec![
                "text", "code2", "quote", "table", "image", "file", "image", "file", "text"
            ]
        );
        let Segment::Text(first) = &segs[0] else {
            panic!()
        };
        assert_eq!(
            first.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![ParaKind::Title, ParaKind::Body]
        );
    }

    #[test]
    fn adds_title_when_missing() {
        let segs = segments(&[Block::Body { runs: r("hello") }], "My note");
        let Segment::Text(paras) = &segs[0] else {
            panic!()
        };
        assert_eq!(paras[0].kind, ParaKind::Title);
        assert_eq!(plain(&paras[0].runs), "My note");
        assert_eq!(paras.len(), 2);
    }

    #[test]
    fn list_markers_and_indents() {
        let blocks = vec![
            Block::Bullet {
                indent: 0,
                runs: r("boots"),
            },
            Block::Dash {
                indent: 1,
                runs: r("socks"),
            },
            Block::Checklist {
                indent: 0,
                checked: true,
                runs: r("passports"),
            },
        ];
        let segs = segments(&blocks, "");
        let Segment::Text(paras) = &segs[0] else {
            panic!()
        };
        assert_eq!(
            paras[0].marker.as_ref().and_then(marker_text).as_deref(),
            Some("•")
        );
        assert_eq!(paras[1].kind, ParaKind::List(1));
        assert_eq!(
            paras[1].marker.as_ref().and_then(marker_text).as_deref(),
            Some("–")
        );
        assert_eq!(paras[2].marker, Some(Marker::Check(true)));
        assert_eq!(marker_text(&Marker::Check(true)), None);
    }

    #[test]
    fn numbering_counts_restarts_and_respects_explicit_numbers() {
        let blocks = vec![
            Block::Numbered {
                indent: 0,
                number: 0,
                runs: r("a"),
            },
            Block::Numbered {
                indent: 1,
                number: 0,
                runs: r("a.i"),
            },
            Block::Numbered {
                indent: 0,
                number: 0,
                runs: r("b"),
            },
            Block::Body { runs: r("break") },
            Block::Numbered {
                indent: 0,
                number: 0,
                runs: r("c"),
            },
            Block::Numbered {
                indent: 0,
                number: 7,
                runs: r("d"),
            },
            Block::Numbered {
                indent: 0,
                number: 0,
                runs: r("e"),
            },
        ];
        let segs = segments(&blocks, "");
        let Segment::Text(paras) = &segs[0] else {
            panic!()
        };
        let markers: Vec<_> = paras
            .iter()
            .map(|p| p.marker.as_ref().and_then(marker_text))
            .collect();
        let want = [
            Some("1."),
            Some("1."),
            Some("2."),
            None,
            Some("1."),
            Some("7."),
            Some("8."),
        ];
        assert_eq!(markers, want.map(|m| m.map(String::from)));
    }

    #[test]
    fn markup_escapes_and_nests_styles() {
        let runs = vec![
            Run {
                text: "Fish & <chips> ".into(),
                ..Default::default()
            },
            Run {
                text: "now".into(),
                bold: true,
                italic: true,
                ..Default::default()
            },
            Run {
                text: "".into(),
                bold: true,
                ..Default::default()
            },
            Run {
                text: " map".into(),
                link: Some("https://x.y/?a=1&b=\"2\"".into()),
                underline: true,
                ..Default::default()
            },
            Run {
                text: "old".into(),
                strikethrough: true,
                ..Default::default()
            },
        ];
        assert_eq!(
            markup(&runs, false),
            "Fish &amp; &lt;chips&gt; <b><i>now</i></b><a href=\"https://x.y/?a=1&amp;b=&quot;2&quot;\"><u> map</u></a><s>old</s>"
        );
    }

    #[test]
    fn checked_items_are_struck_and_dimmed() {
        assert_eq!(
            markup(&r("Passports"), true),
            "<span strikethrough=\"true\" fgalpha=\"55%\">Passports</span>"
        );
        assert_eq!(markup(&r(""), true), "");
        assert_eq!(markup(&r("Chargers"), false), "Chargers");
    }

    #[test]
    fn markup_is_valid_pango() {
        let runs = vec![
            Run {
                text: "a < b".into(),
                bold: true,
                underline: true,
                strikethrough: true,
                ..Default::default()
            },
            Run {
                text: "link".into(),
                link: Some("https://e.com/a b".into()),
                italic: true,
                ..Default::default()
            },
        ];
        // Links are a GtkLabel extension, so check the rest with Pango's parser.
        let m = markup(&runs, true)
            .replace("<a href=\"https://e.com/a b\">", "")
            .replace("</a>", "");
        assert!(pango::parse_markup(&m, '\0').is_ok(), "{m}");
    }
}
