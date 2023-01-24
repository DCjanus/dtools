use std::io::BufRead;

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
            print!("{}\n", line.as_str());
        }
    }

    if cmd.count {
        for (k, v) in counter.iter() {
            print!("{:>8} {}\n", v, k.as_str());
        }
    }

    Ok(())
}
