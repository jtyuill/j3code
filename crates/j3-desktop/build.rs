//! Embeds the source commit as `J3_GIT_COMMIT` for the window's version
//! watermark. Builds without a git checkout (the Nix sandbox) pass it in the
//! environment instead.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn main() {
    println!("cargo:rerun-if-env-changed=J3_GIT_COMMIT");
    let commit = match std::env::var("J3_GIT_COMMIT") {
        Ok(commit) if !commit.is_empty() => commit,
        _ => {
            // Rebuild when HEAD moves: a checkout rewrites HEAD, a commit
            // rewrites the branch ref (loose or packed).
            for path in ["HEAD", "packed-refs"] {
                if let Some(path) = git(&["rev-parse", "--git-path", path]) {
                    println!("cargo:rerun-if-changed={path}");
                }
            }
            if let Some(path) = git(&["symbolic-ref", "-q", "HEAD"])
                .and_then(|branch| git(&["rev-parse", "--git-path", &branch]))
            {
                println!("cargo:rerun-if-changed={path}");
            }
            git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".to_owned())
        }
    };
    println!("cargo:rustc-env=J3_GIT_COMMIT={commit}");
}
