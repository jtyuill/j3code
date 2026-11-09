//! Assistant markdown as flat blocks for SwiftUI (`docs/m8.md` D2b).
//!
//! The math goes as on the desktop: [`math::prepare`] turns each display
//! formula that is a paragraph by itself into an image block, and
//! [`inline::split`] cuts out the top-level paragraphs that hold inline
//! formulas, which come back as [`MdInline::Math`] runs. Elsewhere (lists,
//! quotes, tables) a formula reads as its Unicode approximation.
//!
//! Nesting is flattened: a block carries its list depth, its quote depth and,
//! for the first block of a list item, the item's marker. Swift lays the
//! blocks out in a `VStack` without recursion.

use j3_text::math::{self, Formula, inline};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// A message's markdown, parsed.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct Markdown {
    pub blocks: Vec<MdBlock>,
    /// Every formula the blocks name, by index.
    pub formulas: Vec<Formula>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MdBlock {
    /// Depth of the lists the block is in; 0 outside lists.
    pub indent: u32,
    /// Depth of the block quotes the block is in.
    pub quote: u32,
    /// The list item's marker, on the item's first block only.
    pub marker: Option<ListMarker>,
    pub kind: MdBlockKind,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum ListMarker {
    Bullet,
    Number { number: u64 },
    Task { checked: bool },
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum MdBlockKind {
    Paragraph {
        inlines: Vec<MdInline>,
    },
    Heading {
        level: u8,
        inlines: Vec<MdInline>,
    },
    Code {
        /// The fence's first word.
        language: Option<String>,
        code: String,
    },
    /// A display formula: `Markdown::formulas[formula]`.
    Math {
        formula: u32,
    },
    /// A paragraph that is one image.
    Image {
        url: String,
        alt: String,
    },
    Table {
        alignments: Vec<MdAlignment>,
        header: Vec<MdCell>,
        rows: Vec<MdRow>,
    },
    Rule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MdAlignment {
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MdCell {
    pub inlines: Vec<MdInline>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MdRow {
    pub cells: Vec<MdCell>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum MdInline {
    Text {
        text: String,
        bold: bool,
        italic: bool,
        strike: bool,
        /// A code span.
        code: bool,
        link: Option<String>,
    },
    /// An inline formula: `Markdown::formulas[formula]`.
    Math {
        formula: u32,
    },
    Image {
        url: String,
        alt: String,
    },
    /// A hard line break.
    Break,
}

/// Parses `markdown` (an assistant message) into blocks.
#[uniffi::export]
pub fn parse_markdown(markdown: String) -> Markdown {
    let prepared = math::prepare(&markdown);
    let mut builder = Builder {
        formulas: prepared.formulas,
        ..Builder::default()
    };
    for part in inline::split(&prepared.source) {
        match part {
            inline::Part::Markdown(source) => builder.parse(&source, None),
            inline::Part::Paragraph { source, formulas } => {
                let offset = builder.formulas.len() as u32;
                builder.formulas.extend(formulas);
                builder.parse(&source, Some(offset));
            }
        }
    }
    Markdown {
        blocks: builder.blocks,
        formulas: builder.formulas,
    }
}

/// What the inline runs being collected belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Paragraph,
    /// A tight list item's text, which has no paragraph tags.
    Implicit,
    Heading(u8),
    Cell,
}

#[derive(Default)]
struct Table {
    alignments: Vec<MdAlignment>,
    header: Vec<MdCell>,
    rows: Vec<MdRow>,
    row: Vec<MdCell>,
    in_head: bool,
}

#[derive(Default)]
struct Builder {
    blocks: Vec<MdBlock>,
    formulas: Vec<Formula>,
    /// The next number of each open list; `None` for bullets.
    lists: Vec<Option<u64>>,
    quote: u32,
    marker: Option<ListMarker>,
    target: Option<Target>,
    runs: Vec<MdInline>,
    text: String,
    bold: u32,
    italic: u32,
    strike: u32,
    links: Vec<String>,
    /// Inside an image: its url and the alt text so far.
    image: Option<(String, String)>,
    code: Option<(Option<String>, String)>,
    table: Option<Table>,
    /// The formula index placeholder `0` stands for, in a paragraph part.
    placeholders: Option<u32>,
}

impl Builder {
    fn parse(&mut self, source: &str, placeholders: Option<u32>) {
        self.placeholders = placeholders;
        let options =
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        for event in Parser::new_ext(source, options) {
            self.event(event);
        }
        self.close_inline();
        self.placeholders = None;
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => {
                if let Some((_, alt)) = &mut self.image {
                    alt.push_str(&code);
                    return;
                }
                self.open_implicit();
                self.flush();
                self.runs.push(MdInline::Text {
                    text: code.into_string(),
                    bold: self.bold > 0,
                    italic: self.italic > 0,
                    strike: self.strike > 0,
                    code: true,
                    link: self.links.last().cloned(),
                });
            }
            Event::InlineMath(tex) | Event::DisplayMath(tex) => self.text(&tex),
            Event::Html(html) | Event::InlineHtml(html) => {
                if let Some((_, code)) = &mut self.code {
                    code.push_str(&html);
                } else {
                    self.text(&html);
                }
            }
            Event::FootnoteReference(name) => self.text(&format!("[^{name}]")),
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => {
                self.open_implicit();
                self.flush();
                self.runs.push(MdInline::Break);
            }
            Event::Rule => {
                self.close_inline();
                self.push(MdBlockKind::Rule);
            }
            Event::TaskListMarker(checked) => {
                if self.marker.is_some() {
                    self.marker = Some(ListMarker::Task { checked });
                }
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.close_inline();
                self.target = Some(Target::Paragraph);
            }
            Tag::Heading { level, .. } => {
                self.close_inline();
                self.target = Some(Target::Heading(level as u8));
            }
            Tag::BlockQuote(_) => {
                self.close_inline();
                self.quote += 1;
            }
            Tag::CodeBlock(kind) => {
                self.close_inline();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().map(|word| word.to_owned())
                    }
                    CodeBlockKind::Indented => None,
                };
                self.code = Some((language, String::new()));
            }
            Tag::HtmlBlock => {
                self.close_inline();
                self.code = Some((Some("html".into()), String::new()));
            }
            Tag::List(start) => {
                self.close_inline();
                self.lists.push(start);
            }
            Tag::Item => {
                self.close_inline();
                let marker = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = ListMarker::Number { number: *number };
                        *number += 1;
                        marker
                    }
                    _ => ListMarker::Bullet,
                };
                self.marker = Some(marker);
            }
            Tag::Table(alignments) => {
                self.close_inline();
                self.table = Some(Table {
                    alignments: alignments.into_iter().map(alignment).collect(),
                    ..Table::default()
                });
            }
            Tag::TableHead => {
                if let Some(table) = &mut self.table {
                    table.in_head = true;
                }
            }
            Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.row.clear();
                }
            }
            Tag::TableCell => {
                self.runs.clear();
                self.text.clear();
                self.target = Some(Target::Cell);
            }
            Tag::Emphasis => {
                self.flush();
                self.italic += 1;
            }
            Tag::Strong => {
                self.flush();
                self.bold += 1;
            }
            Tag::Strikethrough => {
                self.flush();
                self.strike += 1;
            }
            Tag::Link { dest_url, .. } => {
                self.open_implicit();
                self.flush();
                self.links.push(dest_url.into_string());
            }
            Tag::Image { dest_url, .. } => {
                self.open_implicit();
                self.flush();
                self.image = Some((dest_url.into_string(), String::new()));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.close_inline(),
            TagEnd::BlockQuote(_) => {
                self.close_inline();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::CodeBlock | TagEnd::HtmlBlock => {
                if let Some((language, mut code)) = self.code.take() {
                    if code.ends_with('\n') {
                        code.pop();
                    }
                    self.push(MdBlockKind::Code { language, code });
                }
            }
            TagEnd::List(_) => {
                self.close_inline();
                self.lists.pop();
            }
            TagEnd::Item => {
                self.close_inline();
                // An empty item still shows its marker.
                if self.marker.is_some() {
                    self.push(MdBlockKind::Paragraph {
                        inlines: Vec::new(),
                    });
                }
            }
            TagEnd::TableCell => {
                self.flush();
                let cell = MdCell {
                    inlines: std::mem::take(&mut self.runs),
                };
                self.target = None;
                if let Some(table) = &mut self.table {
                    table.row.push(cell);
                }
            }
            TagEnd::TableHead => {
                if let Some(table) = &mut self.table {
                    table.header = std::mem::take(&mut table.row);
                    table.in_head = false;
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = &mut self.table
                    && !table.in_head
                {
                    let cells = std::mem::take(&mut table.row);
                    table.rows.push(MdRow { cells });
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.push(MdBlockKind::Table {
                        alignments: table.alignments,
                        header: table.header,
                        rows: table.rows,
                    });
                }
            }
            TagEnd::Emphasis => {
                self.flush();
                self.italic = self.italic.saturating_sub(1);
            }
            TagEnd::Strong => {
                self.flush();
                self.bold = self.bold.saturating_sub(1);
            }
            TagEnd::Strikethrough => {
                self.flush();
                self.strike = self.strike.saturating_sub(1);
            }
            TagEnd::Link => {
                self.flush();
                self.links.pop();
            }
            TagEnd::Image => {
                if let Some((url, alt)) = self.image.take() {
                    self.runs.push(MdInline::Image { url, alt });
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if let Some((_, code)) = &mut self.code {
            code.push_str(text);
            return;
        }
        if let Some((_, alt)) = &mut self.image {
            alt.push_str(text);
            return;
        }
        self.open_implicit();
        self.text.push_str(text);
    }

    /// Inline content outside a paragraph (a tight list item's text) opens
    /// a paragraph of its own.
    fn open_implicit(&mut self) {
        if self.target.is_none() {
            self.target = Some(Target::Implicit);
        }
    }

    /// Ends the pending text as runs: plain text, and a math run for each
    /// formula placeholder in a paragraph part.
    fn flush(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.text);
        let mut plain = String::new();
        for c in text.chars() {
            match self
                .placeholders
                .zip(inline::placeholder_index(c))
                .map(|(offset, index)| offset + index as u32)
                .filter(|&index| (index as usize) < self.formulas.len())
            {
                Some(formula) => {
                    self.push_text(std::mem::take(&mut plain));
                    self.runs.push(MdInline::Math { formula });
                }
                None => plain.push(c),
            }
        }
        self.push_text(plain);
    }

    fn push_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.runs.push(MdInline::Text {
            text,
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            code: false,
            link: self.links.last().cloned(),
        });
    }

    /// Ends the paragraph or heading being collected, if any.
    fn close_inline(&mut self) {
        self.flush();
        let Some(target) = self.target else {
            return;
        };
        if target == Target::Cell {
            return;
        }
        self.target = None;
        let inlines = std::mem::take(&mut self.runs);
        let kind = match target {
            Target::Heading(level) => MdBlockKind::Heading { level, inlines },
            _ => match inlines.as_slice() {
                [MdInline::Image { url, alt }] => match math::parse_src(url) {
                    Some(formula) if formula < self.formulas.len() => MdBlockKind::Math {
                        formula: formula as u32,
                    },
                    _ => MdBlockKind::Image {
                        url: url.clone(),
                        alt: alt.clone(),
                    },
                },
                [] => return,
                _ => MdBlockKind::Paragraph { inlines },
            },
        };
        self.push(kind);
    }

    fn push(&mut self, kind: MdBlockKind) {
        self.blocks.push(MdBlock {
            indent: self.lists.len() as u32,
            quote: self.quote,
            marker: self.marker.take(),
            kind,
        });
    }
}

fn alignment(alignment: Alignment) -> MdAlignment {
    match alignment {
        Alignment::None => MdAlignment::None,
        Alignment::Left => MdAlignment::Left,
        Alignment::Center => MdAlignment::Center,
        Alignment::Right => MdAlignment::Right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(run: &MdInline) -> &str {
        match run {
            MdInline::Text { text, .. } => text,
            _ => "",
        }
    }

    #[test]
    fn blocks_carry_lists_quotes_and_styles() {
        let doc = parse_markdown(
            "# Title\n\nSome **bold** and `code` with [a link](https://x.y).\n\n\
             - one\n- two\n  1. nested\n- [x] done\n\n> quoted\n\n```rust\nfn main() {}\n```\n\n---\n"
                .into(),
        );
        let kinds: Vec<_> = doc.blocks.iter().map(|b| &b.kind).collect();
        assert!(matches!(kinds[0], MdBlockKind::Heading { level: 1, .. }));
        let MdBlockKind::Paragraph { inlines } = kinds[1] else {
            panic!("{kinds:?}")
        };
        assert!(
            inlines.iter().any(
                |run| matches!(run, MdInline::Text { text, bold: true, .. } if text == "bold")
            )
        );
        assert!(
            inlines.iter().any(
                |run| matches!(run, MdInline::Text { text, code: true, .. } if text == "code")
            )
        );
        assert!(inlines.iter().any(
            |run| matches!(run, MdInline::Text { link: Some(link), .. } if link == "https://x.y")
        ));
        let list: Vec<_> = doc.blocks[2..6]
            .iter()
            .map(|b| (b.indent, b.marker.clone()))
            .collect();
        assert_eq!(
            list,
            [
                (1, Some(ListMarker::Bullet)),
                (1, Some(ListMarker::Bullet)),
                (2, Some(ListMarker::Number { number: 1 })),
                (1, Some(ListMarker::Task { checked: true })),
            ]
        );
        assert_eq!(doc.blocks[6].quote, 1);
        assert!(matches!(&doc.blocks[7].kind,
            MdBlockKind::Code { language: Some(lang), code } if lang == "rust" && code == "fn main() {}"));
        assert!(matches!(doc.blocks[8].kind, MdBlockKind::Rule));
    }

    #[test]
    fn math_becomes_formula_runs_and_blocks() {
        let doc = parse_markdown("Area $\\pi r^2$ here.\n\n$$\\int x\\,dx$$\n".into());
        let MdBlockKind::Paragraph { inlines } = &doc.blocks[0].kind else {
            panic!("{doc:?}")
        };
        assert_eq!(text(&inlines[0]), "Area ");
        let MdInline::Math { formula } = inlines[1] else {
            panic!("{inlines:?}")
        };
        assert_eq!(doc.formulas[formula as usize].tex, "\\pi r^2");
        assert_eq!(text(&inlines[2]), " here.");
        let MdBlockKind::Math { formula } = doc.blocks[1].kind else {
            panic!("{doc:?}")
        };
        assert!(doc.formulas[formula as usize].display);
    }

    #[test]
    fn tables_keep_header_and_rows() {
        let doc = parse_markdown("| a | b |\n|:--|--:|\n| 1 | 2 |\n| 3 | 4 |\n".into());
        let MdBlockKind::Table {
            alignments,
            header,
            rows,
        } = &doc.blocks[0].kind
        else {
            panic!("{doc:?}")
        };
        assert_eq!(alignments, &[MdAlignment::Left, MdAlignment::Right]);
        assert_eq!(text(&header[1].inlines[0]), "b");
        assert_eq!(rows.len(), 2);
        assert_eq!(text(&rows[1].cells[0].inlines[0]), "3");
    }
}
