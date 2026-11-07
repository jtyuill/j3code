//! The extension → language map shared by the file pane and the feed's tool
//! output. The names are fence language names, as
//! `rune_kit::syntax::highlight` takes them.

/// Fence languages by the file extensions that select them.
const LANGUAGES: &[(&[&str], &str)] = &[
    (&["rs"], "rust"),
    (&["ts", "mts", "cts"], "typescript"),
    (&["tsx"], "tsx"),
    (&["js", "mjs", "cjs", "jsx"], "javascript"),
    (&["json", "jsonc"], "json"),
    (&["toml"], "toml"),
    (&["md", "markdown", "mdx"], "markdown"),
    (&["py", "pyi"], "python"),
    (&["sh", "bash", "zsh"], "bash"),
    (&["c", "h"], "c"),
    (&["cpp", "cc", "cxx", "hpp", "hh"], "cpp"),
    (&["css"], "css"),
    (&["go"], "go"),
    (&["html", "htm"], "html"),
    (&["java"], "java"),
    (&["sql"], "sql"),
    (&["yaml", "yml"], "yaml"),
];

/// The fence language of `path`, from its extension; `"text"` when there is
/// none for it.
pub fn language(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((_, extension)) = name.rsplit_once('.') else {
        return "text";
    };
    LANGUAGES
        .iter()
        .find(|(extensions, _)| {
            extensions
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
        .map_or("text", |(_, language)| *language)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_come_from_extensions() {
        assert_eq!(language("src/main.rs"), "rust");
        assert_eq!(language("web/App.TSX"), "tsx");
        assert_eq!(language("include/a.HPP"), "cpp");
        assert_eq!(language("lib/x.h"), "c");
        assert_eq!(language(".github/ci.yml"), "yaml");
        assert_eq!(language("db/schema.sql"), "sql");
        assert_eq!(language("index.htm"), "html");
        assert_eq!(language("settings.jsonc"), "json");
        assert_eq!(language("Makefile"), "text");
        assert_eq!(language("v1.2/README"), "text");
        assert_eq!(language("notes.txt"), "text");
    }
}
