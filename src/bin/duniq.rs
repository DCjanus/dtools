use std::io::BufRead;
use std::io::Write;

use ahash::AHashMap;
use clap::Parser;

use dtools::AnyResult;

#[derive(Debug, Parser)]
struct Command {
    /// prefix lines by the number of occurrences
    #[clap(short, long)]
    count: bool,
}

fn main() -> AnyResult {
    let cmd: Command = Parser::parse();

    let print_count = if cmd.count { 0 } else { 1 };

    let mut o = std::io::BufWriter::new(std::io::stdout());
    let mut counter: AHashMap<_, u64> = Default::default();
    for line in std::io::stdin().lock().lines() {
        let line = match line {
            Ok(x) => x,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => Err(e)?,
        };
        let line = std::rc::Rc::new(line);
        let count = counter.entry(line.clone()).or_default();
        *count += 1;

        if *count == print_count {
            writeln!(o, "{}", line.as_str())?;
        }
    }

    if cmd.count {
        for (k, v) in counter.iter() {
            writeln!(o, "{:>8} {}", v, k.as_str())?;
        }
    }
    o.flush()?;

    Ok(())
}
