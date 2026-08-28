mod common;

use std::fs;

use common::{dcx, init_repository};

#[test]
fn worktree_cleanup_help_exposes_only_the_interactive_workflow() {
    let root = tempfile::tempdir().unwrap();
    let skills = root.path().join("skills");

    let output = dcx(&skills)
        .args(["git", "worktree", "cleanup", "--help"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains("--force"));
    assert!(!stdout.contains("--yes"));
}

#[test]
fn worktree_cleanup_requires_an_interactive_terminal() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("repository");
    let skills = root.path().join("skills");
    fs::create_dir(&repository).unwrap();
    init_repository(&repository);

    let output = dcx(&skills)
        .current_dir(&repository)
        .args(["git", "worktree", "cleanup"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("interactive worktree selection requires a terminal")
    );
}
