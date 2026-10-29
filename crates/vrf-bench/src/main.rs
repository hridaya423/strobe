use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use vrf_bench::{corpus, engines::Engine, refs, run, score};

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
    Refs {
        #[arg(long, default_value = "all")]
        corpus: String,
        #[arg(long, default_value = "every:5")]
        keep: score::Sampling,
    },
    Score {
        #[arg(long)]
        a: PathBuf,
        #[arg(long)]
        b: PathBuf,
        #[arg(long, default_value = "all")]
        frames: score::Sampling,
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Run {
            engine,
            corpus,
            runs,
            out_dir,
        } => {
            let out_dir = out_dir.unwrap_or_else(|| std::env::temp_dir().join("vrf-bench"));
            let date = jiff::Zoned::now().date();
            std::fs::create_dir_all(run::results_dir())?;
            for c in corpus::select(&corpus)? {
                for &e in engine.iter().filter(|e| e.supports(c.framework)) {
                    let r = run::run(e, &c, runs, &out_dir)?;
                    let path = run::results_dir()
                        .join(format!("{date}-{}-{}.json", r.engine, r.corpus_id));
                    std::fs::write(&path, serde_json::to_string_pretty(&r)?)?;
                    println!(
                        "{:<14} {:<24} {:>8.3} s {:>8.1} fps",
                        r.engine, r.corpus_id, r.median_s, r.fps
                    );
                }
            }
        }
        Cmd::Refs { corpus, keep } => {
            for c in corpus::select(&corpus)? {
                let out = corpus::bench_dir().join("refs").join(&c.id);
                let floor = refs::build(&c, keep, &out)?;
                let w = &floor.worst;
                println!(
                    "{:<24} {} frames, noise floor: FLIP mean {:.5}, max delta {}, over 8 {:.4}%",
                    c.id,
                    floor.frames_compared,
                    w.flip_mean.value,
                    w.max_delta.value,
                    w.frac_over_8.value * 100.0
                );
            }
        }
        Cmd::Score { a, b, frames, out } => {
            let heat = out.with_extension("worst-flip.png");
            let r = score::score(&a, &b, frames, Some(&heat))?;
            std::fs::write(&out, serde_json::to_string_pretty(&r)?)?;
            let w = &r.worst;
            println!("{} frames compared", r.frames_compared);
            println!(
                "worst FLIP mean   {:.5} (frame {})",
                w.flip_mean.value, w.flip_mean.frame
            );
            println!(
                "worst FLIP p99.9  {:.4} (frame {})",
                w.flip_p999.value, w.flip_p999.frame
            );
            println!(
                "worst max delta   {} (frame {})",
                w.max_delta.value, w.max_delta.frame
            );
            println!(
                "worst over 8      {:.4}% (frame {})",
                w.frac_over_8.value * 100.0,
                w.frac_over_8.frame
            );
            println!(
                "worst over 32     {:.4}% (frame {})",
                w.frac_over_32.value * 100.0,
                w.frac_over_32.frame
            );
            println!(
                "lowest SSIM       {:.4} (frame {})",
                w.ssim.value, w.ssim.frame
            );
            println!("heatmap           {}", heat.display());
        }
    }
    Ok(())
}
