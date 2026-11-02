//! One open thread: header, streaming feed, todo card, queued messages, the
//! pending question, the working indicator and the composer.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    message_scroller::{MessageScroller, MessageScrollerState},
    notification::{Notification, NotificationType},
    questionnaire::{
        Questionnaire, QuestionnaireActions, QuestionnaireChoice, QuestionnaireChoiceDefinition,
        QuestionnaireChoices, QuestionnaireDescription, QuestionnaireError, QuestionnaireEvent,
        QuestionnaireInput, QuestionnaireInputDefinition, QuestionnaireItem,
        QuestionnaireItemDefinition, QuestionnaireNext, QuestionnairePrevious,
        QuestionnaireProgress, QuestionnaireState, QuestionnaireSubmission, QuestionnaireSubmit,
        QuestionnaireTitle,
    },
    spinner::Spinner,
    text::TextViewState,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    div, rems,
};
use j3_client::{
    Answer, SendMode, ThreadHandle, ThreadState, ThreadSync,
    t3::orchestration::{
        PlanStepStatus, ProviderGoal, ProviderGoalStatus, TurnItemKind, UserInputQuestion,
    },
};

use crate::{
    actions::{ArchiveThread, DeleteThread, RenameThread},
    agent_hub::{AgentHub, CloseHub},
    composer::{Composer, ComposerEvent, RunState, Submission},
    environment::Environment,
    feed::is_hidden,
    feed_view::{RowContext, render_item},
    git_control::GitControl,
    preferences::Preferences,
    titlebar,
};

struct QuestionCard {
    request_id: String,
    questions: Vec<UserInputQuestion>,
    /// Choice values per question id, in order.
    values: Vec<(String, Vec<String>, bool)>,
    state: Option<Entity<QuestionnaireState>>,
    schema_error: Option<SharedString>,
    dismissible: bool,
    _subscription: Option<Subscription>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoalCommand {
    Pause,
    Resume,
    Drop,
}

struct QueueEdit {
    run_id: String,
    input: Entity<InputState>,
    _subscription: Subscription,
}

pub struct ThreadView {
    environment: Entity<Environment>,
    /// The window's preferences, for the text measure.
    preferences: Entity<Preferences>,
    handle: ThreadHandle,
    state: Arc<ThreadState>,
    /// Indices into `state.feed()` of the rows shown.
    visible: Rc<Vec<usize>>,
    scroller: Entity<MessageScrollerState>,
    markdown: HashMap<String, (Entity<TextViewState>, String)>,
    markdown_snapshot: Rc<HashMap<String, Entity<TextViewState>>>,
    expanded: Rc<HashSet<String>>,
    composer: Entity<Composer>,
    git: Entity<GitControl>,
    question: Option<QuestionCard>,
    editing: Option<QueueEdit>,
    todo_open: bool,
    /// The agents hub, shown instead of the feed and composer.
    hub: Option<(Entity<AgentHub>, Subscription)>,
    _watch: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ThreadView {
    pub fn new(
        environment: Entity<Environment>,
        preferences: Entity<Preferences>,
        thread_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (handle, cwd, preferred) = {
            let env = environment.read(cx);
            let shell = &env.state().shell;
            let thread = shell.thread(thread_id);
            let cwd = thread
                .and_then(|thread| shell.project(&thread.project_id))
                .map(|project| project.workspace_root.clone())
                .unwrap_or_default();
            (
                env.connection().thread(thread_id),
                cwd,
                thread.map(|thread| thread.model_selection.clone()),
            )
        };
        let mut states = handle.state();
        let state = states.borrow_and_update().clone();
        let watch = cx.spawn_in(window, async move |this, cx| {
            while states.changed().await.is_ok() {
                let state = states.borrow_and_update().clone();
                if this
                    .update_in(cx, |this, window, cx| this.on_state(state, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let composer = cx.new(|cx| Composer::new(environment.clone(), cwd, preferred, window, cx));
        let git = cx.new(|cx| GitControl::new(environment.clone(), thread_id, cx));
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let subscriptions = vec![
            cx.subscribe_in(&composer, window, Self::on_composer_event),
            cx.observe(&environment, |_, _, cx| cx.notify()),
            cx.observe(&preferences, |_, _, cx| cx.notify()),
        ];
        let connection = environment.read(cx).connection().clone();
        let visited = thread_id.to_owned();
        cx.spawn(async move |_, _| {
            if let Err(e) = connection.visit(&visited).await {
                tracing::debug!("visit {visited}: {e}");
            }
        })
        .detach();

        let mut view = Self {
            environment,
            preferences,
            handle,
            state: Arc::new(ThreadState::clone(&state)),
            visible: Rc::default(),
            scroller,
            markdown: HashMap::new(),
            markdown_snapshot: Rc::default(),
            expanded: Rc::default(),
            composer,
            git,
            question: None,
            editing: None,
            todo_open: true,
            hub: None,
            _watch: watch,
            _subscriptions: subscriptions,
        };
        view.on_state(state, window, cx);
        view
    }

    pub fn thread_id(&self) -> &str {
        self.handle.thread_id()
    }

    pub fn environment(&self) -> &Entity<Environment> {
        &self.environment
    }

    pub fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        self.composer
            .update(cx, |composer, cx| composer.focus(window, cx));
    }

    pub fn toggle_expanded(&mut self, id: &str, cx: &mut Context<Self>) {
        let mut expanded = HashSet::clone(&self.expanded);
        if !expanded.remove(id) {
            expanded.insert(id.to_owned());
        }
        self.expanded = Rc::new(expanded);
        let feed = self.state.feed();
        if let Some(row) = self.visible.iter().position(|&ix| feed[ix].item.id == id) {
            self.scroller.update(cx, |scroller, cx| {
                scroller.remeasure_items(row..row + 1, cx)
            });
        }
        cx.notify();
    }

    fn connection(&self, cx: &App) -> j3_client::Connection {
        self.environment.read(cx).connection().clone()
    }

    /// Stops the active run.
    pub fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.active_run().is_none() {
            return;
        }
        let connection = self.connection(cx);
        let thread_id = self.thread_id().to_owned();
        self.run_action(window, cx, async move {
            connection.stop(&thread_id).await.map(drop)
        });
    }

    /// Runs a thread command; a failure (often omp's reason, such as a
    /// goal already being active) shows as a notification.
    fn run_action(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        action: impl Future<Output = Result<(), j3_client::Error>> + 'static,
    ) {
        cx.spawn_in(window, async move |_, cx| {
            if let Err(e) = action.await {
                let _ = cx.update(|window, cx| {
                    window.push_notification(
                        Notification::new()
                            .message(e.to_string())
                            .with_type(NotificationType::Error),
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    pub fn stop_subagent(
        &mut self,
        run_id: String,
        subagent_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connection = self.connection(cx);
        let thread_id = self.thread_id().to_owned();
        self.run_action(window, cx, async move {
            connection
                .stop_subagent(&thread_id, &run_id, &subagent_id)
                .await
                .map(drop)
        });
    }

    fn goal_command(&mut self, command: GoalCommand, window: &mut Window, cx: &mut Context<Self>) {
        let connection = self.connection(cx);
        let thread_id = self.thread_id().to_owned();
        self.run_action(window, cx, async move {
            match command {
                GoalCommand::Pause => connection.pause_goal(&thread_id).await,
                GoalCommand::Resume => connection.resume_goal(&thread_id).await,
                GoalCommand::Drop => connection.drop_goal(&thread_id).await,
            }
            .map(drop)
        });
    }

    /// Asks for an objective and optional token budget, then creates the goal.
    pub fn open_goal_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let objective = cx.new(|cx| {
            InputState::new(window, cx).placeholder("What should omp keep working towards?")
        });
        let budget = cx.new(|cx| InputState::new(window, cx).placeholder("No limit"));
        let view = cx.weak_entity();
        let (dialog_objective, dialog_budget) = (objective.clone(), budget.clone());
        window.open_dialog(cx, move |dialog, _, _| {
            let (objective, budget, view) = (
                dialog_objective.clone(),
                dialog_budget.clone(),
                view.clone(),
            );
            dialog
                .title("Set goal")
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().font_medium().child("Objective"))
                                .child(Input::new(&dialog_objective).small()),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().font_medium().child("Token budget"))
                                .child(Input::new(&dialog_budget).small()),
                        ),
                )
                .footer(crate::dialogs::footer("Set Goal"))
                .on_ok(move |_, window, cx| {
                    let text = objective.read(cx).value().trim().to_owned();
                    let Ok(token_budget) = parse_budget(&budget.read(cx).value()) else {
                        return false;
                    };
                    if text.is_empty() {
                        return false;
                    }
                    let _ = view.update(cx, |this, cx| {
                        let connection = this.connection(cx);
                        let thread_id = this.thread_id().to_owned();
                        this.run_action(window, cx, async move {
                            connection
                                .create_goal(&thread_id, &text, token_budget)
                                .await
                                .map(drop)
                        });
                    });
                    true
                })
        });
        objective.update(cx, |input, cx| input.focus(window, cx));
    }

    fn on_state(&mut self, state: Arc<ThreadState>, window: &mut Window, cx: &mut Context<Self>) {
        let feed = state.feed();
        let visible: Vec<usize> = feed
            .iter()
            .enumerate()
            .filter(|(_, row)| !is_hidden(&row.item))
            .map(|(ix, _)| ix)
            .collect();

        // Keep the virtual list's measurements in step with the rows.
        let old_feed = self.state.feed();
        let old_ids = self.visible.iter().map(|&ix| old_feed[ix].item.id.as_str());
        let new_ids = visible.iter().map(|&ix| feed[ix].item.id.as_str());
        let common = old_ids
            .clone()
            .zip(new_ids.clone())
            .take_while(|(a, b)| a == b)
            .count();
        let (old_len, new_len) = (self.visible.len(), visible.len());
        let changed: Vec<usize> = (0..common)
            .filter(|&row| old_feed[self.visible[row]].item != feed[visible[row]].item)
            .collect();
        self.scroller.update(cx, |scroller, cx| {
            if common == old_len {
                scroller.append(new_len - old_len, cx);
            } else {
                scroller.splice(common..old_len, new_len - common, cx);
            }
            for row in changed {
                scroller.remeasure_items(row..row + 1, cx);
            }
        });

        // Markdown states for assistant text and reasoning, updated in place
        // so streaming appends instead of re-parsing.
        let mut live = HashSet::new();
        for &ix in &visible {
            let item = &feed[ix].item;
            let text = match &item.kind {
                TurnItemKind::AssistantMessage(message) => &message.text,
                TurnItemKind::Reasoning(reasoning) => &reasoning.text,
                _ => continue,
            };
            live.insert(item.id.clone());
            match self.markdown.get_mut(&item.id) {
                Some((entity, last)) if last != text => {
                    match text.strip_prefix(last.as_str()) {
                        Some(delta) => entity.update(cx, |state, cx| state.push_str(delta, cx)),
                        None => entity.update(cx, |state, cx| state.set_text(text, cx)),
                    }
                    *last = text.clone();
                }
                Some(_) => {}
                None => {
                    let entity = cx.new(|cx| TextViewState::markdown(text, cx));
                    self.markdown
                        .insert(item.id.clone(), (entity, text.clone()));
                }
            }
        }
        self.markdown.retain(|id, _| live.contains(id));
        self.markdown_snapshot = Rc::new(
            self.markdown
                .iter()
                .map(|(id, (entity, _))| (id.clone(), entity.clone()))
                .collect(),
        );

        // The first pending question gets the card above the composer.
        let pending = state.pending_user_inputs();
        let first = pending.first();
        let current = self.question.as_ref().map(|card| card.request_id.as_str());
        if first.map(|p| p.request.id.as_str()) != current {
            self.question = first.map(|pending| {
                Self::question_card(
                    pending.request.id.clone(),
                    pending.questions.to_vec(),
                    pending.dismissible,
                    window,
                    cx,
                )
            });
        }

        if let Some(edit) = &self.editing
            && !state
                .queued_runs()
                .iter()
                .any(|queued| queued.run.id == edit.run_id)
        {
            self.editing = None;
        }

        // A finished run may have changed files: the git control rereads.
        if self.state.active_run().is_some() && state.active_run().is_none() {
            self.git.update(cx, |git, cx| git.refresh(cx));
        }

        let run_state = match state.active_run() {
            Some(_) => RunState::Running,
            None => RunState::Idle,
        };
        self.composer.update(cx, |composer, cx| {
            composer.set_run_state(run_state, window, cx)
        });

        self.visible = Rc::new(visible);
        if let Some((hub, _)) = &self.hub {
            let state = state.clone();
            hub.update(cx, |hub, cx| hub.set_state(state, cx));
        }
        self.state = state;
        cx.notify();
    }

    fn question_card(
        request_id: String,
        questions: Vec<UserInputQuestion>,
        dismissible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> QuestionCard {
        let mut values = Vec::new();
        let mut items = Vec::new();
        for question in &questions {
            let choice_values: Vec<String> = question
                .options
                .iter()
                .map(|option| option.value.clone().unwrap_or_else(|| option.label.clone()))
                .collect();
            let custom = question.allow_custom_answer == Some(true) || question.options.is_empty();
            let mut item =
                QuestionnaireItemDefinition::new(question.id.clone(), question.question.clone())
                    .with_description(question.header.clone())
                    .with_multiple(question.multi_select == Some(true))
                    .with_required(question.required != Some(false))
                    .with_choices(question.options.iter().zip(&choice_values).map(
                        |(option, value)| {
                            let choice = QuestionnaireChoiceDefinition::new(
                                value.clone(),
                                option.label.clone(),
                            );
                            if option.description.trim().is_empty()
                                || option.description == option.label
                            {
                                choice
                            } else {
                                choice.with_description(option.description.clone())
                            }
                        },
                    ));
            if custom {
                let initial = question.initial_answer.clone();
                let input = cx.new(|cx| {
                    let input = InputState::new(window, cx).placeholder("Type an answer…");
                    match initial {
                        Some(initial) => input.default_value(initial),
                        None => input,
                    }
                });
                item = item.with_input(QuestionnaireInputDefinition::new(input, "Custom answer"));
            }
            values.push((question.id.clone(), choice_values, custom));
            items.push(item);
        }
        let schema_error = schema_problem(&values);
        let (state, subscription) = if schema_error.is_some() {
            (None, None)
        } else {
            let state = cx.new(|cx| {
                QuestionnaireState::new(items, cx)
                    .expect("schema_problem rules out every schema error j3 can produce")
            });
            let subscription = cx.subscribe_in(
                &state,
                window,
                |this, _, event: &QuestionnaireEvent, window, cx| {
                    if let QuestionnaireEvent::Submit(submission) = event {
                        this.answer_question(submission, window, cx);
                    }
                },
            );
            (Some(state), Some(subscription))
        };
        let schema_error = schema_error.map(SharedString::from);
        QuestionCard {
            request_id,
            questions,
            values,
            state,
            schema_error,
            dismissible,
            _subscription: subscription,
        }
    }

    fn answer_question(
        &mut self,
        submission: &QuestionnaireSubmission,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(card) = &self.question else {
            return;
        };
        let answers = build_answers(&card.questions, |id| {
            submission.answer(id).map(|answer| {
                (
                    answer.choices().iter().map(ToString::to_string).collect(),
                    answer.freeform().map(ToString::to_string),
                )
            })
        });
        let connection = self.connection(cx);
        let thread_id = self.thread_id().to_owned();
        let request_id = card.request_id.clone();
        self.run_action(window, cx, async move {
            connection
                .answer(&thread_id, &request_id, answers)
                .await
                .map(drop)
        });
    }

    fn dismiss_question(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = &self.question else {
            return;
        };
        let connection = self.connection(cx);
        let thread_id = self.thread_id().to_owned();
        let request_id = card.request_id.clone();
        self.run_action(window, cx, async move {
            connection.dismiss(&thread_id, &request_id).await.map(drop)
        });
    }

    fn on_composer_event(
        &mut self,
        _: &Entity<Composer>,
        event: &ComposerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ComposerEvent::Stop => self.stop(window, cx),
            ComposerEvent::Submit(submission) => self.send(submission.clone(), window, cx),
            ComposerEvent::OpenAgentHub => self.open_hub(window, cx),
        }
    }

    /// Opens the agents hub; like omp, only when there are subagents.
    pub fn open_hub(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.hub.is_some() || self.state.subagents().is_empty() {
            return;
        }
        let (environment, thread_id, state) = (
            self.environment.clone(),
            self.thread_id().to_owned(),
            self.state.clone(),
        );
        let hub = cx.new(|cx| AgentHub::new(environment, thread_id, state, window, cx));
        let subscription = cx.subscribe_in(&hub, window, |this, _, _: &CloseHub, window, cx| {
            this.hub = None;
            this.focus_composer(window, cx);
            cx.notify();
        });
        hub.update(cx, |hub, cx| hub.focus(window, cx));
        self.hub = Some((hub, subscription));
        cx.notify();
    }

    fn send(&mut self, submission: Submission, window: &mut Window, cx: &mut Context<Self>) {
        // Messages sent into a running thread wait their turn; the queue
        // panel's Steer sends one into the run.
        let mode = match self.state.active_run() {
            None => SendMode::Start,
            Some(_) => SendMode::Queue,
        };
        let connection = self.connection(cx);
        let thread_id = self.thread_id().to_owned();
        let composer = self.composer.clone();
        cx.spawn_in(window, async move |_, cx| {
            let result = connection
                .send(
                    &thread_id,
                    &submission.text,
                    submission.attachments,
                    Some(submission.model),
                    mode,
                )
                .await
                .map(drop)
                .map_err(|e| e.to_string());
            let _ = composer.update_in(cx, |composer, window, cx| {
                composer.finish(result, window, cx)
            });
        })
        .detach();
    }

    fn start_editing(
        &mut self,
        run_id: String,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(text.to_owned()));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let text = input.read(cx).value().to_string();
                    this.save_edit(text, window, cx);
                }
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        self.editing = Some(QueueEdit {
            run_id,
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn save_edit(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.editing.take() else {
            return;
        };
        let connection = self.connection(cx);
        let thread_id = self.thread_id().to_owned();
        self.run_action(window, cx, async move {
            connection
                .edit_queued(&thread_id, &edit.run_id, &text)
                .await
                .map(drop)
        });
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let env = self.environment.read(cx);
        let shell = &env.state().shell;
        let thread = shell.thread(self.thread_id());
        let title: SharedString = self
            .state
            .projection
            .as_ref()
            .map(|p| p.thread.title.clone())
            .or_else(|| thread.map(|t| t.title.clone()))
            .unwrap_or_else(|| "Thread".to_owned())
            .into();
        let location: SharedString = thread
            .and_then(|t| shell.project(&t.project_id))
            .map(|project| format!("{} · {}", env.label(), project.title))
            .unwrap_or_else(|| env.label().to_string())
            .into();
        let theme = cx.theme();
        let has_goal = self.state.goal().is_some();
        let view = cx.weak_entity();
        h_flex()
            .flex_shrink_0()
            .h(rems(titlebar::ROW_HEIGHT))
            .px_4()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_medium()
                    .text_sm()
                    .child(title),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(location),
            )
            .child(div().flex_1())
            .child(self.git.clone())
            .child(
                Button::new("thread-actions")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Ellipsis)
                    .tooltip("Thread actions")
                    .dropdown_menu(move |menu, _, _| {
                        let menu = menu.menu("Rename…", Box::new(RenameThread));
                        // A goal is created from the actions menu; the header
                        // stays quiet, and one goal at a time is enough.
                        let menu = if has_goal {
                            menu
                        } else {
                            let view = view.clone();
                            menu.item(PopupMenuItem::new("Set goal…").on_click(
                                move |_, window, cx| {
                                    let _ = view
                                        .update(cx, |this, cx| this.open_goal_dialog(window, cx));
                                },
                            ))
                        };
                        menu.menu("Archive", Box::new(ArchiveThread))
                            .separator()
                            .menu("Delete…", Box::new(DeleteThread))
                    }),
            )
            .into_any_element()
    }

    fn render_feed(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let placeholder = |text: SharedString| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        match &self.state.sync {
            ThreadSync::Deleted => return placeholder("This thread was deleted.".into()),
            ThreadSync::Failed(error) if self.state.projection.is_none() => {
                return placeholder(format!("Couldn’t load the thread: {error}").into());
            }
            ThreadSync::Loading => {
                return div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Spinner::new())
                    .into_any_element();
            }
            _ => {}
        }
        if self.visible.is_empty() {
            return placeholder("No messages yet.".into());
        }
        let state = self.state.clone();
        let visible = self.visible.clone();
        let measure = self.preferences.read(cx).ui().measure();
        let row = Rc::new(RowContext {
            markdown: self.markdown_snapshot.clone(),
            expanded: self.expanded.clone(),
            view: cx.weak_entity(),
            environment: self.environment.clone(),
            state: self.state.clone(),
        });
        MessageScroller::new("feed", self.scroller.clone(), move |ix, window, cx| {
            let feed = state.feed();
            let item = &feed[visible[ix]].item;
            let previous = ix.checked_sub(1).map(|prev| &feed[visible[prev]].item);
            h_flex()
                .w_full()
                .justify_center()
                .when(ix == 0, |this| this.pt_2())
                .when(ix + 1 == visible.len(), |this| this.pb_2())
                .child(
                    div()
                        .w_full()
                        .max_w(rems(measure))
                        .child(render_item(item, previous, &row, window, cx)),
                )
        })
        // `render_item` spaces rows itself (tool calls sit close together),
        // so drop the scroller's fixed gap between rows.
        .with_row_style(gpui_kit::StyleRefinement::default().pb_0())
        // No vertical list padding: the first and last rows carry it. A
        // wheel or trackpad scroll goes through the list's scrollbar offset,
        // which leaves the padding out of its range: with padding, scrolling
        // down at the bottom moved the list up by it, cut the last row off
        // and stopped following the reply.
        .with_list_style(gpui_kit::StyleRefinement::default().py_0())
        .size_full()
        .into_any_element()
    }

    fn render_todo(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let todo = self.state.latest_todo()?;
        let done = todo
            .steps
            .iter()
            .filter(|step| step.status == PlanStepStatus::Completed)
            .count();
        if todo.steps.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let open = self.todo_open;
        Some(
            v_flex()
                .gap_1()
                .px_3()
                .py_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .id("todo-toggle")
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(
                            Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .xsmall(),
                        )
                        .child(div().font_medium().child("Plan"))
                        .child(format!("{done}/{} done", todo.steps.len()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.todo_open = !this.todo_open;
                            cx.notify();
                        })),
                )
                .when(open, |this| {
                    this.children(todo.steps.iter().map(|step| {
                        let (icon, color) = match step.status {
                            PlanStepStatus::Completed => (IconName::CircleCheck, theme.success),
                            PlanStepStatus::Running => (IconName::LoaderCircle, theme.primary),
                            PlanStepStatus::Pending => (IconName::Minus, theme.muted_foreground),
                        };
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .child(Icon::new(icon).xsmall().text_color(color))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .when(step.status == PlanStepStatus::Completed, |this| {
                                        this.line_through().text_color(theme.muted_foreground)
                                    })
                                    .child(step.text.clone()),
                            )
                    }))
                })
                .into_any_element(),
        )
    }

    fn render_queue(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let queued = self.state.queued_runs();
        if queued.is_empty() {
            return None;
        }
        let held = self.state.queue_held();
        let can_steer = self.state.can_steer();
        let active_run = self.state.active_run().map(|run| run.id.clone());
        let thread_id = self.thread_id().to_owned();
        let theme = cx.theme();
        let rows = queued.iter().map(|queued| {
            let run_id = queued.run.id.clone();
            let text = queued.text.to_owned();
            let editing = self
                .editing
                .as_ref()
                .filter(|edit| edit.run_id == run_id)
                .map(|edit| edit.input.clone());
            let action = |label: &'static str, id: &str| {
                Button::new(SharedString::from(format!("{id}-{run_id}")))
                    .xsmall()
                    .ghost()
                    .label(label)
            };
            let body = match editing {
                Some(input) => h_flex()
                    .flex_1()
                    .gap_1()
                    .child(div().flex_1().child(Input::new(&input).xsmall()))
                    .child(action("Save", "save").on_click(cx.listener({
                        let input = input.clone();
                        move |this, _, window, cx| {
                            let text = input.read(cx).value().to_string();
                            this.save_edit(text, window, cx);
                        }
                    })))
                    .child(action("Cancel", "cancel-edit").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.editing = None;
                            cx.notify();
                        },
                    )))
                    .into_any_element(),
                None => h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(text.clone()),
                    )
                    .when_some(active_run.clone().filter(|_| can_steer), |this, target| {
                        let connection = self.connection(cx);
                        let thread_id = thread_id.clone();
                        let run_id = run_id.clone();
                        this.child(
                            action("Steer", "steer")
                                .tooltip("Send into the running turn")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let (connection, thread_id, run_id, target) = (
                                        connection.clone(),
                                        thread_id.clone(),
                                        run_id.clone(),
                                        target.clone(),
                                    );
                                    this.run_action(window, cx, async move {
                                        connection
                                            .promote_to_steer(&thread_id, &run_id, &target)
                                            .await
                                            .map(drop)
                                    });
                                })),
                        )
                    })
                    .child({
                        let connection = self.connection(cx);
                        let thread_id = thread_id.clone();
                        let run_id = run_id.clone();
                        action("Send now", "send-now")
                            .tooltip("Stop the current run and start this one")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let (connection, thread_id, run_id) =
                                    (connection.clone(), thread_id.clone(), run_id.clone());
                                this.run_action(window, cx, async move {
                                    connection.start_queued(&thread_id, &run_id).await.map(drop)
                                });
                            }))
                    })
                    .child({
                        let run_id = run_id.clone();
                        let text = text.clone();
                        action("Edit", "edit").on_click(cx.listener(move |this, _, window, cx| {
                            this.start_editing(run_id.clone(), &text, window, cx);
                        }))
                    })
                    .child({
                        let connection = self.connection(cx);
                        let thread_id = thread_id.clone();
                        let run_id = run_id.clone();
                        Button::new(SharedString::from(format!("cancel-{run_id}")))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip("Remove from queue")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let (connection, thread_id, run_id) =
                                    (connection.clone(), thread_id.clone(), run_id.clone());
                                this.run_action(window, cx, async move {
                                    connection
                                        .cancel_queued(&thread_id, &run_id)
                                        .await
                                        .map(drop)
                                });
                            }))
                    })
                    .into_any_element(),
            };
            h_flex()
                .id(SharedString::from(format!("queued-{run_id}")))
                .test_support()
                .aria_label(text)
                .gap_2()
                .child(
                    Icon::new(IconName::Inbox)
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .child(body)
        });
        let rows: Vec<_> = rows.collect();
        let connection = self.connection(cx);
        Some(
            v_flex()
                .gap_1()
                .px_3()
                .py_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(div().font_medium().child("Queued"))
                        .when(held, |this| {
                            this.child("Paused").child(div().flex_1()).child(
                                Button::new("resume-queue")
                                    .xsmall()
                                    .outline()
                                    .icon(IconName::Play)
                                    .label("Resume")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let (connection, thread_id) =
                                            (connection.clone(), thread_id.clone());
                                        this.run_action(window, cx, async move {
                                            connection.resume_queue(&thread_id).await.map(drop)
                                        });
                                    })),
                            )
                        }),
                )
                .children(rows)
                .into_any_element(),
        )
    }

    fn render_question(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let card = self.question.as_ref()?;
        let theme = cx.theme();
        let header = h_flex()
            .gap_2()
            .text_xs()
            .text_color(theme.warning)
            .child(Icon::new(IconName::Bell).xsmall())
            .child(div().font_medium().child("omp is asking"))
            .child(div().flex_1())
            .when(card.dismissible, |this| {
                this.child(
                    Button::new("dismiss-question")
                        .xsmall()
                        .ghost()
                        .label("Dismiss")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.dismiss_question(window, cx)),
                        ),
                )
            });
        let body = match (&card.state, &card.schema_error) {
            (Some(state), _) => {
                let mut questionnaire = Questionnaire::new(state).small();
                if card.values.len() > 1 {
                    questionnaire = questionnaire.child(QuestionnaireProgress::new(state));
                }
                for (id, values, custom) in &card.values {
                    let mut choices = QuestionnaireChoices::new(state, id.clone());
                    for value in values {
                        choices = choices.child(QuestionnaireChoice::new(
                            state,
                            id.clone(),
                            value.clone(),
                        ));
                    }
                    if *custom {
                        choices = choices.child(QuestionnaireInput::new(state, id.clone()));
                    }
                    questionnaire = questionnaire.child(
                        QuestionnaireItem::new(state, id.clone())
                            .child(QuestionnaireTitle::new(state, id.clone()))
                            .child(QuestionnaireDescription::new(state, id.clone()))
                            .child(choices)
                            .child(QuestionnaireError::new(state, id.clone())),
                    );
                }
                questionnaire
                    .child(
                        QuestionnaireActions::new(state)
                            .child(QuestionnairePrevious::new(state))
                            .child(QuestionnaireNext::new(state))
                            .child(QuestionnaireSubmit::new(state)),
                    )
                    .into_any_element()
            }
            (None, error) => div()
                .text_sm()
                .text_color(theme.danger)
                .child(format!(
                    "This question can’t be shown: {}",
                    error.clone().unwrap_or_default()
                ))
                .into_any_element(),
        };
        Some(
            v_flex()
                .id("question-card")
                .test_support()
                .gap_2()
                .px_3()
                .py_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.warning)
                .child(header)
                .child(body)
                .into_any_element(),
        )
    }

    fn render_activity(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let run = self.state.active_run()?;
        let theme = cx.theme();
        let waiting = self.question.is_some();
        let label = if waiting {
            "Waiting for your answer"
        } else {
            match run.status {
                j3_client::t3::orchestration::RunStatus::Preparing
                | j3_client::t3::orchestration::RunStatus::Starting => "Starting…",
                _ => "Working…",
            }
        };
        Some(
            h_flex()
                .gap_2()
                .px_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .when(!waiting, |this| this.child(Spinner::new().xsmall()))
                .child(label)
                .into_any_element(),
        )
    }

    fn render_goal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let goal = self.state.goal()?;
        let theme = cx.theme();
        let status = goal.status;
        let color = match status {
            ProviderGoalStatus::Active => theme.primary,
            ProviderGoalStatus::Complete => theme.success,
            ProviderGoalStatus::Blocked
            | ProviderGoalStatus::UsageLimited
            | ProviderGoalStatus::BudgetLimited => theme.warning,
            ProviderGoalStatus::Paused => theme.muted_foreground,
        };
        let usage = goal_usage(goal);
        let command = |id: &'static str, label: &'static str, command: GoalCommand| {
            Button::new(id).xsmall().ghost().label(label).on_click(
                cx.listener(move |this, _, window, cx| this.goal_command(command, window, cx)),
            )
        };
        Some(
            h_flex()
                .id("goal")
                .test_support()
                .aria_label(format!("{}, {}", goal.objective, goal_status_label(status)))
                .gap_2()
                .px_3()
                .py_1p5()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .text_sm()
                .child(Icon::new(IconName::Star).xsmall().text_color(color))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(goal.objective.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(color)
                        .child(goal_status_label(status)),
                )
                .when(!usage.is_empty(), |this| {
                    this.child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(usage),
                    )
                })
                .when(status == ProviderGoalStatus::Active, |this| {
                    this.child(command("goal-pause", "Pause", GoalCommand::Pause))
                })
                .when(
                    matches!(
                        status,
                        ProviderGoalStatus::Paused | ProviderGoalStatus::BudgetLimited
                    ),
                    |this| this.child(command("goal-resume", "Resume", GoalCommand::Resume)),
                )
                .child(command(
                    "goal-drop",
                    if status == ProviderGoalStatus::Complete {
                        "Clear"
                    } else {
                        "Drop"
                    },
                    GoalCommand::Drop,
                ))
                .into_any_element(),
        )
    }
}

/// The token budget typed in the goal dialog: empty means none.
fn parse_budget(text: &str) -> Result<Option<u64>, std::num::ParseIntError> {
    let digits: String = text
        .chars()
        .filter(|c| !matches!(c, ',' | '_' | ' '))
        .collect();
    if digits.is_empty() {
        Ok(None)
    } else {
        digits.parse().map(Some)
    }
}

/// A token count like `950`, `12.3k` or `1.2M`.
pub fn format_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}k", tokens as f64 / 1_000.),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.),
    }
}

/// What the goal card says about usage: tokens of budget and time.
fn goal_usage(goal: &ProviderGoal) -> String {
    let mut parts = Vec::new();
    match (goal.tokens_used, goal.token_budget.flatten()) {
        (Some(used), Some(budget)) => parts.push(format!(
            "{} / {} tokens",
            format_tokens(used),
            format_tokens(budget)
        )),
        (Some(used), None) => parts.push(format!("{} tokens", format_tokens(used))),
        (None, Some(budget)) => parts.push(format!("budget {} tokens", format_tokens(budget))),
        (None, None) => {}
    }
    if let Some(seconds) = goal.time_used_seconds.filter(|s| *s > 0) {
        parts.push(if seconds < 60 {
            format!("{seconds}s")
        } else {
            format!("{}m", seconds / 60)
        });
    }
    parts.join(" · ")
}

fn goal_status_label(status: ProviderGoalStatus) -> &'static str {
    match status {
        ProviderGoalStatus::Active => "Active",
        ProviderGoalStatus::Paused => "Paused",
        ProviderGoalStatus::Blocked => "Blocked",
        ProviderGoalStatus::UsageLimited => "Usage limited",
        ProviderGoalStatus::BudgetLimited => "Budget reached",
        ProviderGoalStatus::Complete => "Complete",
    }
}

/// Why questions (id, choice values, custom input) cannot become a
/// questionnaire: a repeated question id or a repeated choice value.
fn schema_problem(values: &[(String, Vec<String>, bool)]) -> Option<String> {
    let mut ids = HashSet::new();
    for (id, choices, _) in values {
        if !ids.insert(id) {
            return Some(format!("question “{id}” appears twice"));
        }
        let mut seen = HashSet::new();
        if let Some(dup) = choices.iter().find(|value| !seen.insert(*value)) {
            return Some(format!("question “{id}” offers “{dup}” twice"));
        }
    }
    None
}

/// Wire answers from questionnaire results: `answer_of(question id)` gives
/// the chosen values and any typed answer. Multi-select questions send all
/// values; single answers prefer the typed text.
pub fn build_answers(
    questions: &[UserInputQuestion],
    answer_of: impl Fn(&str) -> Option<(Vec<String>, Option<String>)>,
) -> BTreeMap<String, Answer> {
    let mut answers = BTreeMap::new();
    for question in questions {
        let Some((choices, freeform)) = answer_of(&question.id) else {
            continue;
        };
        let freeform = freeform.filter(|text| !text.trim().is_empty());
        let answer = if question.multi_select == Some(true) {
            let mut values = choices;
            values.extend(freeform);
            if values.is_empty() {
                continue;
            }
            Answer::Many(values)
        } else {
            match freeform.or_else(|| choices.into_iter().next()) {
                Some(value) => Answer::One(value),
                None => continue,
            }
        };
        answers.insert(question.id.clone(), answer);
    }
    answers
}

impl Focusable for ThreadView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus_handle(cx)
    }
}

impl Render for ThreadView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        if let Some((hub, _)) = &self.hub {
            return v_flex()
                .size_full()
                .bg(cx.theme().background)
                .child(header)
                .child(div().flex_1().min_h_0().child(hub.clone()));
        }
        let feed = self.render_feed(cx);
        let todo = self.render_todo(cx);
        let queue = self.render_queue(cx);
        let question = self.render_question(cx);
        let activity = self.render_activity(cx);
        let goal = self.render_goal(cx);
        let theme = cx.theme();
        let measure = self.preferences.read(cx).ui().measure();
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(header)
            .child(div().flex_1().min_h_0().pt_3().child(feed))
            .child(
                // Extra air between the last message and the composer.
                h_flex()
                    .w_full()
                    .justify_center()
                    .px_4()
                    .pt_4()
                    .pb_3()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(rems(measure))
                            .gap_2()
                            .children(todo)
                            .children(queue)
                            .children(question)
                            .children(activity)
                            .children(goal)
                            .child(self.composer.clone()),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(id: &str, multi: bool) -> UserInputQuestion {
        serde_json::from_value(serde_json::json!({
            "id": id, "header": "H", "question": "Q?", "options": [], "multiSelect": multi
        }))
        .unwrap()
    }

    #[test]
    fn answers_follow_question_shape() {
        let questions = [
            question("color", false),
            question("tools", true),
            question("skip", false),
        ];
        let answers = build_answers(&questions, |id| match id {
            "color" => Some((vec!["red".into()], None)),
            "tools" => Some((vec!["a".into(), "b".into()], Some("c".into()))),
            "skip" => Some((vec![], Some("  ".into()))),
            _ => None,
        });
        assert_eq!(
            answers,
            BTreeMap::from([
                ("color".to_owned(), Answer::One("red".into())),
                (
                    "tools".to_owned(),
                    Answer::Many(vec!["a".into(), "b".into(), "c".into()])
                ),
            ])
        );
        let typed = build_answers(&questions[..1], |_| {
            Some((vec!["red".into()], Some("teal".into())))
        });
        assert_eq!(typed["color"], Answer::One("teal".into()));
    }
}
