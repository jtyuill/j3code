//! An environment's archived threads, with unarchive and delete.

use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, rems,
};
use j3_client::t3::methods::ArchivedShellSnapshot;
use time::OffsetDateTime;

use crate::{environment::Environment, feed::relative_age};

enum Listing {
    Loading,
    Loaded(ArchivedShellSnapshot),
    Failed(SharedString),
}

/// A thread was restored; the workspace opens it.
pub struct Unarchived {
    pub thread_id: String,
}

pub struct ArchivedView {
    environment: Entity<Environment>,
    listing: Listing,
    error: Option<SharedString>,
}

impl EventEmitter<Unarchived> for ArchivedView {}

impl ArchivedView {
    pub fn new(environment: Entity<Environment>, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            environment,
            listing: Listing::Loading,
            error: None,
        };
        view.reload(cx);
        view
    }

    pub fn environment(&self) -> &Entity<Environment> {
        &self.environment
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let connection = self.environment.read(cx).connection().clone();
        cx.spawn(async move |this, cx| {
            let result = connection.archived().await;
            let _ = this.update(cx, |this, cx| {
                this.listing = match result {
                    Ok(snapshot) => Listing::Loaded(snapshot),
                    Err(e) => Listing::Failed(e.to_string().into()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn unarchive(&mut self, thread_id: String, cx: &mut Context<Self>) {
        let connection = self.environment.read(cx).connection().clone();
        cx.spawn(async move |this, cx| {
            let result = connection.unarchive(&thread_id).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => {
                    cx.emit(Unarchived { thread_id });
                    this.reload(cx);
                }
                Err(e) => {
                    this.error = Some(e.to_string().into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn confirm_delete(&mut self, thread_id: String, title: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            let thread_id = thread_id.clone();
            alert
                .title(format!("Delete “{title}”?"))
                .description("The conversation is removed from j3. omp’s session file stays on disk.")
                .confirm()
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let thread_id = thread_id.clone();
                    let _ = view.update(cx, |this, cx| this.delete(thread_id, cx));
                    true
                })
        });
    }

    fn delete(&mut self, thread_id: String, cx: &mut Context<Self>) {
        let connection = self.environment.read(cx).connection().clone();
        cx.spawn(async move |this, cx| {
            let result = connection.delete_thread(&thread_id).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => this.reload(cx),
                    Err(e) => this.error = Some(e.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_list(&self, snapshot: &ArchivedShellSnapshot, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        if snapshot.threads.is_empty() {
            return div()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No archived threads.")
                .into_any_element();
        }
        let now = OffsetDateTime::now_utc();
        let mut threads: Vec<_> = snapshot.threads.iter().collect();
        threads.sort_by(|a, b| b.archived_at.cmp(&a.archived_at));
        v_flex()
            .children(threads.into_iter().map(|thread| {
                let project = snapshot
                    .projects
                    .iter()
                    .find(|p| p.id == thread.project_id)
                    .map(|p| p.title.clone())
                    .unwrap_or_default();
                let age = thread
                    .archived_at
                    .as_deref()
                    .and_then(|at| relative_age(at, now))
                    .unwrap_or_default();
                let title: SharedString = thread.title.clone().into();
                let id = thread.id.clone();
                let delete_id = thread.id.clone();
                let delete_title = title.clone();
                h_flex()
                    .id(SharedString::from(format!("archived-{}", thread.id)))
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(title))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(project))
                    .child(
                        div()
                            .w(rems(2.5))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(age),
                    )
                    .child(
                        Button::new(SharedString::from(format!("unarchive-{id}")))
                            .xsmall()
                            .outline()
                            .icon(IconName::Undo2)
                            .label("Unarchive")
                            .on_click(cx.listener(move |this, _, _, cx| this.unarchive(id.clone(), cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("delete-{delete_id}")))
                            .xsmall()
                            .ghost()
                            .label("Delete…")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_delete(delete_id.clone(), delete_title.clone(), window, cx)
                            })),
                    )
            }))
            .into_any_element()
    }
}

impl Render for ArchivedView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self.environment.read(cx).label().clone();
        let body = match &self.listing {
            Listing::Loading => div().p_4().child(Spinner::new()).into_any_element(),
            Listing::Failed(error) => div()
                .p_4()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(format!("Couldn’t load archived threads: {error}"))
                .into_any_element(),
            Listing::Loaded(snapshot) => {
                let snapshot = snapshot.clone();
                self.render_list(&snapshot, cx)
            }
        };
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h_10()
                    .px_4()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().font_medium().text_sm().child("Archived threads"))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(label)),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().px_4().py_2().text_xs().text_color(theme.danger).child(error))
            })
            .child(div().id("archived-list").flex_1().min_h_0().overflow_y_scroll().child(body))
    }
}
