//! herdr's goto picker (spec §8.4, "Go to agent"): every agent, grouped by
//! `"{project} · {env}"`, with search and the single-key state filters (`b` blocked, `w`
//! working, `i` idle, `d` done, `a` all, shown as a hint row). `/` moves to the search field,
//! ↑↓ move the selection, Enter jumps to the agent and closes. `Dialog::Goto` draws [`body`];
//! the picker's state lives in `s.sidebar.goto`.

use j3_client::workflow::{AgentOrder, AgentStatus};
use rune::{Cx, El, Key, KeyEvent, Mods, ScrollSpec, Tx, col, row, spacer, text};
use rune_kit::{Kit, TextInput};

use crate::{
    app::{J3, Ui},
    dialogs::Dialog,
    icons,
    sidebar::{AgentRow, HerdrRows, ThreadKey, glyph},
};

/// The filter keys in hint-row order: blocked, working, idle, done.
const FILTERS: [AgentStatus; 4] = [
    AgentStatus::Blocked,
    AgentStatus::Working,
    AgentStatus::Idle,
    AgentStatus::Done,
];

/// The picker's state while its dialog is open.
#[derive(Default)]
pub struct GotoState {
    pub query: rune::TextEdit,
    /// The state filter (`None`: all).
    pub filter: Option<AgentStatus>,
    /// Index into the visible agents.
    pub selected: usize,
}

/// Whether `agent` passes `filter` and contains every word of the search.
fn matches(agent: &AgentRow, filter: Option<AgentStatus>, words: &[String]) -> bool {
    if filter.is_some_and(|filter| agent.status != filter) {
        return false;
    }
    let haystack = format!(
        "{} {} {} {}",
        agent.title,
        agent.project,
        agent.env_label,
        agent.status.label()
    )
    .to_lowercase();
    words.iter().all(|word| haystack.contains(word.as_str()))
}

fn words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// The visible agents, in space order.
fn visible<'r>(rows: &'r HerdrRows, state: &GotoState) -> Vec<&'r AgentRow> {
    let words = words(&state.query.text);
    rows.agents
        .iter()
        .filter(|agent| matches(agent, state.filter, &words))
        .collect()
}

impl J3 {
    /// Every agent in space order, whatever the sidebar's workflow and order.
    fn goto_rows(&self) -> HerdrRows {
        HerdrRows::build(
            &self.env_snapshots(),
            self.prefs.ui().status_indicators,
            AgentOrder::Spaces,
        )
    }

    /// Opens the picker afresh: no search, no filter, the open thread selected.
    pub fn open_goto(&mut self, tx: &mut Tx) {
        self.sidebar.goto = GotoState::default();
        let open = self.open_thread_key();
        let rows = self.goto_rows();
        self.sidebar.goto.selected = visible(&rows, &self.sidebar.goto)
            .iter()
            .position(|agent| Some(&agent.key) == open.as_ref())
            .unwrap_or(0);
        self.open_dialog(Dialog::goto(), tx);
    }

    fn goto_move(&mut self, delta: isize, tx: &mut Tx) {
        let rows = self.goto_rows();
        let shown = visible(&rows, &self.sidebar.goto);
        if shown.is_empty() {
            return;
        }
        let ix = (self.sidebar.goto.selected as isize + delta).clamp(0, shown.len() as isize - 1);
        self.sidebar.goto.selected = ix as usize;
        tx.scroll_into_view(shown[ix as usize].key.element_id("goto"));
    }

    fn goto_filter(&mut self, filter: Option<AgentStatus>) {
        self.sidebar.goto.filter = filter;
        self.sidebar.goto.selected = 0;
    }

    /// Jumps to the selected agent and closes the picker.
    pub fn goto_confirm(&mut self, tx: &mut Tx) {
        let rows = self.goto_rows();
        let key = visible(&rows, &self.sidebar.goto)
            .get(self.sidebar.goto.selected)
            .map(|agent| agent.key.clone());
        if let Some(key) = key {
            self.goto_jump(key, tx);
        }
    }

    fn goto_jump(&mut self, key: ThreadKey, tx: &mut Tx) {
        self.close_dialog();
        self.open_thread(key, tx);
    }
}

/// Keys while the picker is open and its search field doesn't have them: ↑↓, Enter, `/`
/// and the filter letters. True when the picker took the key.
pub fn on_key(s: &mut J3, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if ev.mods.cmd || ev.mods.ctrl || ev.mods.alt || ev.mods.logo {
        return false;
    }
    match &ev.key {
        Key::Up => s.goto_move(-1, tx),
        Key::Down => s.goto_move(1, tx),
        Key::Enter => s.goto_confirm(tx),
        Key::Text(typed) => match typed.as_str() {
            "/" => s.sidebar.goto.query.focused = true,
            "a" => {
                s.sidebar.goto.query.clear();
                s.goto_filter(None);
            }
            typed => {
                let mut chars = typed.chars();
                let (Some(letter), None) = (chars.next(), chars.next()) else {
                    return false;
                };
                let Some(status) = FILTERS
                    .into_iter()
                    .find(|status| status.filter_key() == Some(letter))
                else {
                    return false;
                };
                s.goto_filter(Some(status));
            }
        },
        _ => return false,
    }
    true
}

/// The search field's own keys: ↑↓ move the selection (Enter submits).
fn search_key(s: &mut J3, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if ev.mods != Mods::default() {
        return false;
    }
    match ev.key {
        Key::Up => s.goto_move(-1, tx),
        Key::Down => s.goto_move(1, tx),
        _ => return false,
    }
    true
}

/// The picker: search field, filter hints with the count, then the grouped agents.
pub fn body(s: &J3, _cx: &mut Cx<J3>, ui: &mut Ui) -> El {
    let k = ui.k;
    let state = &s.sidebar.goto;
    let rows = s.goto_rows();
    let open = s.open_thread_key();
    let shown = visible(&rows, state);
    let search = TextInput::new(&state.query)
        .placeholder("Search agents")
        .label("Search agents")
        .leading_icon(&icons::SEARCH)
        .on_edit(|s: &mut J3| &mut s.sidebar.goto.query)
        .width(None)
        .on_change(|s: &mut J3, _: &mut Tx| s.sidebar.goto.selected = 0)
        .on_key(search_key)
        .on_submit(|s: &mut J3, tx: &mut Tx| s.goto_confirm(tx))
        .build(k)
        .key("goto-search");

    let mut hints = row().key("goto-hints").w_full().gap(12.0).center_y();
    for status in FILTERS {
        let letter = status.filter_key().unwrap_or_default();
        hints = hints.child(hint(
            letter,
            status.label(),
            state.filter == Some(status),
            k,
        ));
    }
    hints = hints
        .child(hint('a', "all", state.filter.is_none(), k))
        .child(spacer())
        .child(
            text(if shown.len() == 1 {
                "1 agent".to_owned()
            } else {
                format!("{} agents", shown.len())
            })
            .key("goto-count")
            .font(k.ty.text_xsmall)
            .color(k.c.text_muted),
        );

    let mut list = col().key("goto-rows").w_full().gap(2.0);
    let mut heading: Option<String> = None;
    for (ix, agent) in shown.iter().enumerate() {
        let group = format!("{} · {}", agent.project, agent.env_label);
        if heading.as_ref() != Some(&group) {
            list = list.child(
                row()
                    .key(format!("goto-group-{ix}"))
                    .w_full()
                    .px(8.0)
                    .pt(8.0)
                    .child(
                        text(group.clone())
                            .font(k.ty.text_xsmall_medium)
                            .color(k.c.text_muted),
                    ),
            );
            heading = Some(group);
        }
        let current = open.as_ref() == Some(&agent.key);
        list = list.child(entry(
            agent,
            ix,
            ix == state.selected,
            current,
            rows.indicators,
            k,
        ));
    }
    if shown.is_empty() {
        list = list.child(
            row().key("goto-none").w_full().px(8.0).py(8.0).child(
                text("No matching agents")
                    .font(k.ty.text_small)
                    .color(k.c.text_muted),
            ),
        );
    }
    col()
        .key("goto-picker")
        .w_full()
        .px(16.0)
        .pb(16.0)
        .gap(8.0)
        .child(search)
        .child(hints)
        .child(
            col()
                .key("goto-list")
                .w_full()
                .max_h(384.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(list),
        )
}

/// `"{key} {label}"`, emphasised while it is the filter.
fn hint(letter: char, label: &str, active: bool, k: &Kit) -> El {
    text(format!("{letter} {label}"))
        .key(format!("goto-filter-{letter}"))
        .font(if active {
            k.ty.text_xsmall_medium
        } else {
            k.ty.text_xsmall
        })
        .color(if active {
            k.c.text_base
        } else {
            k.c.text_muted
        })
}

/// One agent: glyph, title (medium when it is the open thread) and status.
fn entry(
    agent: &AgentRow,
    ix: usize,
    selected: bool,
    current: bool,
    indicators: j3_client::workflow::StatusIndicators,
    k: &Kit,
) -> El {
    let c = &k.c;
    let mut e = row()
        .key(agent.key.element_id("goto"))
        .role("option")
        .label(format!("{}, {}", agent.title, agent.status.label()))
        .w_full()
        .h(28.0)
        .px(8.0)
        .gap(8.0)
        .center_y()
        .radius(k.radius.md)
        .on_press_tx(move |s: &mut J3, tx: &mut Tx| {
            s.sidebar.goto.selected = ix;
            s.goto_confirm(tx);
        })
        .child(glyph(agent.status, indicators, k))
        .child(
            text(agent.title.clone())
                .font(if current {
                    k.ty.text_small_medium
                } else {
                    k.ty.text_small
                })
                .color(c.text_base)
                .truncate()
                .grow(1.0)
                .shrink(),
        )
        .child(
            text(agent.status.label())
                .font(k.ty.text_xsmall)
                .color(if selected { c.text_base } else { c.text_muted }),
        );
    e = k.interactive(e);
    if selected {
        e = e.fill(c.pressed);
    }
    e
}
