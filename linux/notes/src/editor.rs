//! The plain-note editor: a text view over the bridge's Markdown edit format with light
//! highlighting (headings sized, markers dimmed, bold/italic/strike/links styled, fences mono).

use gtk::prelude::*;
use gtk::{gdk, pango};

/// Colours that text tags need explicitly (tags can't follow CSS).
#[derive(Clone)]
pub struct Palette {
    pub link: gdk::RGBA,
    pub dim: gdk::RGBA,
}

impl Palette {
    pub fn current(widget: &impl IsA<gtk::Widget>) -> Palette {
        let sm = adw::StyleManager::default();
        let fg = widget.as_ref().color();
        Palette {
            link: sm.accent_color().to_standalone_rgba(sm.is_dark()),
            dim: gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.5),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Md {
    H1,
    H2,
    H3,
    Marker,
    Bold,
    Italic,
    Strike,
    Link,
    Code,
    Quote,
}

impl Md {
    const ALL: [Md; 10] = [
        Md::H1,
        Md::H2,
        Md::H3,
        Md::Marker,
        Md::Bold,
        Md::Italic,
        Md::Strike,
        Md::Link,
        Md::Code,
        Md::Quote,
    ];

    fn name(self) -> &'static str {
        match self {
            Md::H1 => "md-h1",
            Md::H2 => "md-h2",
            Md::H3 => "md-h3",
            Md::Marker => "md-marker",
            Md::Bold => "md-bold",
            Md::Italic => "md-italic",
            Md::Strike => "md-strike",
            Md::Link => "md-link",
            Md::Code => "md-code",
            Md::Quote => "md-quote",
        }
    }
}

/// Highlight spans as (start, end, kind) in character offsets.
pub fn md_spans(text: &str) -> Vec<(usize, usize, Md)> {
    let mut out = Vec::new();
    let mut offset = 0;
    let mut in_code = false;
    for line in text.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        let n = chars.len();
        let start = offset;
        offset += n + 1;
        if line.trim_start().starts_with("```") {
            out.push((start, start + n, Md::Code));
            out.push((start, start + n, Md::Marker));
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push((start, start + n, Md::Code));
            continue;
        }
        let lead = chars.iter().take_while(|c| **c == ' ').count();
        let rest: String = chars[lead..].iter().collect();
        let mut body_from = lead;
        let heading = [("### ", Md::H3), ("## ", Md::H2), ("# ", Md::H1)]
            .into_iter()
            .find(|(p, _)| lead == 0 && rest.starts_with(p));
        if let Some((p, kind)) = heading {
            out.push((start, start + n, kind));
            out.push((start, start + p.len(), Md::Marker));
            body_from = p.len();
        } else if rest.starts_with("> ") {
            out.push((start + lead, start + lead + 2, Md::Marker));
            out.push((start + lead + 2, start + n, Md::Quote));
            body_from = lead + 2;
        } else if rest.starts_with("- ") || rest.starts_with("* ") {
            out.push((start + lead, start + lead + 2, Md::Marker));
            body_from = lead + 2;
        } else {
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            if digits > 0 && rest[digits..].starts_with(". ") {
                out.push((start + lead, start + lead + digits + 2, Md::Marker));
                body_from = lead + digits + 2;
            }
        }
        inline_spans(&chars, body_from, start, &mut out);
    }
    out
}

fn find(chars: &[char], from: usize, pat: &[char]) -> Option<usize> {
    (from..chars.len().saturating_sub(pat.len() - 1)).find(|&i| chars[i..].starts_with(pat))
}

fn inline_spans(chars: &[char], from: usize, base: usize, out: &mut Vec<(usize, usize, Md)>) {
    let mut i = from;
    while i < chars.len() {
        let pair = |d: &[char], kind: Md, out: &mut Vec<(usize, usize, Md)>| -> Option<usize> {
            let close = find(chars, i + d.len() + 1, d)?;
            out.push((base + i, base + i + d.len(), Md::Marker));
            out.push((base + i + d.len(), base + close, kind));
            out.push((base + close, base + close + d.len(), Md::Marker));
            Some(close + d.len())
        };
        let next = if chars[i..].starts_with(&['*', '*']) {
            pair(&['*', '*'], Md::Bold, out)
        } else if chars[i..].starts_with(&['~', '~']) {
            pair(&['~', '~'], Md::Strike, out)
        } else if chars[i] == '*' && chars.get(i + 1).is_some_and(|c| !c.is_whitespace()) {
            pair(&['*'], Md::Italic, out)
        } else if chars[i] == '[' {
            find(chars, i + 1, &[']', '(']).and_then(|mid| {
                let close = find(chars, mid + 2, &[')'])?;
                out.push((base + i, base + i + 1, Md::Marker));
                out.push((base + i + 1, base + mid, Md::Link));
                out.push((base + mid, base + close + 1, Md::Marker));
                Some(close + 1)
            })
        } else {
            None
        };
        i = next.unwrap_or(i + 1);
    }
}

pub struct Editor {
    pub scroller: gtk::ScrolledWindow,
    pub view: gtk::TextView,
}

impl Editor {
    pub fn new() -> Editor {
        let view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(18)
            .bottom_margin(48)
            .left_margin(4)
            .right_margin(4)
            .pixels_below_lines(2)
            .accepts_tab(false)
            .hexpand(true)
            .vexpand(true)
            .build();
        view.add_css_class("note-editor");
        view.update_property(&[gtk::accessible::Property::Label("Note text")]);
        let clamp = adw::Clamp::builder()
            .maximum_size(720)
            .tightening_threshold(560)
            .child(&view)
            .build();
        clamp.set_margin_start(18);
        clamp.set_margin_end(18);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .build();
        scroller.add_css_class("view");

        let buffer = view.buffer();
        let table = buffer.tag_table();
        for kind in Md::ALL {
            let b = gtk::TextTag::builder().name(kind.name());
            let tag = match kind {
                Md::H1 => b.scale(1.6).weight(800),
                Md::H2 => b.scale(1.3).weight(700),
                Md::H3 => b.scale(1.12).weight(700),
                Md::Marker => b,
                Md::Bold => b.weight(700),
                Md::Italic => b.style(pango::Style::Italic),
                Md::Strike => b.strikethrough(true),
                Md::Link => b.underline(pango::Underline::Single),
                Md::Code => b.family("monospace"),
                Md::Quote => b.style(pango::Style::Italic),
            }
            .build();
            table.add(&tag);
        }
        buffer.connect_changed(highlight);
        Editor { scroller, view }
    }

    pub fn update_palette(&self, pal: &Palette) {
        let table = self.view.buffer().tag_table();
        if let Some(t) = table.lookup(Md::Marker.name()) {
            t.set_foreground_rgba(Some(&pal.dim));
        }
        if let Some(t) = table.lookup(Md::Link.name()) {
            t.set_foreground_rgba(Some(&pal.link));
        }
        if let Some(t) = table.lookup(Md::Code.name()) {
            let d = pal.dim;
            t.set_paragraph_background_rgba(Some(&gdk::RGBA::new(
                d.red(),
                d.green(),
                d.blue(),
                0.08,
            )));
        }
    }

    pub fn text(&self) -> String {
        let b = self.view.buffer();
        b.text(&b.start_iter(), &b.end_iter(), false).to_string()
    }

    pub fn set_text(&self, text: &str) {
        let b = self.view.buffer();
        b.set_text(text);
        b.place_cursor(&b.end_iter());
    }
}

fn highlight(buffer: &gtk::TextBuffer) {
    let (s, e) = buffer.bounds();
    buffer.remove_all_tags(&s, &e);
    let text = buffer.text(&s, &e, false);
    let table = buffer.tag_table();
    for (a, b, kind) in md_spans(&text) {
        if let Some(tag) = table.lookup(kind.name()) {
            buffer.apply_tag(
                &tag,
                &buffer.iter_at_offset(a as i32),
                &buffer.iter_at_offset(b as i32),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(text: &str, kind: Md) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        md_spans(text)
            .into_iter()
            .filter(|s| s.2 == kind)
            .map(|(a, b, _)| chars[a..b].iter().collect())
            .collect()
    }

    #[test]
    fn headings_and_markers() {
        let t = "# Title\n\n## Sub\n- item\n  * deep\n12. twelve\n> quoted";
        assert_eq!(pick(t, Md::H1), vec!["# Title"]);
        assert_eq!(pick(t, Md::H2), vec!["## Sub"]);
        assert_eq!(
            pick(t, Md::Marker),
            vec!["# ", "## ", "- ", "* ", "12. ", "> "]
        );
        assert_eq!(pick(t, Md::Quote), vec!["quoted"]);
    }

    #[test]
    fn inline_styles() {
        let t = "a **bold** and *it* and ~~gone~~ [site](https://x.y) é*ok*";
        assert_eq!(pick(t, Md::Bold), vec!["bold"]);
        assert_eq!(pick(t, Md::Italic), vec!["it", "ok"]);
        assert_eq!(pick(t, Md::Strike), vec!["gone"]);
        assert_eq!(pick(t, Md::Link), vec!["site"]);
        assert!(pick(t, Md::Marker).contains(&"](https://x.y)".to_string()));
    }

    #[test]
    fn fenced_code_is_not_parsed_inline() {
        let t = "```\n**not bold**\n```\nafter **bold**";
        assert_eq!(pick(t, Md::Code), vec!["```", "**not bold**", "```"]);
        assert_eq!(pick(t, Md::Bold), vec!["bold"]);
    }

    #[test]
    fn unclosed_markers_are_ignored() {
        assert!(pick("a ** b", Md::Bold).is_empty());
        assert!(pick("[x](", Md::Link).is_empty());
        assert!(pick("2 * 3", Md::Italic).is_empty());
    }
}
