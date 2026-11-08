//! Completion for the composer: omp slash commands and skills of the
//! thread's workspace (skills are inserted as `/skill:<name>`), the models
//! offered by a `^` mention, and the files and directories an `@` reference
//! names (omp reads `@path` files into the prompt itself on submit).

use std::collections::HashSet;

use j3_protocol::t3::server::{ServerConfig, ServerProviderSkill, ServerProviderSlashCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionKind {
    Command,
    Skill,
    /// A model for a `^` mention, by slug. Accepted through the mention
    /// token, not [`apply`].
    Model(String),
    /// A file or directory for an `@` reference, by its path as the server
    /// shows it (relative to the workspace, or as typed outside it).
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

/// Commands and skills for `cwd`: the provider-wide lists merged with the
/// workspace snapshot for `cwd`, first occurrence of a name winning.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    pub commands: Vec<ServerProviderSlashCommand>,
    pub skills: Vec<ServerProviderSkill>,
    /// Whether the server has a workspace snapshot for `cwd`.
    pub has_workspace: bool,
}

impl Catalog {
    pub fn for_workspace(config: &ServerConfig, cwd: &str) -> Self {
        let mut catalog = Self::default();
        let mut command_names = HashSet::new();
        let mut skill_names = HashSet::new();
        for provider in &config.providers {
            let snapshot = provider
                .workspace_snapshots
                .iter()
                .flatten()
                .find(|snapshot| snapshot.cwd == cwd);
            catalog.has_workspace |= snapshot.is_some();
            let commands = snapshot
                .into_iter()
                .flat_map(|s| &s.slash_commands)
                .chain(&provider.slash_commands);
            for command in commands {
                if command_names.insert(command.name.clone()) {
                    catalog.commands.push(command.clone());
                }
            }
            let skills = snapshot
                .into_iter()
                .flat_map(|s| &s.skills)
                .chain(&provider.skills);
            for skill in skills {
                if skill.enabled
                    && skill.user_invocable != Some(false)
                    && skill_names.insert(skill.name.clone())
                {
                    catalog.skills.push(skill.clone());
                }
            }
        }
        catalog
    }

    /// Adds the commands and skills fetched for this workspace by
    /// `refresh_workspace`, keeping names already present.
    pub fn merge(
        &mut self,
        commands: &[ServerProviderSlashCommand],
        skills: &[ServerProviderSkill],
    ) {
        self.has_workspace = true;
        for command in commands {
            if !self.commands.iter().any(|c| c.name == command.name) {
                self.commands.push(command.clone());
            }
        }
        for skill in skills {
            if skill.enabled
                && skill.user_invocable != Some(false)
                && !self.skills.iter().any(|s| s.name == skill.name)
            {
                self.skills.push(skill.clone());
            }
        }
    }

    /// Rows matching `query` (the text after `/`): prefix matches first,
    /// then substring matches; commands before skills within each group.
    pub fn complete(&self, query: &str) -> Vec<Completion> {
        let query = query.to_lowercase();
        let commands = self.commands.iter().map(|command| Completion {
            kind: CompletionKind::Command,
            label: format!("/{}", command.name),
            description: command.description.clone(),
            hint: command.input.as_ref().map(|input| input.hint.clone()),
        });
        let skills = self.skills.iter().map(|skill| Completion {
            kind: CompletionKind::Skill,
            label: format!("/skill:{}", skill.name),
            description: skill
                .short_description
                .clone()
                .or_else(|| skill.description.clone()),
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

    fn config() -> ServerConfig {
        serde_json::from_value(serde_json::json!({
            "environment": {"environmentId":"env","label":"mac","platform":{"os":"darwin","arch":"arm64"},
                "serverVersion":"0.1.0","capabilities":{}},
            "auth": {"policy":"unsafe-no-auth","bootstrapMethods":[],"sessionMethods":[],"sessionCookieName":"j3"},
            "cwd":"/", "keybindingsConfigPath":"/k.json", "keybindings":[], "issues":[], "availableEditors":[],
            "observability":{"logsDirectoryPath":"/l","localTracingEnabled":false,"otlpTracesEnabled":false,"otlpMetricsEnabled":false},
            "settings":{},
            "providers":[{"instanceId":"omp","driver":"pi","enabled":true,"installed":true,"version":"1","status":"ready",
                "auth":{"status":"authenticated"},"checkedAt":"t","models":[],
                "slashCommands":[{"name":"compact","description":"Compact the context","input":{"hint":"[focus]"}}],
                "skills":[{"name":"jev","path":"/s/jev","enabled":true,"description":"Calibrated judgments"},
                          {"name":"off","path":"/s/off","enabled":false}],
                "workspaceSnapshots":[{"cwd":"/repo","checkedAt":"t",
                    "slashCommands":[{"name":"review"},{"name":"compact","description":"Workspace compact"}],
                    "skills":[{"name":"commit","path":"/repo/commit","enabled":true,"shortDescription":"Commit"},
                              {"name":"internal","path":"/repo/i","enabled":true,"userInvocable":false}]}]}]
        }))
        .unwrap()
    }

    #[test]
    fn merges_provider_and_workspace_lists() {
        let config = config();
        let catalog = Catalog::for_workspace(&config, "/repo");
        assert!(catalog.has_workspace);
        let commands: Vec<_> = catalog.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(commands, ["review", "compact"]);
        assert_eq!(
            catalog.commands[1].description.as_deref(),
            Some("Workspace compact")
        );
        let skills: Vec<_> = catalog.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(skills, ["commit", "jev"]);

        let elsewhere = Catalog::for_workspace(&config, "/other");
        assert!(!elsewhere.has_workspace);
        assert_eq!(elsewhere.commands.len(), 1);
    }

    #[test]
    fn completes_prefix_matches_before_substring_matches() {
        let catalog = Catalog::for_workspace(&config(), "/repo");
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
        // The workspace's own `compact` (no hint) shadows the provider's.
        let compact = &catalog.complete("compact")[0];
        assert_eq!(compact.hint, None);
        assert_eq!(compact.kind, CompletionKind::Command);
        let elsewhere = Catalog::for_workspace(&config(), "/other");
        assert_eq!(
            elsewhere.complete("compact")[0].hint.as_deref(),
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
