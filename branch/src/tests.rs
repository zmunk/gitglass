use super::*;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

struct Repo(PathBuf);

impl Repo {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gitglass-branch-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        git(&path, &["init", "-b", "main"]).unwrap();
        git(&path, &["config", "user.name", "Test"]).unwrap();
        git(&path, &["config", "user.email", "test@example.com"]).unwrap();
        fs::write(path.join("tracked"), "original\n").unwrap();
        git(&path, &["add", "tracked"]).unwrap();
        git(&path, &["commit", "-m", "initial"]).unwrap();
        git(&path, &["branch", "topic"]).unwrap();
        Self(path)
    }

    fn branch(&self, name: &str) -> Branch {
        load_branches(&self.0)
            .unwrap()
            .into_iter()
            .find(|b| b.name() == name)
            .unwrap()
    }

    fn current(&self) -> String {
        String::from_utf8(git(&self.0, &["symbolic-ref", "--short", "HEAD"]).unwrap())
            .unwrap()
            .trim()
            .to_owned()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn locals_precede_remotes_with_log_recency_order_within_each_group() {
    let repo = Repo::new();
    git(&repo.0, &["switch", "topic"]).unwrap();
    let output = Command::new("git")
        .current_dir(&repo.0)
        .env("GIT_AUTHOR_DATE", "2030-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2030-01-01T00:00:00Z")
        .args(["commit", "--allow-empty", "-m", "newer"])
        .output()
        .unwrap();
    assert!(output.status.success());
    git(
        &repo.0,
        &["update-ref", "refs/remotes/origin/topic", "HEAD"],
    )
    .unwrap();
    let expected = git(
        &repo.0,
        &[
            "--no-pager",
            "branch",
            "--all",
            "--color=never",
            "--sort=-committerdate",
            "--format=%(HEAD) %(refname)",
        ],
    )
    .unwrap();
    let branches = load_branches(&repo.0).unwrap();
    let expected = String::from_utf8(expected).unwrap();
    let mut expected: Vec<&str> = expected.lines().collect();
    expected.sort_by_key(|line| line[2..].starts_with("refs/remotes/"));
    let actual = branches
        .iter()
        .map(|b| format!("{} {}", if b.current { "*" } else { " " }, b.reference))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    assert_eq!(branches[0].name(), "topic");
    assert_eq!(branches[1].name(), "main");
    assert_eq!(branches.last().unwrap().name(), "origin/topic");
    assert!(repo.branch("topic").current);
    assert!(!repo.branch("main").current);
}

#[test]
fn current_branch_has_no_switch_hint_and_switch_is_a_no_op() {
    let repo = Repo::new();
    let current = repo.branch("main");
    assert!(!current.can_switch());
    assert_eq!(
        compact_hints(Some(&current), 2, 5, &[]),
        "q quit • j/k move"
    );
    fs::write(repo.0.join("tracked"), "modified\n").unwrap();
    switch_branch(&repo.0, &current).unwrap();
    assert_eq!(repo.current(), "main");
    assert_eq!(
        fs::read_to_string(repo.0.join("tracked")).unwrap(),
        "modified\n"
    );
}

#[test]
fn clean_switch_succeeds_and_refresh_preserves_selected_reference() {
    let repo = Repo::new();
    switch_branch(&repo.0, &repo.branch("topic")).unwrap();
    assert_eq!(repo.current(), "topic");
    let mut branches = load_branches(&repo.0).unwrap();
    let mut state =
        ListState::default().with_selected(branches.iter().position(|b| b.name() == "topic"));
    git(&repo.0, &["branch", "aaa"]).unwrap();
    refresh(&repo.0, &mut branches, &mut state).unwrap();
    assert_eq!(branches[state.selected().unwrap()].name(), "topic");
    assert!(branches[state.selected().unwrap()].current);
}

#[test]
fn switch_rejects_unstaged_staged_and_untracked_changes() {
    let repo = Repo::new();
    for kind in ["unstaged", "staged", "untracked"] {
        let path = repo.0.join(if kind == "untracked" {
            "untracked"
        } else {
            "tracked"
        });
        fs::write(&path, "modified\n").unwrap();
        if kind == "staged" {
            git(&repo.0, &["add", "tracked"]).unwrap();
        }
        let error = switch_branch(&repo.0, &repo.branch("topic")).unwrap_err();
        assert!(
            error.to_string().contains("uncommitted changes"),
            "{kind}: {error}"
        );
        assert_eq!(repo.current(), "main");
        git(&repo.0, &["reset", "--hard", "HEAD"]).unwrap();
        if kind == "untracked" {
            fs::remove_file(path).unwrap();
        }
    }
}

#[test]
fn remote_branches_cannot_be_switched_to_or_create_local_branches() {
    let repo = Repo::new();
    git(
        &repo.0,
        &["update-ref", "refs/remotes/origin/remote-only", "HEAD"],
    )
    .unwrap();
    let remote = repo.branch("origin/remote-only");
    for dirty in [false, true] {
        if dirty {
            fs::write(repo.0.join("untracked"), "changes").unwrap();
        }
        let error = switch_branch(&repo.0, &remote).unwrap_err();
        assert!(error.to_string().contains("Remote branch"));
        assert_eq!(repo.current(), "main");
        assert!(!load_branches(&repo.0)
            .unwrap()
            .iter()
            .any(|b| b.name() == "remote-only"));
    }
}

#[test]
fn git_switch_failure_is_reported_without_changing_head() {
    let repo = Repo::new();
    let missing = Branch {
        reference: "refs/heads/missing".into(),
        current: false,
    };
    assert!(switch_branch(&repo.0, &missing)
        .unwrap_err()
        .to_string()
        .contains("failed"));
    assert_eq!(repo.current(), "main");
}

#[test]
fn empty_and_detached_repositories_have_no_fake_branches() {
    let repo = Repo::new();
    git(&repo.0, &["switch", "--detach"]).unwrap();
    assert!(load_branches(&repo.0)
        .unwrap()
        .iter()
        .all(|b| !b.current && b.reference.starts_with("refs/heads/")));
    git(&repo.0, &["branch", "-D", "main", "topic"]).unwrap();
    assert!(load_branches(&repo.0).unwrap().is_empty());
}

#[test]
fn compact_hints_follow_selection_and_list_overflow() {
    let local = Branch {
        reference: "refs/heads/topic".into(),
        current: false,
    };
    let remote = Branch {
        reference: "refs/remotes/origin/topic".into(),
        current: false,
    };
    assert_eq!(
        compact_hints(Some(&local), 5, 5, &[]),
        "q quit • j/k move • space switch"
    );
    assert_eq!(compact_hints(Some(&remote), 5, 5, &[]), "q quit • j/k move");
    assert_eq!(compact_hints(None, 0, 5, &[]), "q quit • j/k move");
    assert_eq!(
        compact_hints(Some(&local), 6, 5, &[]),
        "q quit • j/k move • space switch • ctrl-u/d half page"
    );
    assert_eq!(
        compact_hints(Some(&remote), 6, 5, &[]),
        "q quit • j/k move • ctrl-u/d half page"
    );
}

#[test]
fn compact_hints_include_custom_bindings_and_escape_control_characters() {
    let bindings = config::parse_branch_config(
        r#"
[[branch.keybindings]]
key = "y"
command = 'copy "$BRANCH"'
description = "Copy\nbranch"
[[branch.keybindings]]
key = "f"
command = "git fetch"
"#,
    )
    .unwrap();
    assert_eq!(
        compact_hints(None, 0, 5, &bindings),
        r"q quit • j/k move • y  Copy\nbranch • f  git fetch"
    );
}

#[test]
fn navigation_clamps_at_boundaries_and_handles_empty_lists() {
    let mut state = ListState::default();
    navigate(&mut state, 10, true, 4);
    assert_eq!(state.selected(), Some(4));
    navigate(&mut state, 10, true, 100);
    assert_eq!(state.selected(), Some(9));
    navigate(&mut state, 10, false, 100);
    assert_eq!(state.selected(), Some(0));
    navigate(&mut state, 0, true, 1);
    assert_eq!(state.selected(), None);
}
