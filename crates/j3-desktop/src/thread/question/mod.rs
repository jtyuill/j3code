//! The question card (spec §5.2, item 3): omp's first open request (`ask`, `select`,
//! `confirm`, `input`, `editor`) as a questionnaire, one question at a time, above the
//! composer.
//!
//! The questionnaire follows the old gpui-kit `Questionnaire`: single-choice questions are
//! radios (picking one confirms it after a moment: the next question, or submit on the last),
//! multi-choice questions are checkboxes, and a question with `allowOther` (or no options)
//! has a free-form field, prefilled with its `default`, that, typed into, replaces a single
//! choice. Every question needs an answer: Next and Submit refuse an unanswered one ("Choose
//! an answer to continue."). Submitting maps the answers through [`build_answers`] to
//! `requests.answer`; Dismiss sends `requests.dismiss`.

mod view;

use std::collections::HashSet;

use j3_client::{
    entities::{Answer, Question, Request},
    methods::{AnswerParams, DismissParams},
};
use rune::{TextEdit, Tx};

pub use view::card;

use crate::{app::J3, sidebar::ThreadKey};

/// How long a picked single choice shows before it confirms (seconds).
const CONFIRM_DELAY: f64 = 0.15;

/// What an unanswered question says when Next or Submit is tried.
pub const MISSING: &str = "Choose an answer to continue.";

/// The card for one open request.
pub struct QuestionCard {
    pub request_id: String,
    questions: Vec<Question>,
    /// The questionnaire, or why these questions can't be one.
    pub form: Result<Questionnaire, String>,
}

impl QuestionCard {
    pub fn new(request: &Request) -> Self {
        Self {
            request_id: request.id.clone(),
            questions: request.questions.clone(),
            form: Questionnaire::new(&request.questions),
        }
    }
}

/// What a navigation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Nothing moved (an answer is missing, or there's nowhere to go).
    Stay,
    /// Another question is current.
    Moved,
    /// Every question is answered: send them.
    Submit,
}

/// One choice of a question; its label is also what the answer carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub label: String,
    /// Shown under the label unless empty or the same as it.
    pub description: Option<String>,
}

/// One question and its answer so far.
pub struct Item {
    pub id: String,
    pub title: String,
    pub description: String,
    pub choices: Vec<Choice>,
    pub multiple: bool,
    /// The free-form answer field.
    pub input: Option<TextEdit>,
    /// Chosen values, in the order they were picked.
    chosen: Vec<String>,
    /// Next or Submit was tried: the error follows the answer from now on.
    attempted: bool,
    /// Next or Submit found no answer.
    pub missing: bool,
}

impl Item {
    /// The item for `question`; its `default` is the chosen option it names, else the
    /// field's text.
    fn new(question: &Question) -> Self {
        let custom = question.allow_other || question.options.is_empty();
        let choices: Vec<Choice> = question
            .options
            .iter()
            .map(|option| Choice {
                value: option.label.clone(),
                label: option.label.clone(),
                description: option
                    .description
                    .clone()
                    .filter(|d| !d.trim().is_empty() && *d != option.label),
            })
            .collect();
        let default = question.default.as_deref().filter(|d| !d.is_empty());
        let picked = default.filter(|d| choices.iter().any(|choice| choice.value == *d));
        let typed = default.filter(|_| picked.is_none()).unwrap_or_default();
        Self {
            id: question.id.clone(),
            title: question.text.clone(),
            description: question.header.clone().unwrap_or_default(),
            choices,
            multiple: question.multiple,
            input: custom.then(|| TextEdit::new(typed)),
            chosen: picked.into_iter().map(str::to_owned).collect(),
            attempted: false,
            missing: false,
        }
    }

    pub fn is_chosen(&self, value: &str) -> bool {
        self.chosen.iter().any(|chosen| chosen == value)
    }

    /// The typed answer, when there is one.
    fn freeform(&self) -> Option<&str> {
        self.input
            .as_ref()
            .map(|input| input.text.as_str())
            .filter(|text| !text.trim().is_empty())
    }

    /// The chosen values in choice order and the typed answer. A single-choice question's
    /// typed answer replaces its choice.
    pub fn answer(&self) -> (Vec<String>, Option<String>) {
        let freeform = self.freeform().map(str::to_owned);
        let choices = if !self.multiple && freeform.is_some() {
            Vec::new()
        } else {
            self.choices
                .iter()
                .filter(|choice| self.is_chosen(&choice.value))
                .map(|choice| choice.value.clone())
                .collect()
        };
        (choices, freeform)
    }

    fn is_answered(&self) -> bool {
        let (choices, freeform) = self.answer();
        !choices.is_empty() || freeform.is_some()
    }

    /// Checks the answer; true when it may move on.
    fn validate(&mut self) -> bool {
        self.attempted = true;
        self.missing = !self.is_answered();
        !self.missing
    }

    /// The answer changed: an error shown keeps up with it.
    fn answer_changed(&mut self) {
        if self.attempted {
            self.validate();
        }
    }
}

/// The questions of one request, one shown at a time.
pub struct Questionnaire {
    pub items: Vec<Item>,
    current: usize,
    /// A picked single choice confirms at this clock time.
    confirm_at: Option<f64>,
}

impl Questionnaire {
    /// The questionnaire for `questions`, or why they can't be shown.
    pub fn new(questions: &[Question]) -> Result<Self, String> {
        let items: Vec<Item> = questions.iter().map(Item::new).collect();
        if let Some(problem) = schema_problem(&items) {
            return Err(problem);
        }
        Ok(Self {
            items,
            current: 0,
            confirm_at: None,
        })
    }

    pub fn current(&self) -> usize {
        self.current
    }

    pub fn is_last(&self) -> bool {
        self.current + 1 >= self.items.len()
    }

    /// A click on choice `value` of item `ix` at clock time `now`: a multiple-choice item
    /// toggles it; a single-choice item selects it (clearing the typed answer) and confirms
    /// after [`CONFIRM_DELAY`], or at once when it was already the answer.
    pub fn choose(&mut self, ix: usize, value: &str, now: f64) -> Step {
        let Some(item) = self.items.get_mut(ix) else {
            return Step::Stay;
        };
        if item.multiple {
            match item.chosen.iter().position(|chosen| chosen == value) {
                Some(at) => {
                    item.chosen.remove(at);
                }
                None => item.chosen.push(value.to_owned()),
            }
            item.answer_changed();
            return Step::Stay;
        }
        let before = item.answer();
        item.chosen = vec![value.to_owned()];
        if let Some(input) = &mut item.input {
            input.clear();
        }
        item.answer_changed();
        if ix != self.current {
            return Step::Stay;
        }
        if before == self.items[ix].answer() {
            self.confirm_current()
        } else {
            self.confirm_at = Some(now + CONFIRM_DELAY);
            Step::Stay
        }
    }

    /// Item `ix`'s field changed: typed text replaces a single choice.
    pub fn input_changed(&mut self, ix: usize) {
        self.confirm_at = None;
        if let Some(item) = self.items.get_mut(ix) {
            if !item.multiple && item.freeform().is_some() {
                item.chosen.clear();
            }
            item.answer_changed();
        }
    }

    /// When a scheduled confirm is due.
    pub fn confirm_at(&self) -> Option<f64> {
        self.confirm_at
    }

    /// Runs the scheduled confirm when `now` reached it.
    pub fn confirm_due(&mut self, now: f64) -> Option<Step> {
        let at = self.confirm_at?;
        (now >= at).then(|| {
            self.confirm_at = None;
            self.confirm_current()
        })
    }

    /// Enter in a field, or a single choice confirmed: the next question, or submit on the
    /// last.
    pub fn confirm_current(&mut self) -> Step {
        if self.is_last() {
            self.submit()
        } else {
            self.advance()
        }
    }

    pub fn previous(&mut self) -> Step {
        self.confirm_at = None;
        if self.current == 0 {
            return Step::Stay;
        }
        self.current -= 1;
        Step::Moved
    }

    pub fn advance(&mut self) -> Step {
        self.confirm_at = None;
        if !self.items[self.current].validate() || self.is_last() {
            return Step::Stay;
        }
        self.current += 1;
        Step::Moved
    }

    /// Validates every question; the first one missing an answer becomes current.
    pub fn submit(&mut self) -> Step {
        self.confirm_at = None;
        let mut first_invalid = None;
        for (ix, item) in self.items.iter_mut().enumerate() {
            if !item.validate() && first_invalid.is_none() {
                first_invalid = Some(ix);
            }
        }
        match first_invalid {
            Some(ix) => {
                self.current = ix;
                Step::Stay
            }
            None => Step::Submit,
        }
    }

    /// Each question's answer by id, for [`build_answers`].
    pub fn answer_of(&self, id: &str) -> Option<(Vec<String>, Option<String>)> {
        self.items
            .iter()
            .find(|item| item.id == id)
            .map(Item::answer)
    }
}

/// Why questions cannot become a questionnaire: a repeated question id or a repeated choice.
fn schema_problem(items: &[Item]) -> Option<String> {
    let mut ids = HashSet::new();
    for item in items {
        if !ids.insert(&item.id) {
            return Some(format!("question “{}” appears twice", item.id));
        }
        let mut seen = HashSet::new();
        if let Some(dup) = item
            .choices
            .iter()
            .find(|choice| !seen.insert(&choice.value))
        {
            return Some(format!(
                "question “{}” offers “{}” twice",
                item.id, dup.value
            ));
        }
    }
    None
}

/// Wire answers (wire.md §5.6) from questionnaire results, in question order:
/// `answer_of(question id)` gives the chosen labels and any typed answer. A question with
/// options answers its chosen labels in `values` and the typed text in `other`; one without
/// (`input`, `editor`) answers the text in `values`. Unanswered questions are left out.
pub fn build_answers(
    questions: &[Question],
    answer_of: impl Fn(&str) -> Option<(Vec<String>, Option<String>)>,
) -> Vec<Answer> {
    questions
        .iter()
        .filter_map(|question| {
            let (choices, typed) = answer_of(&question.id)?;
            let typed = typed.filter(|text| !text.trim().is_empty());
            let (values, other) = if question.options.is_empty() {
                (typed.into_iter().collect(), None)
            } else {
                (choices, typed)
            };
            (!values.is_empty() || other.is_some()).then(|| Answer {
                question_id: question.id.clone(),
                values,
                other,
            })
        })
        .collect()
}

/// A questionnaire navigation from its buttons or a field's Enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Previous,
    Next,
    Submit,
    Confirm,
}

impl J3 {
    fn questionnaire(&mut self, key: &ThreadKey) -> Option<&mut Questionnaire> {
        self.threads
            .get_mut(key)?
            .chrome
            .question
            .as_mut()?
            .form
            .as_mut()
            .ok()
    }

    /// A click on choice `value` of question `ix` of `key`'s card.
    pub fn choose_answer(&mut self, key: &ThreadKey, ix: usize, value: &str, tx: &mut Tx) {
        let now = self.clock.t;
        let Some(form) = self.questionnaire(key) else {
            return;
        };
        let step = form.choose(ix, value, now);
        if form.confirm_at().is_some() {
            tx.wake_after(std::time::Duration::from_secs_f64(CONFIRM_DELAY));
        }
        if step == Step::Submit {
            self.answer_question(key);
        }
    }

    /// Question `ix`'s field of `key`'s card changed.
    pub fn answer_typed(&mut self, key: &ThreadKey, ix: usize) {
        if let Some(form) = self.questionnaire(key) {
            form.input_changed(ix);
        }
    }

    /// Previous / Next / Submit, or Enter in a field.
    pub fn navigate_question(&mut self, key: &ThreadKey, nav: Nav) {
        let Some(form) = self.questionnaire(key) else {
            return;
        };
        let step = match nav {
            Nav::Previous => form.previous(),
            Nav::Next => form.advance(),
            Nav::Submit => form.submit(),
            Nav::Confirm => form.confirm_current(),
        };
        if step == Step::Submit {
            self.answer_question(key);
        }
    }

    /// A picked single choice whose moment has passed confirms; true when one did.
    pub(crate) fn poll_question(&mut self, key: &ThreadKey, tx: &mut Tx) -> bool {
        let now = self.clock.t;
        let Some(form) = self.questionnaire(key) else {
            return false;
        };
        let Some(at) = form.confirm_at() else {
            return false;
        };
        match form.confirm_due(now) {
            Some(step) => {
                if step == Step::Submit {
                    self.answer_question(key);
                }
                true
            }
            None => {
                tx.wake_after(std::time::Duration::from_secs_f64((at - now).max(0.0)));
                false
            }
        }
    }

    /// Sends `key`'s card answers (`requests.answer`).
    fn answer_question(&mut self, key: &ThreadKey) {
        let Some(card) = self
            .threads
            .get(key)
            .and_then(|t| t.chrome.question.as_ref())
        else {
            return;
        };
        let Ok(form) = &card.form else {
            return;
        };
        let params = AnswerParams {
            thread_id: key.thread_id.clone(),
            request_id: card.request_id.clone(),
            answers: build_answers(&card.questions, |id| form.answer_of(id)),
            command_id: None,
        };
        let Some(connection) = self.thread_connection(key) else {
            return;
        };
        self.run_command(
            "answer the question",
            async move { connection.requests_answer(params).await },
            |_, _, _| {},
        );
    }

    /// Dismisses `key`'s open request (`requests.dismiss`).
    pub fn dismiss_question(&mut self, key: &ThreadKey) {
        let Some(card) = self
            .threads
            .get(key)
            .and_then(|t| t.chrome.question.as_ref())
        else {
            return;
        };
        let params = DismissParams {
            thread_id: key.thread_id.clone(),
            request_id: card.request_id.clone(),
            command_id: None,
        };
        let Some(connection) = self.thread_connection(key) else {
            return;
        };
        self.run_command(
            "dismiss the question",
            async move { connection.requests_dismiss(params).await },
            |_, _, _| {},
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(id: &str, options: &[&str], multiple: bool, other: bool) -> Question {
        let options: Vec<_> = options
            .iter()
            .map(|label| serde_json::json!({"label": label}))
            .collect();
        serde_json::from_value(serde_json::json!({
            "id": id, "header": "H", "text": "Q?", "options": options,
            "multiple": multiple, "allowOther": other
        }))
        .unwrap()
    }

    fn answer(id: &str, values: &[&str], other: Option<&str>) -> Answer {
        Answer {
            question_id: id.into(),
            values: values.iter().map(|v| (*v).to_owned()).collect(),
            other: other.map(str::to_owned),
        }
    }

    #[test]
    fn answers_follow_wire_shapes() {
        let questions = [
            question("color", &["red", "blue"], false, true),
            question("tools", &["a", "b"], true, true),
            question("name", &[], false, false),
            question("skip", &[], false, false),
        ];
        let answers = build_answers(&questions, |id| match id {
            "color" => Some((vec!["red".into()], None)),
            "tools" => Some((vec!["a".into(), "b".into()], Some("c".into()))),
            "name" => Some((vec![], Some("Ada".into()))),
            "skip" => Some((vec![], Some("  ".into()))),
            _ => None,
        });
        assert_eq!(
            answers,
            [
                answer("color", &["red"], None),
                answer("tools", &["a", "b"], Some("c")),
                answer("name", &["Ada"], None),
            ]
        );
        let typed = build_answers(&questions[..1], |_| Some((vec![], Some("teal".into()))));
        assert_eq!(typed, [answer("color", &[], Some("teal"))]);
    }

    #[test]
    fn repeated_ids_or_choices_cannot_be_shown() {
        let twice = [
            question("a", &[], false, false),
            question("a", &[], true, false),
        ];
        assert_eq!(
            Questionnaire::new(&twice).err().as_deref(),
            Some("question “a” appears twice")
        );
        let dup = [question("pick", &["x", "x"], false, false)];
        assert_eq!(
            Questionnaire::new(&dup).err().as_deref(),
            Some("question “pick” offers “x” twice")
        );
    }

    #[test]
    fn questions_block_until_answered() {
        let questions = [
            question("one", &["a", "b"], false, false),
            question("two", &["x", "y"], true, false),
        ];
        let mut form = Questionnaire::new(&questions).unwrap();
        assert_eq!(form.advance(), Step::Stay);
        assert!(form.items[0].missing);
        // A first pick waits a moment, then confirms; the error clears with the answer.
        assert_eq!(form.choose(0, "a", 1.0), Step::Stay);
        assert!(!form.items[0].missing);
        assert_eq!(form.confirm_due(1.1), None);
        assert_eq!(form.confirm_due(1.2), Some(Step::Moved));
        assert_eq!(form.current(), 1);
        // Checkboxes toggle and never confirm by themselves.
        assert_eq!(form.choose(1, "y", 2.0), Step::Stay);
        assert_eq!(form.choose(1, "x", 2.0), Step::Stay);
        assert_eq!(form.confirm_at(), None);
        assert_eq!(form.submit(), Step::Submit);
        assert_eq!(
            form.answer_of("two"),
            Some((vec!["x".into(), "y".into()], None))
        );
        // Picking the chosen answer again confirms at once.
        assert_eq!(form.previous(), Step::Moved);
        assert_eq!(form.choose(0, "a", 3.0), Step::Moved);
    }

    #[test]
    fn typed_answers_replace_a_single_choice() {
        let questions = [question("pick", &["a", "b"], false, true)];
        let mut form = Questionnaire::new(&questions).unwrap();
        form.choose(0, "a", 0.0);
        form.items[0].input.as_mut().unwrap().replace_text("teal");
        form.input_changed(0);
        assert_eq!(
            form.confirm_at(),
            None,
            "typing cancels the pending confirm"
        );
        assert_eq!(form.answer_of("pick"), Some((vec![], Some("teal".into()))));
        assert!(!form.items[0].is_chosen("a"));
        // A pick clears the typed answer again.
        form.choose(0, "b", 1.0);
        assert_eq!(form.answer_of("pick"), Some((vec!["b".into()], None)));
    }

    #[test]
    fn defaults_pick_their_option_or_fill_the_field() {
        let mut typed = question("name", &[], false, false);
        typed.default = Some("Ada".into());
        let mut picked = question("size", &["S", "M"], false, true);
        picked.default = Some("M".into());
        let mut form = Questionnaire::new(&[typed, picked]).unwrap();
        assert_eq!(form.submit(), Step::Submit);
        assert_eq!(form.answer_of("name"), Some((vec![], Some("Ada".into()))));
        assert_eq!(form.answer_of("size"), Some((vec!["M".into()], None)));
    }
}
