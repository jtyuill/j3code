//! The thread header's git control, ported from t3's `GitActionsControl`
//! (`apps/web/src/components/GitActionsControl.tsx`) without feature
//! branches or per-file selection: the quick action button with a menu
//! (Commit…, Push, Pull, and on GitHub Create PR / View PR), the commit
//! dialog, and progress and result notifications.

use std::{rc::Rc, time::Duration};

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _, DropdownButton},
    dialog::{DialogAction, DialogClose, DialogFooter},
    h_flex,
    input::{Textarea, TextareaState},
    menu::PopupMenuItem,
    notification::{Notification, NotificationType},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, AsyncWindowContext, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, WeakEntity, Window, div, rems,
};
use j3_client::{
    GitActionUpdate,
    t3::git::{
        GitActionProgress, GitStackedAction, GitToast, GitToastCta, VcsChangeRequestState,
        VcsPullStatus, VcsStatusResult,
    },
    workflow::git::{self, QuickActionKind},
};

use crate::environment::Environment;

/// How often the status is refreshed while the control lives.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

pub struct GitControl {
    environment: Entity<Environment>,
    thread_id: String,
    /// The thread's project directory, once the shell knows it.
    cwd: Option<String>,
    /// The latest status of `cwd`; `None` when it could not be read.
    status: Option<VcsStatusResult>,
    /// Whether a status (or the failure to read one) arrived for `cwd`.
    loaded: bool,
    /// The running action's current phase ("Committing...").
    running: Option<SharedString>,
    refresh: Option<Task<()>>,
    /// A refresh was asked for while one was in flight.
    refresh_again: bool,
    _poll: Task<()>,
    _subscription: Subscription,
}

impl GitControl {
    pub fn new(environment: Entity<Environment>, thread_id: &str, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&environment, |this, _, cx| this.on_environment(cx));
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        });
        let mut control = Self {
            environment,
            thread_id: thread_id.to_owned(),
            cwd: None,
            status: None,
            loaded: false,
            running: None,
            refresh: None,
            refresh_again: false,
            _poll: poll,
            _subscription: subscription,
        };
        control.on_environment(cx);
        control
    }

    /// Follows the thread's project directory; a new one is read at once.
    fn on_environment(&mut self, cx: &mut Context<Self>) {
        let env = self.environment.read(cx);
        let shell = &env.state().shell;
        let root = shell
            .thread(&self.thread_id)
            .and_then(|thread| shell.project(&thread.project_id))
            .map(|project| project.workspace_root.as_str());
        if root == self.cwd.as_deref() {
            return;
        }
        self.cwd = root.map(str::to_owned);
        self.status = None;
        self.loaded = false;
        self.refresh(cx);
        cx.notify();
    }

    fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// Reads the status again (`vcs.refreshStatus`). A request while one is
    /// in flight runs once that one ends.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.cwd.clone() else {
            return;
        };
        if self.refresh.is_some() {
            self.refresh_again = true;
            return;
        }
        let connection = self.environment.read(cx).connection().clone();
        self.refresh = Some(cx.spawn(async move |this, cx| {
            let result = connection.git_status(&cwd).await;
            let _ = this.update(cx, |this, cx| {
                this.refresh = None;
                if this.cwd.as_deref() == Some(cwd.as_str()) {
                    this.status = result
                        .inspect_err(|e| tracing::debug!("git status of {cwd}: {e}"))
                        .ok();
                    this.loaded = true;
                    cx.notify();
                }
                if std::mem::take(&mut this.refresh_again) {
                    this.refresh(cx);
                }
            });
        }));
    }

    /// Runs a stacked action; its phases label the button and its outcome
    /// is a notification. The action outlives the control, so closing the
    /// thread does not interrupt a commit.
    fn run_action(
        &mut self,
        action: GitStackedAction,
        commit_message: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(cwd) = self.cwd.clone() else {
            return;
        };
        if self.busy() {
            return;
        }
        let generates = commit_message.is_none()
            && self
                .status
                .as_ref()
                .is_some_and(|status| status.has_working_tree_changes);
        self.running = Some(first_phase(action, generates).into());
        cx.notify();
        let mut run =
            self.environment
                .read(cx)
                .connection()
                .run_git_action(&cwd, action, commit_message);
        cx.spawn_in(window, async move |this, cx| {
            let outcome = loop {
                match run.next().await {
                    Some(GitActionUpdate::Progress(event)) => {
                        if let GitActionProgress::PhaseStarted { label, .. } = event.progress {
                            let _ = this.update(cx, |this, cx| {
                                this.running = Some(label.into());
                                cx.notify();
                            });
                        }
                    }
                    Some(GitActionUpdate::Finished(outcome)) => break outcome,
                    None => break Err(j3_client::Error::Closed),
                }
            };
            Self::finish(&this, cx);
            let _ = cx.update(|window, cx| match outcome {
                Ok(result) => notify_toast(result.toast, this, window, cx),
                Err(e) => notify_error(e, window, cx),
            });
        })
        .detach();
    }

    /// Opens the branch's pull request in the browser.
    fn open_pr(&mut self, cx: &mut Context<Self>) {
        if let Some(pr) = self.status.as_ref().and_then(|status| status.pr.as_ref()) {
            cx.open_url(&pr.url);
        }
    }

    /// Pulls (`vcs.pull`) and reports the outcome.
    fn pull(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cwd) = self.cwd.clone() else {
            return;
        };
        if self.busy() {
            return;
        }
        self.running = Some("Pulling...".into());
        cx.notify();
        let connection = self.environment.read(cx).connection().clone();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = connection.pull(&cwd).await;
            Self::finish(&this, cx);
            let _ = cx.update(|window, cx| match outcome {
                Ok(pull) => {
                    let (title, description) = match pull.status {
                        VcsPullStatus::Pulled => (
                            "Pulled",
                            format!(
                                "Updated {} from {}",
                                pull.ref_name,
                                pull.upstream_ref.as_deref().unwrap_or("upstream")
                            ),
                        ),
                        VcsPullStatus::SkippedUpToDate => (
                            "Already up to date",
                            format!("{} is already synchronized.", pull.ref_name),
                        ),
                    };
                    window.push_notification(
                        Notification::new()
                            .title(title)
                            .message(description)
                            .with_type(NotificationType::Success),
                        cx,
                    );
                }
                Err(e) => notify_error(e, window, cx),
            });
        })
        .detach();
    }

    /// An action ended: the control is free and the status is read again.
    fn finish(this: &WeakEntity<Self>, cx: &mut AsyncWindowContext) {
        let _ = this.update(cx, |this, cx| {
            this.running = None;
            this.refresh(cx);
            cx.notify();
        });
    }

    /// t3's commit dialog minus the per-file checkboxes: the branch, the
    /// changed files, an optional message, and Commit or Commit & push.
    fn open_commit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(status) = self.status.clone().filter(|_| !self.busy()) else {
            return;
        };
        let message = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder("Leave empty to auto-generate")
        });
        let control = cx.weak_entity();
        // A GitHub feature branch without a pull request can open one.
        let offers_pr = status.source_control_provider.is_some()
            && !status.is_default_ref
            && status.has_primary_remote
            && status
                .pr
                .as_ref()
                .is_none_or(|pr| pr.state != VcsChangeRequestState::Open);
        let status = Rc::new(status);
        let dialog_message = message.clone();
        let width = window.rem_size() * 32.;
        window.open_dialog(cx, move |dialog, _, cx| {
            let commit = commit_runner(
                control.clone(),
                dialog_message.clone(),
                GitStackedAction::Commit,
            );
            let commit_push = commit_runner(
                control.clone(),
                dialog_message.clone(),
                GitStackedAction::CommitPush,
            );
            let commit_push_pr = commit_runner(
                control.clone(),
                dialog_message.clone(),
                GitStackedAction::CommitPushPr,
            );
            dialog
                .title("Commit changes")
                .w(width)
                .child(commit_body(&status, &dialog_message, cx))
                .footer(
                    DialogFooter::new()
                        .gap_2()
                        .child(
                            DialogClose::new()
                                .child(Button::new("dialog-cancel").outline().label("Cancel")),
                        )
                        .when(offers_pr, |footer| {
                            footer.child(
                                Button::new("git-commit-push-pr")
                                    .outline()
                                    .label("Commit, push & PR")
                                    .on_click(move |_, window, cx| {
                                        commit_push_pr(window, cx);
                                        window.close_dialog(cx);
                                    }),
                            )
                        })
                        .child(
                            Button::new("git-commit-push")
                                .outline()
                                .label("Commit & push")
                                .disabled(!status.has_upstream && !status.has_primary_remote)
                                .on_click(move |_, window, cx| {
                                    commit_push(window, cx);
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            DialogAction::new()
                                .child(Button::new("git-commit").primary().label("Commit")),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    commit(window, cx);
                    true
                })
        });
        message.update(cx, |input, cx| input.focus(window, cx));
    }
}

/// Runs `action` from the commit dialog with its message (empty: generated).
fn commit_runner(
    control: WeakEntity<GitControl>,
    message: Entity<TextareaState>,
    action: GitStackedAction,
) -> impl Fn(&mut Window, &mut App) + 'static {
    move |window, cx| {
        let text = message.read(cx).value().trim().to_owned();
        let commit_message = (!text.is_empty()).then_some(text);
        let _ = control.update(cx, |this, cx| {
            this.run_action(action, commit_message, window, cx)
        });
    }
}

/// The commit dialog's body: branch, files with their line counts, totals
/// and the message.
fn commit_body(status: &VcsStatusResult, message: &Entity<TextareaState>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let counts = |insertions: u64, deletions: u64| {
        h_flex()
            .flex_shrink_0()
            .child(
                div()
                    .text_color(theme.success)
                    .child(format!("+{insertions}")),
            )
            .child(div().text_color(theme.muted_foreground).child(" / "))
            .child(
                div()
                    .text_color(theme.danger)
                    .child(format!("−{deletions}")),
            )
    };
    let files = &status.working_tree.files;
    let file_list = if files.is_empty() {
        div().font_medium().child("none").into_any_element()
    } else {
        v_flex()
            .gap_1()
            .child(
                v_flex()
                    .id("git-commit-files")
                    .test_support()
                    .max_h(rems(11.))
                    .overflow_y_scroll()
                    .p_1()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .children(files.iter().map(|file| {
                        h_flex()
                            .justify_between()
                            .gap_3()
                            .px_2()
                            .py_0p5()
                            .child(div().min_w_0().truncate().child(file.path.clone()))
                            .child(counts(file.insertions, file.deletions))
                    })),
            )
            .child(
                h_flex()
                    .justify_between()
                    .text_xs()
                    .child(div().text_color(theme.muted_foreground).child(format!(
                        "{} {}",
                        files.len(),
                        if files.len() == 1 { "file" } else { "files" }
                    )))
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(counts(
                                status.working_tree.insertions,
                                status.working_tree.deletions,
                            )),
                    ),
            )
            .into_any_element()
    };
    v_flex()
        .gap_3()
        .text_sm()
        .child(
            h_flex()
                .gap_2()
                .child(div().text_color(theme.muted_foreground).child("Branch"))
                .child(
                    div().font_medium().child(
                        status
                            .ref_name
                            .as_deref()
                            .unwrap_or("(detached HEAD)")
                            .to_owned(),
                    ),
                ),
        )
        .child(
            v_flex()
                .gap_1()
                .child(div().text_color(theme.muted_foreground).child("Files"))
                .child(file_list),
        )
        .child(
            v_flex()
                .gap_1()
                .child(div().font_medium().child("Commit message (optional)"))
                .child(Textarea::new(message)),
        )
        .into_any_element()
}

/// The button's label before the server names the first phase.
fn first_phase(action: GitStackedAction, generates: bool) -> &'static str {
    match (action.commits(), generates) {
        (true, true) => "Generating commit message...",
        (true, false) => "Committing...",
        (false, _) => "Pushing...",
    }
}

/// The server's success toast. Its call to action becomes the notification's
/// button: "View PR" opens the pull request, and a `run_action` follow-up
/// (Push after Commit, Create PR after a push) runs.
fn notify_toast(
    toast: GitToast,
    control: WeakEntity<GitControl>,
    window: &mut Window,
    cx: &mut App,
) {
    enum FollowUp {
        Run(GitStackedAction),
        Open(SharedString),
    }
    let follow_up = match toast.cta {
        GitToastCta::RunAction { label, action } => {
            Some((SharedString::from(label), FollowUp::Run(action.kind)))
        }
        GitToastCta::OpenPr { label, url } => {
            Some((SharedString::from(label), FollowUp::Open(url.into())))
        }
        GitToastCta::None => None,
    };
    window.push_notification(
        Notification::new()
            .title(toast.title)
            .when_some(toast.description, |note, description| {
                note.message(description)
            })
            .with_type(NotificationType::Success)
            .when_some(follow_up, |note, (label, follow_up)| {
                let follow_up = Rc::new(follow_up);
                note.action(move |_, _, cx| {
                    let (control, note, follow_up) =
                        (control.clone(), cx.entity().downgrade(), follow_up.clone());
                    Button::new("git-toast-action")
                        .small()
                        .primary()
                        .label(label.clone())
                        .on_click(move |_, window, cx| {
                            let _ = note.update(cx, |note, cx| note.dismiss(window, cx));
                            match &*follow_up {
                                FollowUp::Run(action) => {
                                    let _ = control.update(cx, |this, cx| {
                                        this.run_action(*action, None, window, cx)
                                    });
                                }
                                FollowUp::Open(url) => cx.open_url(url),
                            }
                        })
                })
            }),
        cx,
    );
}

fn notify_error(error: j3_client::Error, window: &mut Window, cx: &mut App) {
    window.push_notification(
        Notification::new()
            .message(error.to_string())
            .with_type(NotificationType::Error),
        cx,
    );
}

impl Render for GitControl {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.loaded && self.status.as_ref().is_none_or(|status| status.is_repo);
        if !shown {
            return div().into_any_element();
        }
        let busy = self.busy();
        let quick = git::quick_action(self.status.as_ref(), busy);
        let menu = git::menu(self.status.as_ref(), busy);
        let control = cx.weak_entity();
        let primary = {
            let control = control.clone();
            Button::new("git-quick-action")
                .label(
                    self.running
                        .clone()
                        .unwrap_or_else(|| SharedString::from(quick.label)),
                )
                .disabled(quick.disabled)
                .when(busy, |button| {
                    button.icon(IconName::LoaderCircle).loading(true)
                })
                .when_some(
                    match quick.kind {
                        QuickActionKind::Hint(hint) if !busy => Some(hint),
                        _ => None,
                    },
                    |button, hint| button.tooltip(hint),
                )
                .on_click(move |_, window, cx| {
                    let _ = control.update(cx, |this, cx| match quick.kind {
                        QuickActionKind::Run(action) => this.run_action(action, None, window, cx),
                        QuickActionKind::Pull => this.pull(window, cx),
                        QuickActionKind::OpenPr => this.open_pr(cx),
                        QuickActionKind::Hint(_) => {}
                    });
                })
        };
        DropdownButton::new("git-control")
            .button(primary)
            .outline()
            .xsmall()
            .menu_accessibility_label("Git actions")
            .dropdown_menu(move |popup, _, _| {
                let item = |label: &'static str, enabled: bool, run: fn(&mut GitControl, &mut Window, &mut Context<GitControl>)| {
                    let control = control.clone();
                    PopupMenuItem::new(label)
                        .disabled(!enabled)
                        .on_click(move |_, window, cx| {
                            let _ = control.update(cx, |this, cx| run(this, window, cx));
                        })
                };
                popup
                    .item(item("Commit…", menu.commit, |this, window, cx| {
                        this.open_commit_dialog(window, cx)
                    }))
                    .item(item("Push", menu.push, |this, window, cx| {
                        this.run_action(GitStackedAction::Push, None, window, cx)
                    }))
                    .item(item("Pull", menu.pull, GitControl::pull))
                    .when(menu.view_pr || menu.create_pr, |popup| {
                        popup.separator().item(if menu.view_pr {
                            item("View PR", true, |this, _, cx| this.open_pr(cx))
                        } else {
                            item("Create PR", true, |this, window, cx| {
                                this.run_action(GitStackedAction::CreatePr, None, window, cx)
                            })
                        })
                    })
            })
            .into_any_element()
    }
}
