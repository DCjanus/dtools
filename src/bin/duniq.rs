use std::io::BufRead;

use ahash::AHashMap;
use clap::Clap;

use dtools::AnyResult;

#[derive(Debug, Clap)]
struct Command {}

fn main() -> AnyResult {
    let cmd: Command = Clap::parse();
    let mut counter: AHashMap<_, u64> = Default::default();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let line = std::rc::Rc::new(line);
        let count = counter.entry(line.clone()).or_default();
        *count += 1;

        if *count == 1 {
            print!("{}\n", line.as_str());
        }
    }

    Ok(())
}
