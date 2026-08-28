use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, bail};
use tempfile::NamedTempFile;

pub mod cargo;
mod cargo_cleanup_tui;
pub mod completion;
pub mod git;
mod git_branch_cleanup_tui;
mod git_worktree_cleanup_tui;
pub mod jwt;
pub mod skill;

pub type AnyResult<T = ()> = anyhow::Result<T>;

/// 排序 UTF-8 文本文件，并使用结果原子替换原文件。
pub fn sort_file(path: &Path, size_limit: u64, uniq: bool, trim_whitespace: bool) -> AnyResult {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to read metadata for {}", path.display()))?;

    if metadata.len() > size_limit {
        bail!(
            "file too large to sort in memory: expected at most {size_limit} bytes, got {} bytes",
            metadata.len()
        );
    }

    let mut lines = BufReader::new(file)
        .lines()
        .collect::<io::Result<Vec<_>>>()
        .with_context(|| format!("failed to read {}", path.display()))?;

    if trim_whitespace {
        lines.iter_mut().for_each(|line| {
            line.truncate(line.trim_end().len());
            let leading_whitespace = line.len() - line.trim_start().len();
            line.drain(..leading_whitespace);
        });
    }

    lines.sort_unstable();
    if uniq {
        lines.dedup();
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create a temporary file in {}", parent.display()))?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        for line in lines {
            writeln!(writer, "{line}").context("failed to write sorted output")?;
        }
        writer.flush().context("failed to flush sorted output")?;
    }

    temporary
        .as_file()
        .sync_all()
        .context("failed to sync sorted output")?;
    temporary
        .as_file()
        .set_permissions(metadata.permissions())
        .context("failed to preserve file permissions")?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", path.display()))?;

    Ok(())
}

/// 输出唯一输入行，可在首次出现时直接输出，也可在统计完成后输出次数。
pub fn uniq_any_order<R: BufRead, W: Write>(
    reader: R,
    mut writer: W,
    count: bool,
) -> io::Result<()> {
    let print_count = u64::from(!count);
    let mut counter = HashMap::<String, u64>::new();

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error),
        };
        let occurrences = counter.entry(line.clone()).or_default();
        *occurrences += 1;

        if *occurrences == print_count {
            writeln!(writer, "{line}")?;
        }
    }

    if count {
        for (line, occurrences) in counter {
            writeln!(writer, "{occurrences:>8} {line}")?;
        }
    }
    writer.flush()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;

    use super::*;

    #[test]
    fn sorts_trims_and_deduplicates_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.txt");
        fs::write(&path, " banana  \npear\n  apple\nbanana\n").unwrap();

        sort_file(&path, 1_000, true, true).unwrap();

        assert_eq!(fs::read_to_string(path).unwrap(), "apple\nbanana\npear\n");
    }

    #[test]
    fn leaves_file_unchanged_when_it_exceeds_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.txt");
        fs::write(&path, "pear\napple\n").unwrap();

        let error = sort_file(&path, 4, false, false).unwrap_err();

        assert!(error.to_string().contains("file too large"));
        assert_eq!(fs::read_to_string(path).unwrap(), "pear\napple\n");
    }

    #[test]
    fn does_not_overwrite_a_legacy_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.txt");
        let legacy_temporary_path = directory.path().join("input.dsort_tmp");
        fs::write(&path, "pear\napple\n").unwrap();
        fs::write(&legacy_temporary_path, "keep me").unwrap();

        sort_file(&path, 1_000, false, false).unwrap();

        assert_eq!(fs::read_to_string(path).unwrap(), "apple\npear\n");
        assert_eq!(
            fs::read_to_string(legacy_temporary_path).unwrap(),
            "keep me"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preserves_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.txt");
        fs::write(&path, "pear\napple\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        sort_file(&path, 1_000, false, false).unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn writes_unique_lines_in_first_seen_order() {
        let mut output = Vec::new();

        uniq_any_order(Cursor::new("pear\napple\npear\n"), &mut output, false).unwrap();

        assert_eq!(String::from_utf8(output).unwrap(), "pear\napple\n");
    }

    #[test]
    fn counts_unique_lines() {
        let mut output = Vec::new();

        uniq_any_order(Cursor::new("pear\napple\npear\n"), &mut output, true).unwrap();

        let mut lines = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        lines.sort();
        assert_eq!(lines, ["       1 apple", "       2 pear"]);
    }
}
