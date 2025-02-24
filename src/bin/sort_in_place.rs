use anyhow::{Context, bail};
use clap::Parser;
use dtools::AnyResult;
use size::Size;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

#[derive(Debug, Parser)]
struct Command {
    /// Sort the file in place
    path: PathBuf,
    /// The maximum size of the file to be sorted in memory
    #[clap(long, default_value = "1GB")]
    size_limit: Size,
}

fn main() -> AnyResult {
    let cmd: Command = Parser::parse();
    let file = std::fs::File::open(&cmd.path).context("Failed to open file")?;
    let meta = file.metadata().context("Failed to get file metadata")?;
    if meta.len() > cmd.size_limit.bytes() as u64 {
        bail!(
            "File too large to sort in memory, expected at most {}, got {}",
            cmd.size_limit,
            Size::from_bytes(meta.len())
        );
    }

    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    for line in reader.lines() {
        let line = line.context("Failed to read line")?;
        lines.push(line);
    }
    lines.sort();

    let mut tmp_path = cmd.path.clone();
    tmp_path.set_extension("dsort_tmp");
    let mut writer = std::fs::File::create(&tmp_path).context("Failed to create temp file")?;
    for line in lines {
        writeln!(writer, "{}", line).context("Failed to write line")?;
    }
    writer.flush().context("Failed to flush temp file")?;

    std::fs::rename(&tmp_path, &cmd.path).context("Failed to rename temp file")?;
    Ok(())
}
