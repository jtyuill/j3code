//! A tool call row (spec §6.3, §6.4) the way omp's TUI draws it: `icon Title description
//! meta status`, then a framed preview of its body (the command and the end of its output,
//! the start of a diff, the first matches) that expands in place, and the images the tool
//! returned.

use j3_client::t3::orchestration::TurnItem;
use rune::{El, Icon, col, el, icon, row, text};
use rune_kit::{Kit, Spinner};

use super::{Ctx, body, disclosure};
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

/// Where a tool's body starts: under its title, past the icon.
pub const INDENT: f32 = 22.0;

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
pub fn view(s: &J3, cx: &Ctx, item: &TurnItem) -> El {
    let Some(view) = tool_view(item) else {
        return el();
    };
    let k = cx.k;
    let status = Status::of(k);
    let id = item.id.clone();
    let expanded = cx.expanded(&id);
    let total = view.body.as_ref().map_or(0, ToolBody::lines);
    let expandable = view.body.is_some() && (view.preview == 0 || total > view.preview || expanded);
    let icon_color = match view.status {
        ToolStatus::Running => status.info,
        ToolStatus::Failed => status.danger,
        ToolStatus::Done => k.c.text_muted,
    };
    let label = [view.title.as_str(), view.description.as_str()].join(" ");
    let description_font = if view.prose {
        k.ty.text_xsmall
    } else {
        rune::TextStyle {
            size: k.ty.text_xsmall.size,
            line_height: k.ty.text_xsmall.line_height,
            ..k.ty.mono
        }
    };
    let mut header = row()
        .key(format!("tool-toggle-{id}"))
        .label(label)
        .w_full()
        .gap(8.0)
        .px(4.0)
        .ml(-4.0)
        .center_y()
        .radius(k.radius.sm)
        .child(icon(tool_icon(view.icon), 16.0).color(icon_color))
        .child(
            text(view.title.clone())
                .font(k.ty.body_medium)
                .color(k.c.text_base),
        )
        .child(
            text(view.description.clone())
                .font(description_font)
                .color(k.c.text_muted)
                .truncate()
                .grow(1.0),
        );
    for (meta, tone) in &view.meta {
        let color = match tone {
            Tone::Muted => k.c.text_muted,
            Tone::Added => status.success,
            Tone::Removed | Tone::Failed => status.danger,
        };
        header = header.child(text(meta.clone()).font(k.ty.text_xsmall).color(color));
    }
    header = match view.status {
        ToolStatus::Running => header.child(Spinner::new().size(12.0).build(k)),
        ToolStatus::Failed => header.child(icon(&icons::CIRCLE_X, 12.0).color(status.danger)),
        ToolStatus::Done => header,
    };
    if expandable {
        let (key, toggled) = (cx.key.clone(), id.clone());
        header = header
            .child(disclosure(k, expanded))
            .hover(k.alpha(k.c.hover, 0.5))
            .cursor(rune::Cursor::Pointer)
            .on_press(move |s: &mut J3| s.toggle_feed_row(&key, &toggled));
    }
    if let Some(intent) = &view.intent {
        header = header.help(intent.clone());
    }
    let mut column = col().w_full().child(header);
    if let Some(body) = view.body.as_ref().filter(|_| expanded || view.preview > 0) {
        let limit = if expanded {
            MAX_DETAIL_LINES
        } else {
            view.preview
        };
        let hidden = total.saturating_sub(limit);
        let failed = view.status == ToolStatus::Failed;
        let mut detail = col()
            .w_full()
            .pl(INDENT)
            .pt(4.0)
            .child(body::view(k, body, limit, expanded, failed));
        if hidden > 0 || (expanded && total > view.preview) {
            let (key, toggled) = (cx.key.clone(), id.clone());
            detail = detail.child(
                el().key(format!("tool-more-{id}"))
                    .pt(2.0)
                    .cursor(rune::Cursor::Pointer)
                    .on_press(move |s: &mut J3| s.toggle_feed_row(&key, &toggled))
                    .child(
                        text(more_label(expanded, hidden))
                            .font(k.ty.text_xsmall)
                            .color(k.c.text_muted),
                    ),
            );
        }
        column = column.child(detail);
    }
    if view.images > 0 {
        let mut images = row().w_full().pl(INDENT).pt(4.0).gap(8.0).flex_wrap();
        for index in 0..view.images as u64 {
            images = images.child(output_image(s, cx, item, index));
        }
        column = column.child(images);
    }
    column
}

/// Image `index` of a tool's output, as large as it is up to the feed's width and
/// [`MAX_OUTPUT_IMAGE_HEIGHT`].
fn output_image(s: &J3, cx: &Ctx, item: &TurnItem, index: u64) -> El {
    let k = cx.k;
    let image = ImageRef::ToolOutput {
        thread_id: item.thread_id.clone(),
        item_id: item.id.clone(),
        index,
    };
    let decorate = |frame: El| {
        frame
            .key(format!("tool-image-{}-{index}", item.id))
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
                decorate(crate::images::fitted(&image, w * fit, h * fit, 0.0))
                    .label("Tool output image")
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
