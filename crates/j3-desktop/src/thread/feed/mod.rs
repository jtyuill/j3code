//! A thread's feed (spec §5.1, §6): the scrolling list of turn items — messages, reasoning,
//! tool calls, subagents and notices — with the thread's sync state in its place while it
//! loads, fails or was deleted.
//!
//! The list is virtual (`virtual_rows`): only the rows near the viewport are built, each a
//! `memo` versioned by its item's revision (bumped when the item changes between thread
//! states), its expansion and the state of the images it draws, so a streaming reply rebuilds
//! its own row only. The viewport sticks to the bottom: it follows a streaming reply, and a
//! wheel scroll that ends at the bottom keeps following.
//!
//! Rows space themselves in one rhythm per turn: 44 points before a user message (a new
//! turn), 24 after it, 4 between consecutive activity rows (tools, reasoning, plan updates,
//! notices) and 16 between anything else within the turn. The space sits above a row and the
//! feed's end padding on the list, so appending never changes the height of the row before.
//! Activity rows sit on one rail (`rail`): a glyph column whose hairline joins consecutive
//! activity rows.
//!
//! Motion follows `crate::motion`: tool calls arrive constantly in a live turn, so a row
//! that arrives complete fades in quickly while its height glides open on a short spring of
//! its own — no clipped unroll, no speed-capped climb in stages as its output lands. Only
//! streaming text keeps the stack's paced growth.

mod body;
mod lines;
mod rail;
mod rows;
#[cfg(test)]
mod tests;
mod tool;

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
    sync::Arc,
};

use j3_client::{
    ThreadState, ThreadSync,
    t3::orchestration::{TurnItem, TurnItemKind},
};
use rune::{Color, Cx, El, GrowthSpec, Image, ScrollSpec, col, el, icon, memo, text};
use rune_kit::{Kit, Spinner};

use crate::{
    app::{J3, Ui},
    environment::ImageRef,
    feed::{self, is_activity, is_hidden},
    icons,
    images::ImageLoad,
    math::view::{Look, doc},
    motion,
    sidebar::ThreadKey,
    theme::{self, Metrics, Status},
};

/// Height a row is taken to be until it is measured.
const ESTIMATED_ROW: f32 = 96.0;

/// Space above a row: before a new turn, after the user's message, between consecutive
/// activity rows, and between anything else within a turn.
const TURN_GAP: f32 = 44.0;
const AFTER_PROMPT_GAP: f32 = 24.0;
const ACTIVITY_GAP: f32 = 4.0;
const WITHIN_TURN_GAP: f32 = 16.0;
/// Space at the feed's ends.
const END_PAD: f32 = 8.0;

/// A thread's feed UI state, kept in its `ThreadUi`.
#[derive(Default)]
pub struct FeedUi {
    /// Rows the user expanded (tool bodies, reasoning, subagents), by item id; ids that left
    /// the feed are dropped when a new state is drawn.
    expanded: RefCell<HashSet<String>>,
    /// The visible rows of the last state drawn and their items' revisions.
    rows: RefCell<Rows>,
    /// Images rows drew before they were decoded: the feed's poll draws again once any is.
    wait: RefCell<Vec<Image>>,
}

#[derive(Default)]
struct Rows {
    state: Option<Arc<ThreadState>>,
    /// Feed indices of the items shown (`!feed::is_hidden`).
    visible: Rc<[usize]>,
    /// Each shown item's revision: bumped whenever the item differs from the last state's.
    revisions: HashMap<String, u64>,
    next: u64,
}

impl FeedUi {
    /// The rows of `state`, updating the revisions (and forgetting the expansion of items
    /// that left the feed) when it is a new state.
    fn sync(&self, state: &Arc<ThreadState>) -> Rc<[usize]> {
        let mut rows = self.rows.borrow_mut();
        if rows
            .state
            .as_ref()
            .is_some_and(|last| Arc::ptr_eq(last, state))
        {
            return rows.visible.clone();
        }
        let last = rows.state.take();
        let before: HashMap<&str, &TurnItem> = last
            .iter()
            .flat_map(|last| last.feed())
            .map(|projected| (projected.item.id.as_str(), &projected.item))
            .collect();
        let feed = state.feed();
        let visible: Rc<[usize]> = (0..feed.len())
            .filter(|&ix| !is_hidden(&feed[ix].item))
            .collect();
        let mut revisions = HashMap::with_capacity(visible.len());
        for &ix in visible.iter() {
            let item = &feed[ix].item;
            let kept = before
                .get(item.id.as_str())
                .filter(|old| **old == item)
                .and_then(|_| rows.revisions.get(&item.id).copied());
            let revision = kept.unwrap_or_else(|| {
                rows.next += 1;
                rows.next
            });
            revisions.insert(item.id.clone(), revision);
        }
        rows.revisions = revisions;
        self.expanded
            .borrow_mut()
            .retain(|id| feed.iter().any(|projected| projected.item.id == *id));
        rows.visible = visible.clone();
        rows.state = Some(state.clone());
        visible
    }

    fn revision(&self, id: &str) -> u64 {
        self.rows.borrow().revisions.get(id).copied().unwrap_or(0)
    }

    /// Whether item `id`'s row is expanded.
    pub fn is_expanded(&self, id: &str) -> bool {
        self.expanded.borrow().contains(id)
    }
}

impl J3 {
    /// Expands or collapses item `id`'s row in `key`'s feed (a tool body, reasoning, a
    /// subagent card); the choice is kept while the item stays in the feed.
    pub fn toggle_feed_row(&mut self, key: &ThreadKey, id: &str) {
        if let Some(thread) = self.threads.get_mut(key) {
            let expanded = thread.feed.expanded.get_mut();
            if !expanded.remove(id) {
                expanded.insert(id.to_owned());
            }
        }
    }

    /// Stops subagent `subagent_id` of run `run_id` in `key`'s thread.
    pub fn stop_subagent(&mut self, key: &ThreadKey, run_id: &str, subagent_id: &str) {
        let Some(connection) = self.envs.get(&key.env).map(|env| env.connection().clone()) else {
            return;
        };
        let (thread_id, run_id, subagent_id) = (
            key.thread_id.clone(),
            run_id.to_owned(),
            subagent_id.to_owned(),
        );
        self.run_command(
            "stop the subagent",
            async move {
                connection
                    .stop_subagent(&thread_id, &run_id, &subagent_id)
                    .await
            },
            |_, _, _| {},
        );
    }

    /// Before a frame: true when an image a feed row waits on finished decoding, so the
    /// rows showing it draw again.
    pub(crate) fn poll_feed(&mut self) -> bool {
        let mut changed = false;
        for thread in self.threads.values() {
            let mut wait = thread.feed.wait.borrow_mut();
            let before = wait.len();
            wait.retain(|image| !image.is_ready() && image.error().is_none());
            changed |= wait.len() != before;
        }
        changed
    }
}

/// What a row reads besides the state: the kit, the thread and its feed state.
pub struct Ctx<'a> {
    pub k: &'a Kit,
    pub key: &'a ThreadKey,
    pub state: &'a ThreadState,
    feed: &'a FeedUi,
    /// The text column's width in points.
    pub width: f32,
    /// Device pixels per point.
    scale: f32,
    /// Typeset math (the `math_rendering` preference).
    math: bool,
    /// Space above the row, in points; an activity row draws it itself, beside its rail.
    pub top: f32,
    /// Whether the rows around an activity row are on the rail too.
    pub rail: rail::Rail,
}

impl Ctx<'_> {
    pub fn expanded(&self, id: &str) -> bool {
        self.feed.is_expanded(id)
    }

    /// Notes that a row drew `image` before it was decoded.
    pub fn wait(&self, image: &Image) {
        let mut wait = self.feed.wait.borrow_mut();
        if !wait.iter().any(|waiting| waiting.id() == image.id()) {
            wait.push(image.clone());
        }
    }

    pub fn wait_all(&self, images: &[Image]) {
        for image in images {
            self.wait(image);
        }
    }

    /// The look of markdown in `ink`.
    pub fn look(&self, ink: Color, streaming: bool) -> Look<'_> {
        Look {
            k: self.k,
            ink,
            scale: self.scale,
            width: self.width,
            streaming,
            math: self.math,
        }
    }
}

/// A chevron: down when expanded, right when not.
pub fn disclosure(k: &Kit, expanded: bool) -> El {
    icon(
        if expanded {
            &icons::CHEVRON_DOWN
        } else {
            &icons::CHEVRON_RIGHT
        },
        12.0,
    )
    .color(k.c.text_muted)
}

/// The feed of `key`'s thread, or the note that stands in for it.
pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> El {
    let k = ui.k;
    let note = |message: String| {
        el().key("feed-note")
            .w_full()
            .grow(1.0)
            .center()
            .child(text(message).font(k.ty.body).color(k.c.text_muted))
    };
    let loading = || {
        el().key("feed-note")
            .w_full()
            .grow(1.0)
            .center()
            .child(Spinner::new().build(k))
    };
    let Some(thread) = s.threads.get(key) else {
        return loading();
    };
    let state = &thread.state;
    match &state.sync {
        ThreadSync::Deleted => return note("This thread was deleted.".into()),
        ThreadSync::Failed(e) if state.projection.is_none() => {
            return note(format!("Couldn’t load the thread: {e}"));
        }
        ThreadSync::Loading => return loading(),
        _ if state.projection.is_none() => return loading(),
        _ => {}
    }
    let visible = thread.feed.sync(state);
    if visible.is_empty() {
        return note("No messages yet.".into());
    }
    let (dark, scale) = (k.dark, cx.env().scale);
    let count = visible.len();
    let version = Rc::as_ptr(&visible) as *const () as usize;
    let (key_of, visible_of) = (key.clone(), visible.clone());
    let build_key = key.clone();
    let build = move |s: &J3, i: usize| build_row(s, &build_key, &visible, i, dark, scale);
    // Keyed by thread: switching threads mounts a new stack, whose rows land at rest
    // instead of growing open as rows new to the old one.
    let viewport = col()
        .key(key.element_id("feed-viewport"))
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: true })
        .child(
            col()
                .key("feed-rows")
                .w_full()
                .py(END_PAD)
                .growth(GrowthSpec::default())
                .virtual_rows(
                    count,
                    ESTIMATED_ROW,
                    move |s: &J3, i| {
                        let id = s
                            .threads
                            .get(&key_of)
                            .and_then(|thread| thread.state.feed().get(*visible_of.get(i)?))
                            .map_or("", |projected| projected.item.id.as_str());
                        Cow::Owned(format!("feed-{id}"))
                    },
                    build,
                )
                .virtual_rows_version(version),
        );
    rune_kit::scroll_bar(k, "feed-area", "feed", viewport)
        .w_full()
        .grow(1.0)
        .min_h(0.0)
}

/// Row `i` of the visible rows `visible` of `key`'s feed.
fn build_row(s: &J3, key: &ThreadKey, visible: &[usize], i: usize, dark: bool, scale: f32) -> El {
    let Some(thread) = s.threads.get(key) else {
        return el();
    };
    let feed = thread.state.feed();
    let item_at = |i: usize| Some(&feed.get(*visible.get(i)?)?.item);
    let Some(item) = item_at(i) else {
        return el();
    };
    let k = theme::kit(dark, s.prefs.ui());
    let m = Metrics::new(s.prefs.ui());
    let previous = i.checked_sub(1).and_then(item_at);
    let next = item_at(i + 1);
    let is_prompt = |item: &TurnItem| matches!(item.kind, TurnItemKind::UserMessage(_));
    let top = match previous {
        None => 0.0,
        Some(_) if is_prompt(item) => TURN_GAP,
        Some(previous) if is_prompt(previous) => AFTER_PROMPT_GAP,
        Some(previous) if is_activity(previous) && is_activity(item) => ACTIVITY_GAP,
        Some(_) => WITHIN_TURN_GAP,
    };
    let on_rail = is_activity(item);
    let rail = rail::Rail {
        above: on_rail && previous.is_some_and(is_activity),
        below: on_rail && next.is_some_and(is_activity),
    };
    // Text that streams in keeps the stack's paced growth; everything else arrives whole.
    let streams = match &item.kind {
        TurnItemKind::AssistantMessage(_) => true,
        TurnItemKind::Reasoning(reasoning) => reasoning.streaming,
        _ => false,
    };
    let cx = Ctx {
        k: &k,
        key,
        state: &thread.state,
        feed: &thread.feed,
        width: m.measure,
        scale,
        math: s.prefs.ui().math_rendering,
        top,
        rail,
    };
    let label = match &item.kind {
        TurnItemKind::UserMessage(message) => Some(message.text.clone()),
        TurnItemKind::AssistantMessage(message) => Some(message.text.clone()),
        _ => None,
    };
    let frame = move |content: El| {
        // The measure is the text's width; the side padding sits outside it.
        let inner = el()
            .w_full()
            .max_w(m.measure + 32.0)
            .px(16.0)
            .child(content);
        let row = col()
            .w_full()
            .items_center()
            .pt(if on_rail { 0.0 } else { top })
            .child(inner);
        let row = if streams {
            row
        } else {
            row.fold_glide(motion::QUICK).transition(motion::arrive())
        };
        match label {
            Some(label) => row.label(label),
            None => row,
        }
    };
    let expanded = cx.expanded(&item.id);
    let base = (
        thread.feed.revision(&item.id),
        top.to_bits(),
        streams,
        expanded,
        dark,
        k.ty.body.size.to_bits(),
        k.ty.mono.size.to_bits(),
        (m.measure.to_bits(), scale.to_bits()),
        cx.math,
        rail,
    );
    // Item ids repeat across threads (`ti_1`…): the thread is part of the row's identity.
    let id = key.element_id(&format!("feed-row-{}", item.id));
    // Markdown resolves its images first: their states version the row.
    match &item.kind {
        TurnItemKind::AssistantMessage(message) => {
            let look = cx.look(k.c.text_base, message.streaming);
            let doc = doc(s, &key.env, &item.thread_id, &message.text, look);
            cx.wait_all(&doc.wait);
            return memo(id, (base, doc.images), || {
                frame(doc.view(s, &format!("md-{}", item.id), look))
            });
        }
        TurnItemKind::Reasoning(reasoning) if expanded => {
            let look = cx.look(k.c.text_muted, reasoning.streaming);
            let images = doc(s, &key.env, &item.thread_id, &reasoning.text, look).images;
            return memo(id, (base, images), || frame(content(s, &cx, item)));
        }
        _ => {}
    }
    let extra = extra_version(s, &cx, item);
    memo(id, (base, extra), || frame(content(s, &cx, item)))
}

/// What besides the item a row's look depends on: the images it shows, its subagent's
/// state, the environment's model names.
fn extra_version(s: &J3, cx: &Ctx, item: &TurnItem) -> u64 {
    let mut hasher = DefaultHasher::new();
    let mut image = |image: &ImageRef| match s.image(&cx.key.env, image) {
        ImageLoad::Loading => 0u8.hash(&mut hasher),
        ImageLoad::Failed => 1u8.hash(&mut hasher),
        ImageLoad::Loaded(image) => (image.id(), image.is_ready()).hash(&mut hasher),
    };
    match &item.kind {
        TurnItemKind::UserMessage(message) => {
            for attachment in &message.attachments {
                if let Some(attachment) = ImageRef::attachment(attachment) {
                    image(&attachment);
                }
            }
            if let Some(env) = s.envs.get(&cx.key.env) {
                (Arc::as_ptr(env.models()) as *const () as usize).hash(&mut hasher);
            }
        }
        TurnItemKind::CommandExecution(_)
        | TurnItemKind::FileChange(_)
        | TurnItemKind::FileSearch(_)
        | TurnItemKind::WebSearch(_)
        | TurnItemKind::DynamicTool(_) => {
            let images = feed::tool_view(item).map_or(0, |view| view.images);
            for index in 0..images as u64 {
                image(&ImageRef::ToolOutput {
                    thread_id: item.thread_id.clone(),
                    item_id: item.id.clone(),
                    index,
                });
            }
        }
        TurnItemKind::Subagent(subagent) => {
            if let Some(entity) = cx.state.subagent(&subagent.subagent_id) {
                (
                    entity.status,
                    &entity.title,
                    &entity.model,
                    &entity.progress,
                    &entity.result,
                    &entity.run_id,
                )
                    .hash(&mut hasher);
            }
        }
        _ => {}
    }
    hasher.finish()
}

/// The content of `item`'s row.
fn content(s: &J3, cx: &Ctx, item: &TurnItem) -> El {
    let k = cx.k;
    let status = Status::of(k);
    match &item.kind {
        TurnItemKind::UserMessage(message) => rows::user_message(
            s,
            cx,
            &item.id,
            &message.text,
            &message.attachments,
            message.input_intent,
        ),
        TurnItemKind::AssistantMessage(message) => {
            let look = cx.look(k.c.text_base, message.streaming);
            doc(s, &cx.key.env, &item.thread_id, &message.text, look).view(
                s,
                &format!("md-{}", item.id),
                look,
            )
        }
        TurnItemKind::Reasoning(reasoning) => {
            rows::reasoning(s, cx, item, &reasoning.text, reasoning.streaming)
        }
        TurnItemKind::UserInputRequest(request) => rows::question(cx, item.status, request),
        TurnItemKind::TodoList(todo) => rows::notice(
            cx,
            &icons::CIRCLE_CHECK,
            k.c.text_faint,
            k.c.text_muted,
            feed::todo_notice(todo),
        ),
        TurnItemKind::Error(error) => rows::notice(
            cx,
            &icons::CIRCLE_ALERT,
            status.danger,
            k.c.text_base,
            feed::error_message(error),
        ),
        TurnItemKind::SystemNotice(notice) => rows::notice(
            cx,
            &icons::INFO,
            status.info,
            k.c.text_muted,
            notice.message.clone(),
        ),
        TurnItemKind::RunInterruptResult(result) => rows::notice(
            cx,
            &icons::SQUARE,
            k.c.text_faint,
            k.c.text_muted,
            result.message.clone(),
        ),
        TurnItemKind::Compaction(compaction) => {
            rows::compaction(k, feed::compaction_label(compaction))
        }
        TurnItemKind::CommandExecution(_)
        | TurnItemKind::FileChange(_)
        | TurnItemKind::FileSearch(_)
        | TurnItemKind::WebSearch(_)
        | TurnItemKind::DynamicTool(_) => tool::view(s, cx, item),
        TurnItemKind::Subagent(subagent) => rows::subagent(cx, item, subagent),
        // Stop requests and unknown kinds are hidden before rows are built.
        TurnItemKind::RunInterruptRequest(_) | TurnItemKind::Unknown(_) => el(),
    }
}
