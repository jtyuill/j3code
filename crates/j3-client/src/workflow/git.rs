//! The git control's choices, ported from t3's `resolveQuickAction` and
//! `buildMenuItems` (`apps/web/src/components/GitActionsControl.logic.ts:94-310`)
//! without pull requests, publishing or the default-branch confirmation:
//! j3 commits, pushes and pulls.

use j3_protocol::t3::git::{GitStackedAction, VcsStatusResult};

/// What the primary button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickActionKind {
    /// Runs a stacked action (`git.runStackedAction`).
    Run(GitStackedAction),
    /// Pulls (`vcs.pull`).
    Pull,
    /// Nothing to run; the hint explains why (a disabled button's tooltip).
    Hint(&'static str),
}

/// The git control's primary button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuickAction {
    pub label: &'static str,
    pub disabled: bool,
    pub kind: QuickActionKind,
}

const fn hint(label: &'static str, hint: &'static str) -> QuickAction {
    QuickAction {
        label,
        disabled: true,
        kind: QuickActionKind::Hint(hint),
    }
}

const fn run(label: &'static str, action: GitStackedAction) -> QuickAction {
    QuickAction {
        label,
        disabled: false,
        kind: QuickActionKind::Run(action),
    }
}

/// `resolveQuickAction` (`GitActionsControl.logic.ts:167-310`), reduced to
/// commit, push and pull.
pub fn quick_action(status: Option<&VcsStatusResult>, busy: bool) -> QuickAction {
    if busy {
        return hint("Commit", "Git action in progress.");
    }
    let Some(status) = status else {
        return hint("Commit", "Git status is unavailable.");
    };
    if status.ref_name.is_none() {
        return hint("Commit", "Create and checkout a branch before pushing.");
    }
    let ahead = status.ahead_count > 0;
    let behind = status.behind_count > 0;

    if status.has_working_tree_changes {
        return if !status.has_upstream && !status.has_primary_remote {
            run("Commit", GitStackedAction::Commit)
        } else {
            run("Commit & push", GitStackedAction::CommitPush)
        };
    }

    if !status.has_upstream {
        return if !status.has_primary_remote {
            hint("Commit", "No changes to commit.")
        } else if !ahead {
            hint("Push", "No local commits to push.")
        } else {
            run("Push", GitStackedAction::Push)
        };
    }

    if ahead && behind {
        return hint(
            "Sync branch",
            "Branch has diverged from upstream. Rebase/merge first.",
        );
    }
    if behind {
        return QuickAction {
            label: "Pull",
            disabled: false,
            kind: QuickActionKind::Pull,
        };
    }
    if ahead {
        return run("Push", GitStackedAction::Push);
    }
    hint("Commit", "Branch is up to date. No action needed.")
}

/// Which items of the git control's menu are enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GitMenu {
    /// "Commit…": there are changes to commit.
    pub commit: bool,
    /// "Push": a branch with commits to push and somewhere to push them.
    pub push: bool,
    /// "Pull": the upstream has commits this branch lacks.
    pub pull: bool,
}

/// `buildMenuItems` (`GitActionsControl.logic.ts:94-165`) without the pull
/// request item, plus pull.
pub fn menu(status: Option<&VcsStatusResult>, busy: bool) -> GitMenu {
    let Some(status) = status.filter(|_| !busy) else {
        return GitMenu::default();
    };
    let behind = status.behind_count > 0;
    GitMenu {
        commit: status.has_working_tree_changes,
        push: status.ref_name.is_some()
            && !behind
            && status.ahead_count > 0
            && (status.has_upstream || status.has_primary_remote),
        pull: status.has_upstream && behind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GitStackedAction::{Commit, CommitPush, Push};
    use QuickActionKind::{Hint, Pull, Run};

    /// A repository on `main` with the given shape.
    fn status(
        changes: bool,
        remote: bool,
        upstream: bool,
        ahead: u64,
        behind: u64,
    ) -> VcsStatusResult {
        VcsStatusResult {
            is_repo: true,
            has_primary_remote: remote,
            ref_name: Some("main".into()),
            has_working_tree_changes: changes,
            has_upstream: upstream,
            ahead_count: ahead,
            behind_count: behind,
            ..VcsStatusResult::not_a_repo()
        }
    }

    #[test]
    fn the_quick_action_ladder() {
        let detached = VcsStatusResult {
            ref_name: None,
            ..status(true, true, true, 0, 0)
        };
        #[rustfmt::skip]
        let cases = [
            ("busy", Some(status(true, true, true, 0, 0)), true, "Commit", true, Hint("Git action in progress.")),
            ("no status", None, false, "Commit", true, Hint("Git status is unavailable.")),
            ("detached", Some(detached), false, "Commit", true, Hint("Create and checkout a branch before pushing.")),
            ("changes, local only", Some(status(true, false, false, 0, 0)), false, "Commit", false, Run(Commit)),
            ("changes, remote", Some(status(true, true, false, 0, 0)), false, "Commit & push", false, Run(CommitPush)),
            ("changes, upstream", Some(status(true, true, true, 2, 1)), false, "Commit & push", false, Run(CommitPush)),
            ("clean, local only", Some(status(false, false, false, 3, 0)), false, "Commit", true, Hint("No changes to commit.")),
            ("clean, unpublished, nothing ahead", Some(status(false, true, false, 0, 0)), false, "Push", true, Hint("No local commits to push.")),
            ("clean, unpublished, ahead", Some(status(false, true, false, 2, 0)), false, "Push", false, Run(Push)),
            ("diverged", Some(status(false, true, true, 1, 1)), false, "Sync branch", true, Hint("Branch has diverged from upstream. Rebase/merge first.")),
            ("behind", Some(status(false, true, true, 0, 2)), false, "Pull", false, Pull),
            ("ahead", Some(status(false, true, true, 2, 0)), false, "Push", false, Run(Push)),
            ("up to date", Some(status(false, true, true, 0, 0)), false, "Commit", true, Hint("Branch is up to date. No action needed.")),
            ("upstream without primary remote", Some(status(false, false, true, 1, 0)), false, "Push", false, Run(Push)),
        ];
        for (name, status, busy, label, disabled, kind) in cases {
            assert_eq!(
                quick_action(status.as_ref(), busy),
                QuickAction {
                    label,
                    disabled,
                    kind
                },
                "{name}"
            );
        }
    }

    #[test]
    fn the_menu_enables_what_can_run() {
        let detached = VcsStatusResult {
            ref_name: None,
            ..status(false, true, true, 2, 0)
        };
        let menu_of = |commit, push, pull| GitMenu { commit, push, pull };
        #[rustfmt::skip]
        let cases = [
            ("no status", None, false, menu_of(false, false, false)),
            ("busy", Some(status(true, true, true, 2, 0)), true, menu_of(false, false, false)),
            ("changes", Some(status(true, false, false, 0, 0)), false, menu_of(true, false, false)),
            ("ahead of upstream", Some(status(false, true, true, 2, 0)), false, menu_of(false, true, false)),
            ("unpublished, ahead", Some(status(false, true, false, 1, 0)), false, menu_of(false, true, false)),
            ("local only, ahead", Some(status(false, false, false, 1, 0)), false, menu_of(false, false, false)),
            ("behind", Some(status(false, true, true, 0, 3)), false, menu_of(false, false, true)),
            ("diverged", Some(status(true, true, true, 1, 1)), false, menu_of(true, false, true)),
            ("detached", Some(detached), false, menu_of(false, false, false)),
        ];
        for (name, status, busy, expected) in cases {
            assert_eq!(menu(status.as_ref(), busy), expected, "{name}");
        }
    }
}
