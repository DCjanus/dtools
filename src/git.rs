use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{ErrorKind, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use gix::bstr::ByteSlice;
use gix::remote::Direction;
use gix::{ObjectId, Repository};
use globset::{Glob, GlobSet, GlobSetBuilder};
use tempfile::NamedTempFile;

use crate::AnyResult;

const EXCLUDE_FILE: &str = "dcx/branches-exclude";

#[derive(Debug)]
struct Branch {
    name: String,
    object: ObjectId,
    upstream: String,
    tracking: String,
    worktree: String,
}

#[derive(Debug, Clone)]
struct Base {
    name: String,
    object: ObjectId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteKind {
    Merged,
    Equivalent,
}

#[derive(Debug, Clone)]
pub(crate) struct BaseAudit {
    pub(crate) name: String,
    pub(crate) ahead: usize,
    pub(crate) behind: usize,
    pub(crate) absorption: Option<DeleteKind>,
}

#[derive(Debug, Clone)]
pub(crate) struct BranchAudit {
    pub(crate) name: String,
    pub(crate) short_object: String,
    pub(crate) upstream: String,
    pub(crate) tracking: String,
    pub(crate) worktree: String,
    pub(crate) protected_reason: Option<String>,
    pub(crate) author: String,
    pub(crate) committed_at: String,
    pub(crate) subject: String,
    pub(crate) diff_base: String,
    pub(crate) diffstat: String,
    pub(crate) bases: Vec<BaseAudit>,
}

#[derive(Debug, Clone)]
pub(crate) struct WorktreeAudit {
    pub(crate) path: PathBuf,
    pub(crate) branch: String,
    pub(crate) short_object: String,
    pub(crate) state: WorktreeState,
    pub(crate) protection: Option<WorktreeProtection>,
    pub(crate) subject: String,
    pub(crate) diff_base: String,
    pub(crate) diffstat: String,
    pub(crate) bases: Vec<BaseAudit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorktreeState {
    Clean,
    Dirty,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorktreeProtection {
    Main,
    Current,
    Locked(String),
    Dirty,
}

#[derive(Debug, Default)]
struct ExcludeConfig {
    comments: Vec<String>,
    patterns: BTreeSet<String>,
}

pub fn branch_cleanup(update: bool) -> AnyResult {
    let mut repo = discover_repository()?;
    if update {
        // Fetch remains an explicit network operation. All local inspection and mutation below
        // is performed through gix without spawning Git processes.
        git_fetch()?;
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        bail!("interactive branch selection requires a terminal");
    }

    let audits = branch_audits(&mut repo)?;
    let selected = crate::git_branch_cleanup_tui::select_branches(audits)?;
    if selected.is_empty() {
        return Ok(());
    }
    delete_selected(&repo, &selected)?;
    println!("Deleted {} local branches:", selected.len());
    for branch in selected {
        println!("  {branch}");
    }
    Ok(())
}

pub fn worktree_cleanup() -> AnyResult {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        bail!("interactive worktree selection requires a terminal");
    }

    let repo = discover_repository()?;
    let audits = worktree_audits(&repo)?;
    let selected = crate::git_worktree_cleanup_tui::select_worktrees(audits)?;
    if selected.is_empty() {
        return Ok(());
    }
    remove_selected_worktrees(&repo, &selected)?;
    println!("Removed {} linked worktrees:", selected.len());
    for path in selected {
        println!("  {}", path.display());
    }
    Ok(())
}

fn branch_audits(repo: &mut Repository) -> AnyResult<Vec<BranchAudit>> {
    repo.object_cache_size_if_unset(8 * 1024 * 1024);
    let bases = resolve_audit_bases(repo)?;
    let base_local_names = base_local_names(&bases);
    let excludes = build_glob_set(&load_exclude_config(repo)?.patterns)?;
    let worktrees = checked_out_branches(repo)?;

    local_branches(repo, &worktrees)?
        .into_iter()
        .map(|branch| audit_branch(repo, branch, &bases, &base_local_names, &excludes))
        .collect()
}

fn worktree_audits(repo: &Repository) -> AnyResult<Vec<WorktreeAudit>> {
    let current_path = repo.workdir().map(normalize_path).transpose()?;
    let mut main_repo = repo
        .main_repo()
        .context("failed to open the main repository")?;
    let main_path = main_repo
        .workdir()
        .map(normalize_path)
        .transpose()?
        .context("worktree cleanup requires a non-bare main repository")?;
    let branch_audits = branch_audits(&mut main_repo)?
        .into_iter()
        .map(|audit| (audit.name.clone(), audit))
        .collect::<HashMap<_, _>>();

    let mut audits = vec![audit_worktree(
        &main_repo,
        main_path,
        Some(WorktreeProtection::Main),
        &branch_audits,
    )?];
    for proxy in main_repo.worktrees()? {
        let path = proxy.base().with_context(|| {
            format!(
                "failed to read linked worktree path for {}",
                proxy.id().to_str_lossy()
            )
        })?;
        let locked_reason = proxy
            .lock_reason()
            .map(|reason| WorktreeProtection::Locked(reason.to_str_lossy().into_owned()));
        let linked_repo = proxy
            .into_repo_with_possibly_inaccessible_worktree()
            .with_context(|| format!("failed to open linked worktree at {}", path.display()))?;
        let is_current = current_path
            .as_ref()
            .is_some_and(|current| paths_match(current, &path));
        let protected_reason = if is_current {
            Some(WorktreeProtection::Current)
        } else {
            locked_reason
        };
        audits.push(audit_worktree(
            &linked_repo,
            path,
            protected_reason,
            &branch_audits,
        )?);
    }
    audits.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(audits)
}

fn audit_worktree(
    repo: &Repository,
    path: PathBuf,
    mut protection: Option<WorktreeProtection>,
    branches: &HashMap<String, BranchAudit>,
) -> AnyResult<WorktreeAudit> {
    let exists = path.is_dir();
    let dirty = exists && worktree_is_dirty(&path)?;
    if dirty && protection.is_none() {
        protection = Some(WorktreeProtection::Dirty);
    }
    let state = if !exists {
        WorktreeState::Missing
    } else if dirty {
        WorktreeState::Dirty
    } else {
        WorktreeState::Clean
    };
    let branch = repo
        .head_name()?
        .map(|name| name.shorten().to_str_lossy().into_owned())
        .unwrap_or_else(|| "(detached HEAD)".to_owned());
    let commit = repo.head_commit().context("worktree HEAD is unborn")?;
    let short_object = commit.short_id()?.to_string();
    let subject = commit.message()?.title.to_str_lossy().trim().to_owned();
    let branch_audit = branches.get(&branch);

    Ok(WorktreeAudit {
        path,
        branch,
        short_object,
        state,
        protection,
        subject,
        diff_base: branch_audit
            .map(|audit| audit.diff_base.clone())
            .unwrap_or_else(|| "base".to_owned()),
        diffstat: branch_audit
            .map(|audit| audit.diffstat.clone())
            .unwrap_or_else(|| "detached HEAD; no branch audit available".to_owned()),
        bases: branch_audit
            .map(|audit| audit.bases.clone())
            .unwrap_or_default(),
    })
}

fn worktree_is_dirty(path: &Path) -> AnyResult<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
        .with_context(|| format!("failed to inspect worktree at {}", path.display()))?;
    if !output.status.success() {
        bail!(
            "git status failed for {} with {}: {}",
            path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(!output.stdout.is_empty())
}

fn remove_selected_worktrees(repo: &Repository, paths: &[PathBuf]) -> AnyResult {
    let audits = worktree_audits(repo)?;
    for path in paths {
        let audit = audits
            .iter()
            .find(|audit| paths_match(&audit.path, path))
            .with_context(|| {
                format!(
                    "linked worktree is no longer registered: {}",
                    path.display()
                )
            })?;
        if let Some(protection) = &audit.protection {
            bail!(
                "refusing to remove protected worktree {}: {}",
                path.display(),
                protection_description(protection)
            );
        }
    }

    let main_repo = repo
        .main_repo()
        .context("failed to open the main repository")?;
    let main_path = main_repo
        .workdir()
        .context("worktree cleanup requires a non-bare main repository")?;
    for path in paths {
        let output = Command::new("git")
            .arg("-C")
            .arg(main_path)
            .args(["worktree", "remove", "--"])
            .arg(path)
            .output()
            .with_context(|| format!("failed to remove worktree at {}", path.display()))?;
        if !output.status.success() {
            bail!(
                "git worktree remove failed for {} with {}: {}",
                path.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }
    Ok(())
}

fn protection_description(protection: &WorktreeProtection) -> String {
    match protection {
        WorktreeProtection::Main => "main worktree".to_owned(),
        WorktreeProtection::Current => "current worktree".to_owned(),
        WorktreeProtection::Locked(reason) if reason.is_empty() => "locked worktree".to_owned(),
        WorktreeProtection::Locked(reason) => format!("locked worktree: {reason}"),
        WorktreeProtection::Dirty => "worktree has local changes".to_owned(),
    }
}

fn normalize_path(path: &Path) -> AnyResult<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("failed to resolve worktree path {}", path.display()))
}

fn paths_match(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn resolve_audit_bases(repo: &Repository) -> AnyResult<Vec<Base>> {
    let mut bases = BTreeMap::<String, ObjectId>::new();
    for reference in repo.references()?.remote_branches()? {
        let mut reference = reference.map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let name = reference.name().as_bstr().to_str_lossy().into_owned();
        if !name.ends_with("/HEAD") {
            continue;
        }
        let target_name = match reference.target() {
            gix::refs::TargetRef::Symbolic(name) => name.shorten().to_str_lossy().into_owned(),
            gix::refs::TargetRef::Object(_) => name.trim_end_matches("/HEAD").to_owned(),
        };
        let target = reference.peel_to_id()?.detach();
        bases.insert(target_name, target);
    }

    if let Some(head_name) = repo.head_name()?
        && let Some(upstream) =
            repo.branch_remote_tracking_ref_name(head_name.as_ref(), Direction::Fetch)
    {
        let upstream = upstream?;
        if let Ok(mut reference) = repo.find_reference(upstream.as_ref()) {
            bases.insert(
                upstream.shorten().to_str_lossy().into_owned(),
                reference.peel_to_id()?.detach(),
            );
        }
    }
    Ok(bases
        .into_iter()
        .map(|(name, object)| Base { name, object })
        .collect())
}

fn audit_branch(
    repo: &Repository,
    branch: Branch,
    bases: &[Base],
    base_local_names: &HashSet<String>,
    excludes: &GlobSet,
) -> AnyResult<BranchAudit> {
    let protected_reason = if base_local_names.contains(&branch.name) {
        Some("base branch".to_owned())
    } else if !branch.worktree.is_empty() {
        Some("checked out in a worktree".to_owned())
    } else if excludes.is_match(&branch.name) {
        Some("excluded by rule".to_owned())
    } else {
        None
    };

    let commit = repo.find_commit(branch.object)?;
    let author = commit.author()?.name.to_str_lossy().into_owned();
    let committed_at = commit
        .time()?
        .format(gix::date::time::format::ISO8601_STRICT)?;
    let subject = commit.message()?.title.to_str_lossy().trim().to_owned();
    let short_object = commit.short_id()?.to_string();

    let mut base_audits = Vec::new();
    for base in bases {
        let (ahead, behind, merge_base) = ahead_behind(repo, branch.object, base.object)?;
        let absorption = changes_absorbed(repo, branch.object, base.object, merge_base)?;
        base_audits.push(BaseAudit {
            name: base.name.clone(),
            ahead,
            behind,
            absorption,
        });
    }
    let diff_base = base_audits
        .iter()
        .find(|base| base.absorption.is_some())
        .or_else(|| {
            base_audits
                .iter()
                .min_by_key(|base| base.ahead.saturating_add(base.behind))
        })
        .map(|base| base.name.clone());
    let diffstat = match diff_base
        .as_deref()
        .and_then(|name| bases.iter().find(|base| base.name == name))
    {
        Some(base) => branch_diffstat(repo, branch.object, base.object)?
            .unwrap_or_else(|| "与审计基准没有共同历史".to_owned()),
        None => "没有可用的审计基准".to_owned(),
    };

    Ok(BranchAudit {
        name: branch.name,
        short_object,
        upstream: branch.upstream,
        tracking: branch.tracking,
        worktree: branch.worktree,
        protected_reason,
        author,
        committed_at,
        subject,
        diff_base: diff_base.unwrap_or_else(|| "base".to_owned()),
        diffstat,
        bases: base_audits,
    })
}

fn ahead_behind(
    repo: &Repository,
    branch: ObjectId,
    base: ObjectId,
) -> AnyResult<(usize, usize, Option<ObjectId>)> {
    let merge_base = repo.merge_base(branch, base).ok().map(|id| id.detach());
    let ahead = revision_count_excluding(repo, branch, base)?;
    let behind = revision_count_excluding(repo, base, branch)?;
    Ok((ahead, behind, merge_base))
}

fn revision_count_excluding(
    repo: &Repository,
    tip: ObjectId,
    hidden: ObjectId,
) -> AnyResult<usize> {
    repo.rev_walk([tip])
        .with_hidden([hidden])
        .all()?
        .try_fold(0_usize, |count, item| item.map(|_| count + 1))
        .map_err(Into::into)
}

fn changes_absorbed(
    repo: &Repository,
    branch: ObjectId,
    base: ObjectId,
    merge_base: Option<ObjectId>,
) -> AnyResult<Option<DeleteKind>> {
    let Some(merge_base) = merge_base else {
        return Ok(None);
    };
    if merge_base == branch {
        return Ok(Some(DeleteKind::Merged));
    }

    let branch_tree = repo.find_commit(branch)?.tree_id()?.detach();
    for item in repo.rev_walk([base]).with_hidden([merge_base]).all()? {
        let commit = item?.object()?;
        if commit.tree_id()?.detach() == branch_tree {
            return Ok(Some(DeleteKind::Equivalent));
        }
    }
    Ok(None)
}

fn branch_diffstat(
    repo: &Repository,
    branch: ObjectId,
    base: ObjectId,
) -> AnyResult<Option<String>> {
    let Ok(merge_base) = repo.merge_base(branch, base) else {
        return Ok(None);
    };
    let old_tree = repo.find_commit(merge_base)?.tree()?;
    let new_tree = repo.find_commit(branch)?.tree()?;
    let files = repo.diff_tree_to_tree(&old_tree, &new_tree, None)?.len();
    let mut changes = old_tree.changes()?;
    changes.options(|options| {
        options.track_rewrites(None);
    });
    let stats = changes.stats(&new_tree)?;
    let binaries = files.saturating_sub(stats.files_changed as usize);
    Ok(Some(if files == 0 {
        "无内容差异".to_owned()
    } else {
        let mut summary = format!(
            "{files} 个文件变更，新增 {} 行，删除 {} 行",
            stats.lines_added, stats.lines_removed
        );
        if binaries > 0 {
            summary.push_str(&format!("，其中 {binaries} 个二进制文件"));
        }
        summary
    }))
}

fn delete_selected(repo: &Repository, branches: &[String]) -> AnyResult {
    let base_local_names = base_local_names(&resolve_audit_bases(repo)?);
    let excludes = build_glob_set(&load_exclude_config(repo)?.patterns)?;
    let worktrees = checked_out_branches(repo)?;
    for branch in branches {
        if base_local_names.contains(branch) {
            bail!("refusing to delete base branch {branch}");
        }
        if let Some(path) = worktrees.get(branch) {
            bail!("refusing to delete branch {branch}; checked out at {path}");
        }
        if excludes.is_match(branch) {
            bail!("refusing to delete excluded branch {branch}");
        }
        let reference_name = format!("refs/heads/{branch}");
        repo.find_reference(reference_name.as_str())?
            .delete()
            .with_context(|| format!("failed to delete local branch {branch}"))?;
    }
    Ok(())
}

pub fn exclude_add(patterns: &[String], current: bool) -> AnyResult {
    let repo = discover_repository()?;
    let mut config = load_exclude_config(&repo)?;
    for pattern in patterns {
        validate_pattern(pattern)?;
        config.patterns.insert(pattern.to_owned());
    }
    if current {
        let branch = repo
            .head_name()?
            .context("HEAD is detached; --current requires a checked-out branch")?
            .shorten()
            .to_str_lossy()
            .into_owned();
        validate_pattern(&branch)?;
        config.patterns.insert(branch);
    }
    if patterns.is_empty() && !current {
        bail!("provide at least one pattern or use --current");
    }
    write_exclude_config(&repo, &config)
}

pub fn exclude_remove(patterns: &[String]) -> AnyResult {
    let repo = discover_repository()?;
    let mut config = load_exclude_config(&repo)?;
    for pattern in patterns {
        config.patterns.remove(pattern);
    }
    write_exclude_config(&repo, &config)
}

pub fn exclude_list() -> AnyResult {
    let repo = discover_repository()?;
    for pattern in load_exclude_config(&repo)?.patterns {
        println!("{pattern}");
    }
    Ok(())
}

fn discover_repository() -> AnyResult<Repository> {
    gix::discover(".").context("not inside a Git repository")
}

fn base_local_names(bases: &[Base]) -> HashSet<String> {
    bases
        .iter()
        .filter_map(|base| base.name.split_once('/').map(|(_, name)| name.to_owned()))
        .collect()
}

fn local_branches(
    repo: &Repository,
    worktrees: &HashMap<String, String>,
) -> AnyResult<Vec<Branch>> {
    let mut branches = Vec::new();
    for reference in repo.references()?.local_branches()?.peeled()? {
        let reference = reference.map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let name = reference.name().shorten().to_str_lossy().into_owned();
        let object = reference.id().detach();
        let upstream_ref = reference
            .remote_tracking_ref_name(Direction::Fetch)
            .transpose()?;
        let upstream = upstream_ref
            .as_ref()
            .map(|name| name.shorten().to_str_lossy().into_owned())
            .unwrap_or_default();
        let tracking = match upstream_ref {
            Some(upstream_ref) => match repo.try_find_reference(upstream_ref.as_ref())? {
                Some(mut upstream_reference) => {
                    let upstream_object = upstream_reference.peel_to_id()?.detach();
                    let (ahead, behind, _) = ahead_behind(repo, object, upstream_object)?;
                    tracking_label(ahead, behind)
                }
                None => "[gone]".to_owned(),
            },
            None => String::new(),
        };
        branches.push(Branch {
            name: name.clone(),
            object,
            upstream,
            tracking,
            worktree: worktrees.get(&name).cloned().unwrap_or_default(),
        });
    }
    branches.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(branches)
}

fn tracking_label(ahead: usize, behind: usize) -> String {
    match (ahead, behind) {
        (0, 0) => String::new(),
        (ahead, 0) => format!("[ahead {ahead}]"),
        (0, behind) => format!("[behind {behind}]"),
        (ahead, behind) => format!("[ahead {ahead}, behind {behind}]"),
    }
}

fn checked_out_branches(repo: &Repository) -> AnyResult<HashMap<String, String>> {
    let mut result = HashMap::new();
    add_checked_out_branch(repo, &mut result)?;
    for proxy in repo.worktrees()? {
        let path = proxy.base().ok();
        let linked = proxy.into_repo_with_possibly_inaccessible_worktree()?;
        add_checked_out_branch_with_path(&linked, path, &mut result)?;
    }
    Ok(result)
}

fn add_checked_out_branch(repo: &Repository, result: &mut HashMap<String, String>) -> AnyResult {
    add_checked_out_branch_with_path(repo, repo.workdir().map(PathBuf::from), result)
}

fn add_checked_out_branch_with_path(
    repo: &Repository,
    path: Option<PathBuf>,
    result: &mut HashMap<String, String>,
) -> AnyResult {
    if let (Some(name), Some(path)) = (repo.head_name()?, path) {
        result.insert(
            name.shorten().to_str_lossy().into_owned(),
            path.display().to_string(),
        );
    }
    Ok(())
}

fn load_exclude_config(repo: &Repository) -> AnyResult<ExcludeConfig> {
    let path = exclude_path(repo);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(ExcludeConfig::default()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read exclude file {}", path.display()));
        }
    };
    let mut config = ExcludeConfig::default();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            config.comments.push(line.to_owned());
            continue;
        }
        validate_pattern(line).with_context(|| format!("invalid pattern in {}", path.display()))?;
        config.patterns.insert(line.to_owned());
    }
    Ok(config)
}

fn write_exclude_config(repo: &Repository, config: &ExcludeConfig) -> AnyResult {
    let path = exclude_path(repo);
    let parent = path.parent().context("exclude file has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let mut contents = String::new();
    for comment in &config.comments {
        contents.push_str(comment);
        contents.push('\n');
    }
    if !config.comments.is_empty() && !config.patterns.is_empty() {
        contents.push('\n');
    }
    for pattern in &config.patterns {
        contents.push_str(pattern);
        contents.push('\n');
    }
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create a temporary file in {}", parent.display()))?;
    temporary
        .write_all(contents.as_bytes())
        .with_context(|| format!("failed to write exclude file {}", path.display()))?;
    temporary
        .flush()
        .with_context(|| format!("failed to flush exclude file {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to sync exclude file {}", path.display()))?;
    temporary
        .persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace exclude file {}", path.display()))?;
    Ok(())
}

fn exclude_path(repo: &Repository) -> PathBuf {
    repo.common_dir().join(EXCLUDE_FILE)
}

fn validate_pattern(pattern: &str) -> AnyResult {
    if pattern.trim() != pattern || pattern.is_empty() || pattern.starts_with('#') {
        bail!("invalid exclude pattern: {pattern:?}");
    }
    Glob::new(pattern).with_context(|| format!("invalid exclude pattern {pattern:?}"))?;
    Ok(())
}

fn build_glob_set(patterns: &BTreeSet<String>) -> AnyResult<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            Glob::new(pattern).with_context(|| format!("invalid exclude pattern {pattern:?}"))?,
        );
    }
    builder.build().context("failed to build exclude patterns")
}

fn git_fetch() -> AnyResult {
    let output = Command::new("git")
        .args(["fetch", "--all", "--prune"])
        .output()
        .context("failed to run git fetch --all --prune")?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "git fetch --all --prune failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::*;

    fn git(directory: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn repository() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "--quiet", "--initial-branch=main"]);
        git(root.path(), &["config", "user.name", "Test User"]);
        git(root.path(), &["config", "user.email", "test@example.com"]);
        fs::write(root.path().join("file.txt"), "base\n").unwrap();
        git(root.path(), &["add", "file.txt"]);
        git(root.path(), &["commit", "--quiet", "-m", "base"]);
        root
    }

    #[test]
    fn recognizes_regularly_merged_branches() {
        let root = repository();
        git(root.path(), &["switch", "--quiet", "-c", "topic"]);
        fs::write(root.path().join("file.txt"), "topic\n").unwrap();
        git(root.path(), &["commit", "--quiet", "-am", "topic"]);
        let topic = git(root.path(), &["rev-parse", "HEAD"])
            .parse::<ObjectId>()
            .unwrap();
        git(root.path(), &["switch", "--quiet", "main"]);
        git(root.path(), &["merge", "--quiet", "--no-ff", "topic"]);
        let base = git(root.path(), &["rev-parse", "HEAD"])
            .parse::<ObjectId>()
            .unwrap();

        let repo = gix::open(root.path()).unwrap();
        let merge_base = repo.merge_base(topic, base).ok().map(|id| id.detach());
        assert_eq!(
            changes_absorbed(&repo, topic, base, merge_base).unwrap(),
            Some(DeleteKind::Merged)
        );
    }

    #[test]
    fn recognizes_squash_merges_with_an_exact_historical_tree() {
        let root = repository();
        git(root.path(), &["switch", "--quiet", "-c", "topic"]);
        fs::write(root.path().join("file.txt"), "topic\n").unwrap();
        git(root.path(), &["commit", "--quiet", "-am", "topic"]);
        let topic = git(root.path(), &["rev-parse", "HEAD"])
            .parse::<ObjectId>()
            .unwrap();
        git(root.path(), &["switch", "--quiet", "main"]);
        git(root.path(), &["merge", "--quiet", "--squash", "topic"]);
        git(root.path(), &["commit", "--quiet", "-m", "squash topic"]);
        let base = git(root.path(), &["rev-parse", "HEAD"])
            .parse::<ObjectId>()
            .unwrap();

        let repo = gix::open(root.path()).unwrap();
        let merge_base = repo.merge_base(topic, base).ok().map(|id| id.detach());
        assert_eq!(
            changes_absorbed(&repo, topic, base, merge_base).unwrap(),
            Some(DeleteKind::Equivalent)
        );
    }

    #[test]
    fn reports_a_configured_but_missing_upstream_as_gone() {
        let root = repository();
        git(root.path(), &["remote", "add", "origin", "."]);
        git(root.path(), &["config", "branch.main.remote", "origin"]);
        git(
            root.path(),
            &["config", "branch.main.merge", "refs/heads/main"],
        );

        let repo = gix::open(root.path()).unwrap();
        let branches = local_branches(&repo, &HashMap::new()).unwrap();
        assert_eq!(branches[0].upstream, "origin/main");
        assert_eq!(branches[0].tracking, "[gone]");
    }

    #[test]
    fn discovers_the_remote_default_branch_as_an_audit_base() {
        let root = repository();
        git(root.path(), &["remote", "add", "origin", "."]);
        git(root.path(), &["fetch", "--quiet", "origin", "main"]);
        git(
            root.path(),
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );

        let repo = gix::open(root.path()).unwrap();
        let bases = resolve_audit_bases(&repo).unwrap();
        assert_eq!(bases.len(), 1);
        assert_eq!(bases[0].name, "origin/main");
        assert!(base_local_names(&bases).contains("main"));
    }

    #[test]
    fn deletes_an_unprotected_ref_but_refuses_the_checked_out_branch() {
        let root = repository();
        git(root.path(), &["branch", "topic"]);
        let repo = gix::open(root.path()).unwrap();

        delete_selected(&repo, &["topic".to_owned()]).unwrap();
        assert!(
            repo.try_find_reference("refs/heads/topic")
                .unwrap()
                .is_none()
        );

        let error = delete_selected(&repo, &["main".to_owned()]).unwrap_err();
        assert!(error.to_string().contains("checked out"));
        assert!(
            repo.try_find_reference("refs/heads/main")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn audits_worktree_safety_before_allowing_removal() {
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("main");
        let clean = root.path().join("clean");
        let dirty = root.path().join("dirty");
        fs::create_dir(&main).unwrap();
        git(&main, &["init", "--quiet", "--initial-branch=main"]);
        git(&main, &["config", "user.name", "Test User"]);
        git(&main, &["config", "user.email", "test@example.com"]);
        fs::write(main.join("file.txt"), "base\n").unwrap();
        git(&main, &["add", "file.txt"]);
        git(&main, &["commit", "--quiet", "-m", "base"]);
        git(&main, &["branch", "clean"]);
        git(&main, &["branch", "dirty"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                clean.to_str().unwrap(),
                "clean",
            ],
        );
        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                dirty.to_str().unwrap(),
                "dirty",
            ],
        );
        fs::write(dirty.join("file.txt"), "changed\n").unwrap();

        let repo = gix::open(&main).unwrap();
        let audits = worktree_audits(&repo).unwrap();
        let main_audit = audits
            .iter()
            .find(|audit| paths_match(&audit.path, &main))
            .unwrap();
        let clean_audit = audits
            .iter()
            .find(|audit| paths_match(&audit.path, &clean))
            .unwrap();
        let dirty_audit = audits
            .iter()
            .find(|audit| paths_match(&audit.path, &dirty))
            .unwrap();

        assert_eq!(main_audit.protection, Some(WorktreeProtection::Main));
        assert_eq!(clean_audit.state, WorktreeState::Clean);
        assert!(clean_audit.protection.is_none());
        assert_eq!(dirty_audit.state, WorktreeState::Dirty);
        assert_eq!(dirty_audit.protection, Some(WorktreeProtection::Dirty));

        git(
            &main,
            &[
                "worktree",
                "lock",
                "--reason",
                "keep",
                clean.to_str().unwrap(),
            ],
        );
        let audits = worktree_audits(&repo).unwrap();
        let clean_audit = audits
            .iter()
            .find(|audit| paths_match(&audit.path, &clean))
            .unwrap();
        assert_eq!(
            clean_audit.protection,
            Some(WorktreeProtection::Locked("keep".to_owned()))
        );
    }

    #[test]
    fn removes_a_clean_linked_worktree_but_preserves_its_branch() {
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("main");
        let linked = root.path().join("topic");
        fs::create_dir(&main).unwrap();
        git(&main, &["init", "--quiet", "--initial-branch=main"]);
        git(&main, &["config", "user.name", "Test User"]);
        git(&main, &["config", "user.email", "test@example.com"]);
        fs::write(main.join("file.txt"), "base\n").unwrap();
        git(&main, &["add", "file.txt"]);
        git(&main, &["commit", "--quiet", "-m", "base"]);
        git(&main, &["branch", "topic"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                linked.to_str().unwrap(),
                "topic",
            ],
        );

        let repo = gix::open(&main).unwrap();
        remove_selected_worktrees(&repo, std::slice::from_ref(&linked)).unwrap();

        assert!(!linked.exists());
        assert!(repo.worktrees().unwrap().is_empty());
        assert!(
            repo.try_find_reference("refs/heads/topic")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn refuses_to_remove_the_main_worktree_if_its_path_is_injected() {
        let root = repository();
        let repo = gix::open(root.path()).unwrap();

        let error = remove_selected_worktrees(
            &repo,
            std::slice::from_ref(&root.path().canonicalize().unwrap()),
        )
        .unwrap_err();

        assert!(error.to_string().contains("main worktree"));
        assert!(root.path().join(".git").is_dir());
    }

    #[test]
    fn removes_registration_for_a_missing_linked_worktree() {
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("main");
        let linked = root.path().join("topic");
        let moved = root.path().join("topic-moved");
        fs::create_dir(&main).unwrap();
        git(&main, &["init", "--quiet", "--initial-branch=main"]);
        git(&main, &["config", "user.name", "Test User"]);
        git(&main, &["config", "user.email", "test@example.com"]);
        fs::write(main.join("file.txt"), "base\n").unwrap();
        git(&main, &["add", "file.txt"]);
        git(&main, &["commit", "--quiet", "-m", "base"]);
        git(&main, &["branch", "topic"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                linked.to_str().unwrap(),
                "topic",
            ],
        );
        fs::rename(&linked, &moved).unwrap();

        let repo = gix::open(&main).unwrap();
        let audits = worktree_audits(&repo).unwrap();
        let linked_audit = audits
            .iter()
            .find(|audit| audit.state == WorktreeState::Missing)
            .unwrap();
        assert!(linked_audit.protection.is_none());
        let registered_path = linked_audit.path.clone();

        remove_selected_worktrees(&repo, std::slice::from_ref(&registered_path)).unwrap();

        assert!(moved.exists());
        assert!(repo.worktrees().unwrap().is_empty());
        assert!(
            repo.try_find_reference("refs/heads/topic")
                .unwrap()
                .is_some()
        );
    }
}
