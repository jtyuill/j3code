//! Thread titles named by omp.
//!
//! omp names a session only when it runs interactively (`autoTitle` is
//! `isInteractive` in its `main.ts`), so a `--mode rpc` session never gets
//! one. This asks omp for the same title: its own title prompt
//! (`prompts/system/title-system.md`) and reply parsing
//! (`normalizeGeneratedTitle` in `tiny/text.ts`), run on its fast model with
//! the one-shot `omp -p` that also writes commit messages.

use std::path::Path;

use crate::omp::OmpLauncher;

/// omp's `title-system.md`, verbatim.
const TITLE_SYSTEM_PROMPT: &str = "You name coding-agent sessions. Write a 3-6 word title for the task in the next message.

Output exactly one line: the title inside `<title>` tags, or `<title/>` when there is no task to name.

Rules:
- Name the user's goal as a short imperative phrase built from the user's own key words (the feature, component, or error they name). Avoid vague filler like \"logic\", \"issues\", \"functionality\".
- `[Image #N, WxH]` marks an image you cannot see. Never mention it or guess what it shows; title the task the surrounding words name.
- Answer `<title/>` for greetings, acknowledgements, gibberish, or requests too vague to name without context you cannot see (\"help\", \"fix this\", \"what's wrong?\").
- A `<chat>` block summarizes a longer session: its first `<user>` turn is the opening request, followed by recent turns (often the assistant's working notes). Title the goal the session is working on, using the opening request unless later turns clearly moved to a new task; never title the current micro-step, and ignore file names and symbols unless they are the subject.
- No quotes, trailing punctuation, or explanations.

Examples:
- \"the retry queue drops jobs when redis restarts, can u look\" → `<title>Fix retry queue dropping jobs</title>`
- \"can we get rid of the npm run build warning about peer deps\" → `<title>Remove peer dependency build warning</title>`
- `<chat>` with only assistant notes like \"adding a backoff field to RetryPolicy… now wiring jitter into schedule()\" → `<title>Add retry backoff with jitter</title>`";

/// omp's `MAX_TITLE_CHARS` and `MAX_TITLE_WORDS`.
const MAX_TITLE_CHARS: usize = 80;
const MAX_TITLE_WORDS: usize = 12;
/// omp truncates the message it titles (`truncateTinyMessage`); this keeps a
/// pasted log from becoming the prompt.
const MAX_MESSAGE_CHARS: usize = 2_000;

/// omp's title for a thread that opens with `message`, run in `cwd`. `None`
/// when omp names no title (a greeting, a request too vague to name) or
/// cannot be asked.
pub async fn generate(omp: &OmpLauncher, message: &str, cwd: &Path) -> Option<String> {
    let message: String = message.chars().take(MAX_MESSAGE_CHARS).collect();
    if message.trim().is_empty() {
        return None;
    }
    let prompt = format!("<user>\n{}\n</user>", message.trim());
    match omp
        .generate_text(&prompt, TITLE_SYSTEM_PROMPT, cwd, crate::omp::SMOL_MODEL)
        .await
    {
        Ok(reply) => parse(&reply),
        Err(error) => {
            tracing::warn!(%error, "omp could not name the thread");
            None
        }
    }
}

/// The title inside the first `<title>` tags of `reply`, cleaned as omp's
/// `normalizeGeneratedTitle` does (quotes and closing punctuation off).
/// `None` without tags (a model that answered the message instead of naming
/// it), for `<title/>` and `none`, junk, and titles that are too long.
fn parse(reply: &str) -> Option<String> {
    let lower = reply.to_ascii_lowercase();
    let open = lower.find("<title>")? + "<title>".len();
    let close = lower[open..].find("</title>")? + open;
    let title = reply[open..close].lines().next()?.trim();
    let title = title.trim_matches(['"', '\'']);
    let title = title.strip_suffix(['.', '!', '?']).unwrap_or(title).trim();
    if title.is_empty() || title.eq_ignore_ascii_case("none") {
        return None;
    }
    let words = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .count();
    (words > 0 && words <= MAX_TITLE_WORDS && title.chars().count() <= MAX_TITLE_CHARS)
        .then(|| title.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_come_out_of_their_tags() {
        assert_eq!(
            parse("<title>Fix retry queue dropping jobs</title>\n").as_deref(),
            Some("Fix retry queue dropping jobs")
        );
        assert_eq!(
            parse("  <TITLE>Remove build warning.</TITLE> ").as_deref(),
            Some("Remove build warning")
        );
        assert_eq!(
            parse("Working...\n<title>\"Add retry backoff\"</title>").as_deref(),
            Some("Add retry backoff")
        );
        assert_eq!(
            parse("<title>Fix it</title>\nbecause...").as_deref(),
            Some("Fix it")
        );
    }

    #[test]
    fn no_task_means_no_title() {
        for reply in [
            "<title/>",
            "<title />",
            "<title>none</title>",
            "<title>None</title>",
            "",
            "<title>..</title>",
            "Hi! What can I help you with today?",
            "<title>unterminated",
        ] {
            assert_eq!(parse(reply), None, "{reply:?}");
        }
    }

    #[test]
    fn long_titles_are_rejected() {
        let words = vec!["word"; MAX_TITLE_WORDS + 1].join(" ");
        assert_eq!(parse(&format!("<title>{words}</title>")), None);
        let long = "x".repeat(MAX_TITLE_CHARS + 1);
        assert_eq!(parse(&format!("<title>{long}</title>")), None);
    }
}
