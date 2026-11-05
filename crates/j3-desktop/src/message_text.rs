//! Presentation of a sent user message: omp's `^model` mentions collapse to
//! chips and magic keywords are marked for the gradient, the way the TUI
//! shows them. Pure text work; the feed view turns the spans into styles.

use std::ops::Range;

use crate::{keywords, mention};

/// Glyph in front of a model chip: omp's default `icon.model`.
const MODEL_ICON: &str = "\u{2B22} ";

#[derive(Clone, Copy)]
pub enum SpanKind {
    Keyword(&'static keywords::Keyword),
    Model,
}

/// A styled stretch of [`Rich::text`].
#[derive(Clone)]
pub struct Span {
    /// Bytes of [`Rich::text`], on char boundaries.
    pub range: Range<usize>,
    pub kind: SpanKind,
}

/// The displayed text of a message with its styled spans.
#[derive(Clone)]
pub struct Rich {
    pub text: String,
    /// Sorted by start and never overlapping.
    pub spans: Vec<Span>,
}

/// One mention replaced by its chip text.
struct Replacement {
    /// Bytes of the original text.
    original: Range<usize>,
    /// Bytes of the output text.
    output: Range<usize>,
}

/// The displayed form of `text`. `model_name` maps a `^selector` to the
/// display name of the model it names; selectors it does not know stay
/// literal, as omp leaves them.
///
/// Keywords are found on the original text so omp's masking of code and HTML
/// applies, then mapped through the replacements. A keyword inside a
/// replaced mention is dropped.
pub fn rich(text: &str, model_name: impl Fn(&str) -> Option<String>) -> Rich {
    let mut output = String::new();
    let mut spans = Vec::new();
    let mut replacements: Vec<Replacement> = Vec::new();
    let mut copied = 0;

    if mention::allowed(text) {
        for found in mention::mentions(text) {
            let Some(display) = model_name(found.selector) else {
                continue;
            };
            output.push_str(&text[copied..found.range.start]);
            let start = output.len();
            output.push_str(MODEL_ICON);
            output.push_str(&display);
            let range = start..output.len();
            spans.push(Span {
                range: range.clone(),
                kind: SpanKind::Model,
            });
            replacements.push(Replacement {
                original: found.range.clone(),
                output: range,
            });
            copied = found.range.end;
        }
    }
    output.push_str(&text[copied..]);

    let found = keywords::find(text);
    if replacements.is_empty() && found.is_empty() {
        return Rich {
            text: output,
            spans: Vec::new(),
        };
    }

    for hit in found {
        if replacements
            .iter()
            .any(|r| hit.range.start < r.original.end && r.original.start < hit.range.end)
        {
            continue;
        }
        spans.push(Span {
            range: shift(&replacements, hit.range.start)..shift(&replacements, hit.range.end),
            kind: SpanKind::Keyword(hit.keyword),
        });
    }
    spans.sort_by_key(|span| span.range.start);
    Rich {
        text: output,
        spans,
    }
}

/// The output offset of original offset `at`, which lies outside every
/// replacement.
fn shift(replacements: &[Replacement], at: usize) -> usize {
    let mut shifted = at;
    for replacement in replacements {
        if replacement.original.end > at {
            break;
        }
        shifted = shifted + replacement.output.len() - replacement.original.len();
    }
    shifted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(selector: &str) -> Option<String> {
        match selector {
            "anthropic/claude" => Some("Claude".to_owned()),
            "a/b" => Some("Model B".to_owned()),
            _ => None,
        }
    }

    fn keyword_ids(rich: &Rich) -> Vec<(&str, &'static str)> {
        rich.spans
            .iter()
            .map(|span| match span.kind {
                SpanKind::Keyword(keyword) => (&rich.text[span.range.clone()], keyword.word),
                SpanKind::Model => (&rich.text[span.range.clone()], "model"),
            })
            .collect()
    }

    #[test]
    fn plain_text_is_unchanged() {
        let rich = rich("just a normal message", names);
        assert_eq!(rich.text, "just a normal message");
        assert!(rich.spans.is_empty());
    }

    #[test]
    fn known_mention_becomes_chip_and_shifts_later_text() {
        let rich = rich("use ^anthropic/claude now", names);
        assert_eq!(rich.text, "use \u{2B22} Claude now");
        assert_eq!(rich.spans.len(), 1);
        assert!(matches!(rich.spans[0].kind, SpanKind::Model));
        assert_eq!(&rich.text[rich.spans[0].range.clone()], "\u{2B22} Claude");
        assert_eq!(&rich.text[rich.spans[0].range.end..], " now");
    }

    #[test]
    fn unknown_mention_stays_literal() {
        let rich = rich("ask ^nobody/home please", names);
        assert_eq!(rich.text, "ask ^nobody/home please");
        assert!(rich.spans.is_empty());
    }

    #[test]
    fn commands_never_expand_mentions() {
        let rich = rich("!cmd ^a/b", names);
        assert_eq!(rich.text, "!cmd ^a/b");
        assert!(rich.spans.is_empty());
    }

    #[test]
    fn keyword_after_multibyte_prefix() {
        let text = "h\u{e9}llo \u{1F600} ultrathink";
        let rich = rich(text, names);
        assert_eq!(rich.text, text);
        assert_eq!(rich.spans.len(), 1);
        let start = text.find("ultrathink").unwrap();
        assert_eq!(rich.spans[0].range, start..start + "ultrathink".len());
    }

    #[test]
    fn keyword_after_replaced_mention_is_shifted() {
        let rich = rich("^a/b caf\u{e9} ultrathink", names);
        assert_eq!(rich.text, "\u{2B22} Model B caf\u{e9} ultrathink");
        assert_eq!(
            keyword_ids(&rich),
            vec![("\u{2B22} Model B", "model"), ("ultrathink", "ultrathink"),]
        );
        assert_eq!(rich.spans[1].range.end, rich.text.len());
    }

    #[test]
    fn keyword_in_backticks_has_no_span() {
        let rich = rich("run `ultrathink` please", names);
        assert_eq!(rich.text, "run `ultrathink` please");
        assert!(rich.spans.is_empty());
    }

    #[test]
    fn two_keywords_and_a_mention_stay_ordered() {
        let rich = rich("ultrathink with ^a/b then orchestrate", names);
        assert_eq!(
            rich.text,
            "ultrathink with \u{2B22} Model B then orchestrate"
        );
        assert_eq!(
            keyword_ids(&rich),
            vec![
                ("ultrathink", "ultrathink"),
                ("\u{2B22} Model B", "model"),
                ("orchestrate", "orchestrate"),
            ]
        );
        assert!(
            rich.spans
                .windows(2)
                .all(|w| w[0].range.end <= w[1].range.start)
        );
    }
}
