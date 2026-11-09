//! Completion for the composer: omp slash commands (the server's and the
//! thread's project's) and skills (inserted as `/skill:<name>`), the models
//! offered by a `^` mention, and the files and directories an `@` reference
//! names (omp reads `@path` files into the prompt itself on submit).

use std::collections::HashSet;

use j3_client::methods::{self, CatalogCommand, CatalogSkill};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionKind {
    Command,
    Skill,
    /// A model for a `^` mention, by slug. Accepted through the mention
    /// token, not [`apply`].
    Model(String),
    /// A file or directory for an `@` reference, by its path relative to the
    /// project.
    Path {
        path: String,
        directory: bool,
    },
}

/// One row of the completion menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub kind: CompletionKind,
    /// What the menu shows; commands and skills also insert it, without the
    /// trailing space.
    pub label: String,
    pub description: Option<String>,
    /// Argument hint shown after the label (commands only).
    pub hint: Option<String>,
}

/// Commands and skills for one project: its own commands, then the
/// server's, first occurrence of a name winning, and the server's skills.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    pub commands: Vec<CatalogCommand>,
    pub skills: Vec<CatalogSkill>,
}

impl Catalog {
    /// `catalog`'s commands and skills with `project`'s own commands (as
    /// `server.refresh` returned them) first.
    pub fn new(catalog: &methods::Catalog, project: &[CatalogCommand]) -> Self {
        let mut names = HashSet::new();
        let commands = project
            .iter()
            .chain(&catalog.commands)
            .filter(|command| names.insert(command.name.as_str()))
            .cloned()
            .collect();
        Self {
            commands,
            skills: catalog.skills.clone(),
        }
    }

    /// Rows matching `query` (the text after `/`): prefix matches first,
    /// then substring matches; commands before skills within each group.
    pub fn complete(&self, query: &str) -> Vec<Completion> {
        let query = query.to_lowercase();
        let commands = self.commands.iter().map(|command| Completion {
            kind: CompletionKind::Command,
            label: format!("/{}", command.name),
            description: non_empty(&command.description),
            hint: command.hint.clone(),
        });
        let skills = self.skills.iter().map(|skill| Completion {
            kind: CompletionKind::Skill,
            label: format!("/skill:{}", skill.name),
            description: non_empty(&skill.description),
            hint: None,
        });
        let mut prefix = Vec::new();
        let mut contains = Vec::new();
        for completion in commands.chain(skills) {
            let name = completion.label[1..].to_lowercase();
            if name.starts_with(&query)
                || (completion.kind == CompletionKind::Skill
                    && name["skill:".len()..].starts_with(&query))
            {
                prefix.push(completion);
            } else if name.contains(&query) {
                contains.push(completion);
            }
        }
        prefix.extend(contains);
        prefix
    }
}

fn non_empty(text: &str) -> Option<String> {
    (!text.trim().is_empty()).then(|| text.to_owned())
}

/// The query of a slash command being typed: the text is `/` followed by
/// non-whitespace up to the cursor (a byte offset), at the very start of the
/// message.
pub fn slash_query(text: &str, cursor: usize) -> Option<&str> {
    let before = text.get(..cursor)?;
    let query = before.strip_prefix('/')?;
    (!query.contains(char::is_whitespace)).then_some(query)
}

/// The characters that end a path token, as omp's editor splits them (it
/// works per line, hence the newline).
const PATH_DELIMITERS: [char; 6] = [' ', '\t', '\n', '"', '\'', '='];

/// An `@` file reference being typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathQuery<'a> {
    /// Byte offset of the `@`.
    pub start: usize,
    /// The path typed after `@` (or `@"`) up to the cursor.
    pub query: &'a str,
    /// Typed as `@"…`, for a path with spaces.
    pub quoted: bool,
}

/// omp's `@` reference ending at `cursor` (a byte offset): the token after the
/// last path delimiter starts with `@`, or an unclosed `"` on the line opens
/// right after an `@` that starts a token.
pub fn path_query(text: &str, cursor: usize) -> Option<PathQuery<'_>> {
    let before = text.get(..cursor)?;
    let line = &before[before.rfind('\n').map_or(0, |i| i + 1)..];
    if line.matches('"').count() % 2 == 1 {
        let open = cursor - line.len() + line.rfind('"')?;
        if let Some(at) = open.checked_sub(1)
            && before[at..].starts_with('@')
            && (at == 0 || before[..at].ends_with(PATH_DELIMITERS))
        {
            return Some(PathQuery {
                start: at,
                query: &before[open + 1..],
                quoted: true,
            });
        }
    }
    let start = before.rfind(PATH_DELIMITERS).map_or(0, |i| i + 1);
    let query = before[start..].strip_prefix('@')?;
    Some(PathQuery {
        start,
        query,
        quoted: false,
    })
}

/// A menu row for the server's entry `path`: its name (a directory's with a
/// trailing `/`) over the whole path.
pub fn path_completion(path: String, directory: bool) -> Completion {
    let name = path.rsplit('/').next().unwrap_or(&path);
    let label = if directory {
        format!("{name}/")
    } else {
        name.to_owned()
    };
    Completion {
        kind: CompletionKind::Path {
            path: path.clone(),
            directory,
        },
        description: (path != name).then_some(path),
        label,
        hint: None,
    }
}

/// What accepting `path` inserts in place of its `@` reference, like omp: a
/// file as `@path ` and a directory as `@dir/` so its entries come next; a
/// path with whitespace (or a reference typed quoted) as `@"path" `, a
/// directory's quote left open.
pub fn path_insert(path: &str, directory: bool, quoted: bool) -> String {
    let quoted = quoted || path.contains(char::is_whitespace);
    match (quoted, directory) {
        (false, false) => format!("@{path} "),
        (false, true) => format!("@{path}/"),
        (true, false) => format!("@\"{path}\" "),
        (true, true) => format!("@\"{path}/"),
    }
}

/// `text` with its leading `/token` replaced by `completion` and a space.
pub fn apply(text: &str, completion: &Completion) -> String {
    let rest = text
        .find(char::is_whitespace)
        .map_or("", |end| text[end..].trim_start());
    format!("{} {rest}", completion.label)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        let catalog: methods::Catalog = serde_json::from_value(serde_json::json!({
            "models": [],
            "commands": [{"name": "compact", "description": "Compact the context", "hint": "[focus]"}],
            "skills": [{"name": "commit", "description": "Commit"},
                       {"name": "jev", "description": "Calibrated judgments"}],
            "status": "ready"
        }))
        .unwrap();
        let project: Vec<CatalogCommand> = serde_json::from_value(serde_json::json!([
            {"name": "review", "description": ""},
            {"name": "compact", "description": "Project compact"}
        ]))
        .unwrap();
        Catalog::new(&catalog, &project)
    }

    #[test]
    fn merges_project_and_server_commands() {
        let catalog = catalog();
        let commands: Vec<_> = catalog.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(commands, ["review", "compact"]);
        assert_eq!(catalog.commands[1].description, "Project compact");
        let skills: Vec<_> = catalog.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(skills, ["commit", "jev"]);
    }

    #[test]
    fn completes_prefix_matches_before_substring_matches() {
        let catalog = catalog();
        let labels = |query: &str| -> Vec<String> {
            catalog
                .complete(query)
                .into_iter()
                .map(|c| c.label)
                .collect()
        };
        assert_eq!(
            labels(""),
            ["/review", "/compact", "/skill:commit", "/skill:jev"]
        );
        assert_eq!(labels("co"), ["/compact", "/skill:commit"]);
        assert_eq!(labels("skill:j"), ["/skill:jev"]);
        assert_eq!(labels("ev"), ["/review", "/skill:jev"]);
        assert_eq!(catalog.complete("review")[0].description, None);
        // The project's own `compact` (no hint) shadows the server's.
        let compact = &catalog.complete("compact")[0];
        assert_eq!(compact.hint, None);
        assert_eq!(compact.kind, CompletionKind::Command);
        let server_only = Catalog::new(
            &serde_json::from_value(serde_json::json!({
                "models": [], "skills": [], "status": "ready",
                "commands": [{"name": "compact", "description": "", "hint": "[focus]"}]
            }))
            .unwrap(),
            &[],
        );
        assert_eq!(
            server_only.complete("compact")[0].hint.as_deref(),
            Some("[focus]")
        );
    }

    #[test]
    fn slash_query_only_at_message_start() {
        assert_eq!(slash_query("/com", 4), Some("com"));
        assert_eq!(slash_query("/", 1), Some(""));
        assert_eq!(slash_query("/compact now", 12), None);
        assert_eq!(slash_query("/compact now", 3), Some("co"));
        assert_eq!(slash_query("hi /x", 5), None);
        assert_eq!(slash_query("", 0), None);
        assert_eq!(slash_query("/é", 1), Some(""));
        assert_eq!(slash_query("/é", 2), None);
    }

    #[test]
    fn apply_replaces_the_leading_token() {
        let completion = Completion {
            kind: CompletionKind::Skill,
            label: "/skill:jev".into(),
            description: None,
            hint: None,
        };
        assert_eq!(apply("/je", &completion), "/skill:jev ");
        assert_eq!(apply("/je  rate this", &completion), "/skill:jev rate this");
    }

    #[test]
    fn path_query_follows_omps_at_token_rules() {
        fn query(text: &str) -> Option<(usize, &str, bool)> {
            path_query(text, text.len()).map(|q| (q.start, q.query, q.quoted))
        }
        assert_eq!(query("@"), Some((0, "", false)));
        assert_eq!(query("look at @src/ma"), Some((8, "src/ma", false)));
        assert_eq!(query("a\n@x"), Some((2, "x", false)));
        assert_eq!(query("key=@x"), Some((4, "x", false)));
        assert_eq!(query("mail me@host"), None);
        assert_eq!(query("@src/main.rs "), None);
        assert_eq!(query("see @\"my dir/fi"), Some((4, "my dir/fi", true)));
        // A closed quote ends the reference; an open one not after `@` is a delimiter.
        assert_eq!(query("see @\"a b\" c"), None);
        assert_eq!(query("say \"@x"), Some((5, "x", false)));
        assert_eq!(path_query("@é", 2), None);
    }

    #[test]
    fn path_rows_and_inserts() {
        let row = path_completion("src/lib".into(), true);
        assert_eq!(row.label, "lib/");
        assert_eq!(row.description.as_deref(), Some("src/lib"));
        assert_eq!(path_completion("README.md".into(), false).description, None);
        assert_eq!(path_insert("src/main.rs", false, false), "@src/main.rs ");
        assert_eq!(path_insert("src", true, false), "@src/");
        assert_eq!(
            path_insert("my notes.md", false, false),
            "@\"my notes.md\" "
        );
        assert_eq!(path_insert("my dir", true, true), "@\"my dir/");
    }
}
