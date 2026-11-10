//! A tool call row (spec §6.3, §6.4) on the activity rail: the tool's glyph on the rail (a
//! spinner while it runs, red once it failed), then `Title description meta ›` in one line,
//! then its body (a running command's output, a diff) in a tinted block that expands in
//! place, and the images the tool returned.

use j3_client::entities::Item;
use rune::{El, Icon, TextStyle, col, el, row, text};
use rune_kit::{Kit, Spinner};

use super::{Ctx, body, rail};
use crate::{
    app::J3,
    environment::ImageRef,
    feed::{Tone, ToolBody, ToolIcon, ToolStatus, tool_view},
    icons,
    images::ImageLoad,
    theme::Status,
};

/// Longest tool detail shown, in lines; the rest is summarised.
pub const MAX_DETAIL_LINES: usize = 400;

/// Tallest a tool's output image is drawn, in points.
pub const MAX_OUTPUT_IMAGE_HEIGHT: f32 = 360.0;

/// An expanded body this short has no "Show less" under it: its header is in reach.
const SHORT_BODY: usize = 12;

fn tool_icon(icon: ToolIcon) -> &'static Icon {
    match icon {
        ToolIcon::Terminal => &icons::SQUARE_TERMINAL,
        ToolIcon::Read => &icons::FILE_TEXT,
        ToolIcon::Edit => &icons::FILE_PEN,
        ToolIcon::Write => &icons::FILE_PLUS,
        ToolIcon::Delete => &icons::FILE_X,
        ToolIcon::Grep => &icons::SEARCH,
        ToolIcon::Glob => &icons::FOLDER_SEARCH,
        ToolIcon::Web => &icons::GLOBE,
        ToolIcon::Eval => &icons::CODE,
        ToolIcon::Task => &icons::BOT,
        ToolIcon::Todo => &icons::LIST_TODO,
        ToolIcon::Wait => &icons::HOURGLASS,
        ToolIcon::Message => &icons::MESSAGES_SQUARE,
        ToolIcon::Device => &icons::PLUG,
        ToolIcon::Process => &icons::CPU,
        ToolIcon::Lsp => &icons::BRACES,
        ToolIcon::Ast => &icons::SCAN_SEARCH,
        ToolIcon::Debug => &icons::BUG,
        ToolIcon::Browser => &icons::APP_WINDOW,
        ToolIcon::Github => &icons::GITHUB,
        ToolIcon::Memory => &icons::BRAIN,
        ToolIcon::Tool => &icons::WRENCH,
    }
}

/// The "… N more lines" / "… N lines not shown" / "Show less" toggle under a body.
pub fn more_label(expanded: bool, hidden: usize) -> String {
    match (expanded, hidden) {
        (true, 0) => "Show less".to_owned(),
        (true, hidden) => format!("… {hidden} lines not shown"),
        (false, hidden) => format!(
            "… {hidden} more {}",
            if hidden == 1 { "line" } else { "lines" }
        ),
    }
}

/// The row of tool item `item`.
pub fn view(s: &J3, cx: &Ctx, item: &Item) -> El {
    let Some(view) = tool_view(item) else {
        return el();
    };
    let k = cx.k;
    let status = Status::of(k);
    let id = item.id.clone();
    let expanded = cx.expanded(&id);
    let total = view.body.as_ref().map_or(0, ToolBody::lines);
    let expandable = view.body.is_some() && (view.preview == 0 || total > view.preview || expanded);
    let glyph = match view.status {
        ToolStatus::Running => Spinner::new().size(12.0).build(k),
        ToolStatus::Failed => rail::glyph(tool_icon(view.icon), status.danger),
        ToolStatus::Done => rail::glyph(tool_icon(view.icon), k.c.text_faint),
    };
    let label = [view.title.as_str(), view.description.as_str()].join(" ");
    let description_font = if view.prose {
        k.ty.text_small
    } else {
        TextStyle {
            line_height: k.ty.text_small.line_height,
            ..k.ty.mono
        }
    };
    let meta = view
        .meta
        .iter()
        .map(|(meta, tone)| {
            let color = match tone {
                Tone::Muted => k.c.text_faint,
                Tone::Added => status.success,
                Tone::Removed | Tone::Failed => status.danger,
            };
            text(meta.clone()).font(k.ty.text_xsmall).color(color)
        })
        .collect();
    let mut header = rail::header(
        k,
        text(view.title.clone())
            .font(k.ty.text_small_medium)
            .color(k.c.text_base),
        Some(
            text(view.description.clone())
                .font(description_font)
                .color(k.c.text_muted),
        ),
        meta,
        expandable.then_some(expanded),
    )
    .key(format!("tool-toggle-{id}"))
    .label(label)
    .px(4.0)
    .ml(-4.0)
    .radius(k.radius.sm);
    if expandable {
        let (key, toggled) = (cx.key.clone(), id.clone());
        header = header
            .hover(crate::theme::row_hover(k))
            .cursor(rune::Cursor::Pointer)
            .on_press(move |s: &mut J3| s.toggle_feed_row(&key, &toggled));
    }
    if let Some(intent) = &view.intent {
        header = header.help(intent.clone());
    }
    let mut column = col().w_full();
    let mut has_body = false;
    if let Some(body) = view.body.as_ref().filter(|_| expanded || view.preview > 0) {
        let limit = if expanded {
            MAX_DETAIL_LINES
        } else {
            view.preview
        };
        let hidden = total.saturating_sub(limit);
        let failed = view.status == ToolStatus::Failed;
        let more = (hidden > 0 || (expanded && total > view.preview.max(SHORT_BODY))).then(|| {
            let (key, toggled) = (cx.key.clone(), id.clone());
            el().key(format!("tool-more-{id}"))
                .w_full()
                .px(8.0)
                .py(4.0)
                .cursor(rune::Cursor::Pointer)
                .on_press(move |s: &mut J3| s.toggle_feed_row(&key, &toggled))
                .child(
                    text(more_label(expanded, hidden))
                        .font(k.ty.text_xsmall)
                        .color(k.c.text_faint),
                )
        });
        column = column.child(
            el().w_full()
                .pt(2.0)
                .pb(4.0)
                .child(body::view(k, body, limit, expanded, failed, more)),
        );
        has_body = true;
    }
    if !view.images.is_empty() {
        let mut images = row().w_full().pt(4.0).pb(4.0).gap(8.0).flex_wrap();
        for (index, attachment) in view.images.iter().enumerate() {
            images = images.child(output_image(s, cx, &item.id, index, attachment));
        }
        column = column.child(images);
        has_body = true;
    }
    rail::row(
        k,
        cx.rail,
        cx.top,
        glyph,
        header,
        has_body.then_some(column),
    )
}

/// Image `index` of a tool's output (attachment `attachment`), as large as it is up to the feed's width and
/// [`MAX_OUTPUT_IMAGE_HEIGHT`].
fn output_image(s: &J3, cx: &Ctx, item_id: &str, index: usize, attachment: &str) -> El {
    let k = cx.k;
    let image = ImageRef::Attachment {
        id: attachment.to_owned(),
    };
    let decorate = |frame: El| {
        frame
            .key(format!("tool-image-{item_id}-{index}"))
            .role("image")
            .radius(k.radius.md)
            .clip_rounded(k.radius.md)
            .border(0.5, k.c.border_base)
    };
    let frame = decorate(el()).center();
    match s.image(&cx.key.env, &image) {
        ImageLoad::Loaded(image) => match image.size().filter(|_| image.is_ready()) {
            Some([w, h]) if w > 0 && h > 0 => {
                let density = s.images.density(&image);
                let (w, h) = (w as f32 / density, h as f32 / density);
                let fit = (MAX_OUTPUT_IMAGE_HEIGHT / h).min(1.0);
                let picture = decorate(crate::images::fitted(&image, w * fit, h * fit, 0.0))
                    .label("Tool output image");
                crate::viewer::opens(picture, &image, density, "Tool output image")
            }
            _ => {
                cx.wait(&image);
                loading(frame, k)
            }
        },
        ImageLoad::Loading => loading(frame, k),
        ImageLoad::Failed => frame.label("Image unavailable").px(8.0).py(4.0).child(
            text("Image unavailable")
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted),
        ),
    }
}

fn loading(frame: El, k: &Kit) -> El {
    frame
        .label("Loading image")
        .size(64.0, 64.0)
        .child(Spinner::new().size(12.0).build(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_more_toggle_counts_what_is_hidden() {
        assert_eq!(more_label(false, 1), "… 1 more line");
        assert_eq!(more_label(false, 12), "… 12 more lines");
        assert_eq!(more_label(true, 3), "… 3 lines not shown");
        assert_eq!(more_label(true, 0), "Show less");
    }
}
