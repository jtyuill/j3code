//! The git control's choices, ported from t3's `resolveQuickAction` and
//! `buildMenuItems` (`apps/web/src/components/GitActionsControl.logic.ts:94-310`)
//! without publishing or the default-branch confirmation: j3 commits,
//! pushes, pulls and, on a GitHub remote, opens pull requests.

use j3_protocol::t3::git::{GitStackedAction, VcsChangeRequestState, VcsStatusResult};

/// What the primary button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickActionKind {
    /// Runs a stacked action (`git.runStackedAction`).
    Run(GitStackedAction),
    /// Pulls (`vcs.pull`).
    Pull,
    /// Opens the branch's pull request (its URL is `status.pr`).
    OpenPr,
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

/// Whether the branch has a pull request that is open.
fn has_open_pr(status: &VcsStatusResult) -> bool {
    status
        .pr
        .as_ref()
        .is_some_and(|pr| pr.state == VcsChangeRequestState::Open)
}

/// Whether a pull request can be opened from this branch: a GitHub remote,
/// a branch other than the default one, and no open PR yet.
fn can_create_pr(status: &VcsStatusResult) -> bool {
    status.source_control_provider.is_some()
        && status.ref_name.is_some()
        && !status.is_default_ref
        && status.has_primary_remote
        && !has_open_pr(status)
}

/// `resolveQuickAction` (`GitActionsControl.logic.ts:167-310`), reduced to
/// commit, push, pull and pull requests.
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
    let creates_pr = can_create_pr(status);

    if status.has_working_tree_changes {
        return if !status.has_upstream && !status.has_primary_remote {
            run("Commit", GitStackedAction::Commit)
        } else if creates_pr {
            run("Commit, push & PR", GitStackedAction::CommitPushPr)
        } else {
            run("Commit & push", GitStackedAction::CommitPush)
        };
    }

    if !status.has_upstream {
        return if !status.has_primary_remote {
            hint("Commit", "No changes to commit.")
        } else if !ahead {
            hint("Push", "No local commits to push.")
        } else if creates_pr {
            run("Push & create PR", GitStackedAction::CreatePr)
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
        return if creates_pr {
            run("Push & create PR", GitStackedAction::CreatePr)
        } else {
            run("Push", GitStackedAction::Push)
        };
    }
    if has_open_pr(status) {
        return QuickAction {
            label: "View PR",
            disabled: false,
            kind: QuickActionKind::OpenPr,
        };
    }
    // A branch that never had a PR (a merged or closed one means it was
    // dealt with) and is fully pushed.
    if creates_pr && status.pr.is_none() {
        return run("Create PR", GitStackedAction::CreatePr);
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
    /// "Create PR": a GitHub branch, pushed or not, without an open PR.
    pub create_pr: bool,
    /// "View PR": the branch has an open PR.
    pub view_pr: bool,
}

/// `buildMenuItems` (`GitActionsControl.logic.ts:94-165`) plus pull.
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
        create_pr: can_create_pr(status) && !behind && !status.has_working_tree_changes,
        view_pr: has_open_pr(status),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GitStackedAction::{Commit, CommitPush, Push};
    use QuickActionKind::{Hint, Pull, Run};
    use j3_protocol::t3::git::{SourceControlProviderInfo, VcsChangeRequest};

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
        let menu_of = |commit, push, pull| GitMenu {
            commit,
            push,
            pull,
            ..GitMenu::default()
        };
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

    /// A pushed feature branch on a GitHub remote, with `pr` as `gh` reports.
    fn github(pr: Option<VcsChangeRequestState>, ahead: u64, behind: u64) -> VcsStatusResult {
        VcsStatusResult {
            source_control_provider: Some(SourceControlProviderInfo::github()),
            is_default_ref: false,
            ref_name: Some("feature".into()),
            pr: pr.map(|state| VcsChangeRequest {
                number: 9,
                title: "Add widgets".into(),
                url: "https://github.com/acme/widgets/pull/9".into(),
                base_ref: "main".into(),
                head_ref: "feature".into(),
                state,
                is_draft: None,
                updated_at: None,
            }),
            ..status(false, true, true, ahead, behind)
        }
    }

    #[test]
    fn pull_requests_extend_the_ladder_on_github() {
        use VcsChangeRequestState::{Merged, Open};
        let dirty = VcsStatusResult {
            has_working_tree_changes: true,
            ..github(None, 0, 0)
        };
        let unpublished = VcsStatusResult {
            has_upstream: false,
            ..github(None, 2, 0)
        };
        let on_default = VcsStatusResult {
            is_default_ref: true,
            ..github(None, 2, 0)
        };
        #[rustfmt::skip]
        let cases = [
            ("changes, no PR", dirty.clone(), "Commit, push & PR", false, Run(GitStackedAction::CommitPushPr)),
            ("changes, open PR", VcsStatusResult { pr: github(Some(Open), 0, 0).pr, ..dirty }, "Commit & push", false, Run(CommitPush)),
            ("unpublished, ahead", unpublished, "Push & create PR", false, Run(GitStackedAction::CreatePr)),
            ("ahead, no PR", github(None, 2, 0), "Push & create PR", false, Run(GitStackedAction::CreatePr)),
            ("ahead, open PR", github(Some(Open), 2, 0), "Push", false, Run(Push)),
            ("pushed, open PR", github(Some(Open), 0, 0), "View PR", false, QuickActionKind::OpenPr),
            ("pushed, never had a PR", github(None, 0, 0), "Create PR", false, Run(GitStackedAction::CreatePr)),
            ("pushed, PR merged", github(Some(Merged), 0, 0), "Commit", true, Hint("Branch is up to date. No action needed.")),
            ("behind, open PR", github(Some(Open), 0, 2), "Pull", false, Pull),
            ("default branch", on_default, "Push", false, Run(Push)),
        ];
        for (name, status, label, disabled, kind) in cases {
            assert_eq!(
                quick_action(Some(&status), false),
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
    fn the_menu_offers_the_pull_request_items_on_github() {
        use VcsChangeRequestState::Open;
        let menu_of = |status: VcsStatusResult| {
            let menu = menu(Some(&status), false);
            (menu.create_pr, menu.view_pr)
        };
        assert_eq!(menu_of(github(None, 0, 0)), (true, false));
        assert_eq!(menu_of(github(Some(Open), 0, 0)), (false, true));
        assert_eq!(
            menu_of(github(None, 0, 2)),
            (false, false),
            "behind: pull first"
        );
        assert_eq!(
            menu_of(VcsStatusResult {
                is_default_ref: true,
                ..github(None, 1, 0)
            }),
            (false, false)
        );
        assert_eq!(
            menu_of(status(false, true, true, 1, 0)),
            (false, false),
            "no GitHub remote"
        );
    }
}
