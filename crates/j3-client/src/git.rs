//! The git control's primary button and menu, from a project's
//! [`GitStatus`](crate::methods::GitStatus).

use crate::methods::{GitAction, GitStatus, PullRequestState};

/// What the primary button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuickActionKind {
    /// Run `git.run` with this action.
    Run(GitAction),
    /// Pull from upstream.
    Pull,
    /// Open the branch's open pull request.
    OpenPr,
    /// Nothing to do; the text explains why.
    Hint(&'static str),
}

/// The primary button: its label, whether it is disabled, and its action.
/// Hints are disabled; everything else is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QuickAction {
    pub label: &'static str,
    pub disabled: bool,
    pub kind: QuickActionKind,
}

impl QuickAction {
    fn run(label: &'static str, action: GitAction) -> Self {
        Self {
            label,
            disabled: false,
            kind: QuickActionKind::Run(action),
        }
    }

    fn hint(label: &'static str, hint: &'static str) -> Self {
        Self {
            label,
            disabled: true,
            kind: QuickActionKind::Hint(hint),
        }
    }
}

fn open_pr(status: &GitStatus) -> bool {
    status
        .pull_request
        .as_ref()
        .is_some_and(|pr| pr.state == PullRequestState::Open)
}

/// A pull request can be created: a GitHub remote, a branch other than the
/// default one, and no open pull request yet.
fn can_pr(status: &GitStatus) -> bool {
    status.github
        && status.branch.is_some()
        && !status.on_default_branch
        && status.has_remote
        && !open_pr(status)
}

/// The primary button for `status` (None while unknown); `busy` while a git
/// action runs. First match: busy; no repository; detached HEAD; uncommitted
/// changes (commit, with push and PR where possible); no upstream (push, with
/// PR where possible); diverged; behind (pull); ahead (push, with PR where
/// possible); an open PR (view it); a PR to create; else up to date.
pub fn quick_action(status: Option<&GitStatus>, busy: bool) -> QuickAction {
    if busy {
        return QuickAction::hint("Commit", "Git action in progress.");
    }
    let Some(status) = status.filter(|status| status.is_repo) else {
        return QuickAction::hint("Commit", "Git status is unavailable.");
    };
    if status.branch.is_none() {
        return QuickAction::hint("Commit", "Create and checkout a branch before pushing.");
    }
    let can_pr = can_pr(status);
    if status.dirty {
        return if !status.has_upstream && !status.has_remote {
            QuickAction::run("Commit", GitAction::Commit)
        } else if can_pr {
            QuickAction::run("Commit, push & PR", GitAction::CommitPushPr)
        } else {
            QuickAction::run("Commit & push", GitAction::CommitPush)
        };
    }
    if !status.has_upstream {
        return if !status.has_remote {
            QuickAction::hint("Commit", "No changes to commit.")
        } else if status.ahead == 0 {
            QuickAction::hint("Push", "No local commits to push.")
        } else if can_pr {
            QuickAction::run("Push & create PR", GitAction::Pr)
        } else {
            QuickAction::run("Push", GitAction::Push)
        };
    }
    if status.ahead > 0 && status.behind > 0 {
        return QuickAction::hint(
            "Sync branch",
            "Branch has diverged from upstream. Rebase/merge first.",
        );
    }
    if status.behind > 0 {
        return QuickAction {
            label: "Pull",
            disabled: false,
            kind: QuickActionKind::Pull,
        };
    }
    if status.ahead > 0 {
        return if can_pr {
            QuickAction::run("Push & create PR", GitAction::Pr)
        } else {
            QuickAction::run("Push", GitAction::Push)
        };
    }
    if open_pr(status) {
        return QuickAction {
            label: "View PR",
            disabled: false,
            kind: QuickActionKind::OpenPr,
        };
    }
    if can_pr && status.pull_request.is_none() {
        return QuickAction::run("Create PR", GitAction::Pr);
    }
    QuickAction::hint("Commit", "Branch is up to date. No action needed.")
}

/// Which git menu items are enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct GitMenu {
    pub commit: bool,
    pub push: bool,
    pub pull: bool,
    pub create_pr: bool,
    pub view_pr: bool,
}

/// The git menu for `status`; everything disabled while `busy` or unknown.
/// Commit needs changes; push a branch with local commits, nothing to pull,
/// and somewhere to push; pull an upstream that is ahead; create PR a branch
/// that can have one, nothing to pull and no uncommitted changes; view PR an
/// open pull request.
pub fn menu(status: Option<&GitStatus>, busy: bool) -> GitMenu {
    let Some(status) = status.filter(|_| !busy) else {
        return GitMenu::default();
    };
    GitMenu {
        commit: status.dirty,
        push: status.branch.is_some()
            && status.behind == 0
            && status.ahead > 0
            && (status.has_upstream || status.has_remote),
        pull: status.has_upstream && status.behind > 0,
        create_pr: can_pr(status) && status.behind == 0 && !status.dirty,
        view_pr: open_pr(status),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::methods::PullRequest;

    /// A clean feature branch tracking a GitHub upstream, up to date.
    fn clean() -> GitStatus {
        GitStatus {
            is_repo: true,
            branch: Some("feature".to_owned()),
            on_default_branch: false,
            has_remote: true,
            github: true,
            has_upstream: true,
            ahead: 0,
            behind: 0,
            dirty: false,
            files: Vec::new(),
            additions: 0,
            deletions: 0,
            pull_request: None,
        }
    }

    fn with(patch: impl FnOnce(&mut GitStatus)) -> GitStatus {
        let mut status = clean();
        patch(&mut status);
        status
    }

    fn pr(state: PullRequestState) -> Option<PullRequest> {
        Some(PullRequest {
            url: "https://example.com/pr/1".to_owned(),
            state,
        })
    }

    fn kind(status: &GitStatus) -> (&'static str, bool, QuickActionKind) {
        let action = quick_action(Some(status), false);
        (action.label, action.disabled, action.kind)
    }

    use QuickActionKind::{Hint, OpenPr, Pull, Run};

    #[test]
    fn busy_and_unknown_states_are_hints() {
        assert_eq!(
            quick_action(Some(&clean()), true),
            QuickAction {
                label: "Commit",
                disabled: true,
                kind: Hint("Git action in progress.")
            }
        );
        assert_eq!(
            kind(&with(|s| s.is_repo = false)),
            ("Commit", true, Hint("Git status is unavailable."))
        );
        assert_eq!(
            quick_action(None, false),
            QuickAction {
                label: "Commit",
                disabled: true,
                kind: Hint("Git status is unavailable.")
            }
        );
        assert_eq!(
            kind(&with(|s| s.branch = None)),
            (
                "Commit",
                true,
                Hint("Create and checkout a branch before pushing.")
            )
        );
    }

    #[test]
    fn uncommitted_changes_commit_and_push_where_they_can() {
        let local = with(|s| {
            s.dirty = true;
            s.has_upstream = false;
            s.has_remote = false;
        });
        assert_eq!(kind(&local), ("Commit", false, Run(GitAction::Commit)));
        assert_eq!(
            kind(&with(|s| s.dirty = true)),
            ("Commit, push & PR", false, Run(GitAction::CommitPushPr))
        );
        let default_branch = with(|s| {
            s.dirty = true;
            s.on_default_branch = true;
        });
        assert_eq!(
            kind(&default_branch),
            ("Commit & push", false, Run(GitAction::CommitPush))
        );
        let not_github = with(|s| {
            s.dirty = true;
            s.github = false;
        });
        assert_eq!(
            kind(&not_github),
            ("Commit & push", false, Run(GitAction::CommitPush))
        );
        let open = with(|s| {
            s.dirty = true;
            s.pull_request = pr(PullRequestState::Open);
        });
        assert_eq!(
            kind(&open),
            ("Commit & push", false, Run(GitAction::CommitPush))
        );
    }

    #[test]
    fn a_branch_without_upstream_pushes_once_it_has_commits() {
        let no_remote = with(|s| {
            s.has_upstream = false;
            s.has_remote = false;
        });
        assert_eq!(
            kind(&no_remote),
            ("Commit", true, Hint("No changes to commit."))
        );
        assert_eq!(
            kind(&with(|s| s.has_upstream = false)),
            ("Push", true, Hint("No local commits to push."))
        );
        let ahead = with(|s| {
            s.has_upstream = false;
            s.ahead = 2;
        });
        assert_eq!(
            kind(&ahead),
            ("Push & create PR", false, Run(GitAction::Pr))
        );
        let ahead_on_default = with(|s| {
            s.has_upstream = false;
            s.ahead = 2;
            s.on_default_branch = true;
        });
        assert_eq!(
            kind(&ahead_on_default),
            ("Push", false, Run(GitAction::Push))
        );
    }

    #[test]
    fn tracking_branches_sync_pull_or_push() {
        let diverged = with(|s| {
            s.ahead = 1;
            s.behind = 1;
        });
        assert_eq!(
            kind(&diverged),
            (
                "Sync branch",
                true,
                Hint("Branch has diverged from upstream. Rebase/merge first.")
            )
        );
        assert_eq!(kind(&with(|s| s.behind = 3)), ("Pull", false, Pull));
        assert_eq!(
            kind(&with(|s| s.ahead = 1)),
            ("Push & create PR", false, Run(GitAction::Pr))
        );
        let ahead_with_pr = with(|s| {
            s.ahead = 1;
            s.pull_request = pr(PullRequestState::Open);
        });
        assert_eq!(kind(&ahead_with_pr), ("Push", false, Run(GitAction::Push)));
    }

    #[test]
    fn up_to_date_branches_view_or_create_a_pr() {
        assert_eq!(
            kind(&with(|s| s.pull_request = pr(PullRequestState::Open))),
            ("View PR", false, OpenPr)
        );
        assert_eq!(kind(&clean()), ("Create PR", false, Run(GitAction::Pr)));
        let merged = with(|s| s.pull_request = pr(PullRequestState::Merged));
        assert_eq!(
            kind(&merged),
            (
                "Commit",
                true,
                Hint("Branch is up to date. No action needed.")
            )
        );
        let default_branch = with(|s| s.on_default_branch = true);
        assert_eq!(
            kind(&default_branch),
            (
                "Commit",
                true,
                Hint("Branch is up to date. No action needed.")
            )
        );
    }

    #[test]
    fn menu_items() {
        assert_eq!(menu(None, false), GitMenu::default());
        assert_eq!(
            menu(Some(&with(|s| s.dirty = true)), true),
            GitMenu::default()
        );

        let dirty = menu(Some(&with(|s| s.dirty = true)), false);
        assert_eq!(
            dirty,
            GitMenu {
                commit: true,
                ..GitMenu::default()
            }
        );

        let ahead = menu(Some(&with(|s| s.ahead = 2)), false);
        assert_eq!(
            ahead,
            GitMenu {
                push: true,
                create_pr: true,
                ..GitMenu::default()
            }
        );

        let untracked = with(|s| {
            s.ahead = 2;
            s.has_upstream = false;
            s.has_remote = false;
            s.github = false;
        });
        assert_eq!(
            menu(Some(&untracked), false),
            GitMenu::default(),
            "nowhere to push"
        );

        let behind = menu(Some(&with(|s| s.behind = 1)), false);
        assert_eq!(
            behind,
            GitMenu {
                pull: true,
                ..GitMenu::default()
            }
        );

        let detached = with(|s| {
            s.branch = None;
            s.ahead = 1;
        });
        assert_eq!(menu(Some(&detached), false), GitMenu::default());

        let open = menu(
            Some(&with(|s| s.pull_request = pr(PullRequestState::Open))),
            false,
        );
        assert_eq!(
            open,
            GitMenu {
                view_pr: true,
                ..GitMenu::default()
            }
        );
    }
}
