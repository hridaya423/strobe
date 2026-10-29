use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use vrf_bench::{corpus, engines::Engine, run};

#[derive(Parser)]
#[command(name = "vrf-bench")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Run {
        #[arg(long, value_delimiter = ',', required = true)]
        engine: Vec<Engine>,
        #[arg(long, default_value = "all")]
        corpus: String,
        #[arg(long, default_value_t = 5)]
        runs: usize,
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run { engine, corpus, runs, out_dir } => {
            let out_dir = out_dir.unwrap_or_else(|| std::env::temp_dir().join("vrf-bench"));
            let date = jiff::Zoned::now().date();
            std::fs::create_dir_all(run::results_dir())?;
            for c in corpus::select(&corpus)? {
                for &e in engine.iter().filter(|e| e.supports(c.framework)) {
                    let r = run::run(e, &c, runs, &out_dir)?;
                    let path = run::results_dir().join(format!("{date}-{}-{}.json", r.engine, r.corpus_id));
                    std::fs::write(&path, serde_json::to_string_pretty(&r)?)?;
                    println!("{:<14} {:<24} {:>8.3} s {:>8.1} fps", r.engine, r.corpus_id, r.median_s, r.fps);
                }
            }
        }
    }
    Ok(())
}
