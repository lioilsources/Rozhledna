//! `rz-latency report <file.csv>...` — P50/P95 per pipeline segment from viewer CSV logs.

use std::fs::File;
use std::io::BufReader;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let files = match args.first().map(String::as_str) {
        Some("report") => &args[1..],
        _ => {
            eprintln!("usage: rz-latency report <file.csv>...");
            std::process::exit(2);
        }
    };
    for f in files {
        let recs = rz_latency::read_csv(BufReader::new(File::open(f)?))?;
        println!("== {f}");
        print!("{}", rz_latency::report(&recs));
    }
    Ok(())
}
