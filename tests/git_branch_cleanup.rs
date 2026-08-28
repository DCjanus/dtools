mod common;

use std::fs;

use common::{dcx, init_repository};

#[test]
fn branch_cleanup_help_exposes_only_the_interactive_workflow() {
    let root = tempfile::tempdir().unwrap();
    let skills = root.path().join("skills");

    let output = dcx(&skills)
        .args(["git", "branch", "cleanup", "--help"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--update"));
    assert!(!stdout.contains("--dry-run"));
    assert!(!stdout.contains("--yes"));
    assert!(!stdout.contains("--base"));
}

#[test]
fn legacy_cleanup_command_is_not_available() {
    let root = tempfile::tempdir().unwrap();
    let skills = root.path().join("skills");

    let output = dcx(&skills)
        .args(["git", "cleanup", "--help"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("unrecognized subcommand 'cleanup'")
    );
}

#[test]
fn branch_cleanup_requires_an_interactive_terminal() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("repository");
    let skills = root.path().join("skills");
    fs::create_dir(&repository).unwrap();
    init_repository(&repository);

    let output = dcx(&skills)
        .current_dir(&repository)
        .args(["git", "branch", "cleanup"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("interactive branch selection requires a terminal")
    );
}

#[test]
fn manages_repository_local_exclude_patterns() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("repository");
    let skills = root.path().join("skills");
    fs::create_dir(&repository).unwrap();
    init_repository(&repository);

    let add = dcx(&skills)
        .current_dir(&repository)
        .args([
            "git",
            "branch",
            "cleanup",
            "exclude",
            "add",
            "release/*",
            "develop",
        ])
        .output()
        .unwrap();
    assert!(add.status.success());

    let list = dcx(&skills)
        .current_dir(&repository)
        .args(["git", "branch", "cleanup", "exclude", "list"])
        .output()
        .unwrap();
    assert!(list.status.success());
    assert_eq!(
        String::from_utf8(list.stdout).unwrap(),
        "develop\nrelease/*\n"
    );
    assert_eq!(
        fs::read_to_string(repository.join(".git/dcx/branches-exclude")).unwrap(),
        "develop\nrelease/*\n"
    );

    let remove = dcx(&skills)
        .current_dir(&repository)
        .args(["git", "branch", "cleanup", "exclude", "remove", "develop"])
        .output()
        .unwrap();
    assert!(remove.status.success());
    assert_eq!(
        fs::read_to_string(repository.join(".git/dcx/branches-exclude")).unwrap(),
        "release/*\n"
    );
}
