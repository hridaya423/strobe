use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};
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

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

fn store(dir: &Path, stem: &str, outcome: &Result<run::RunResult>) -> Result<()> {
    let json = dir.join(format!("{stem}.json"));
    let failed = dir.join(format!("{stem}.failed.txt"));
    match outcome {
        Ok(r) => {
            std::fs::write(&json, serde_json::to_string_pretty(r)?)?;
            remove_if_present(&failed)
        }
        Err(err) => {
            std::fs::write(&failed, format!("{err:#}\n"))?;
            remove_if_present(&json)
        }
    }
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
            let mut failed = 0;
            for c in corpus::select(&corpus)? {
                for &e in engine.iter().filter(|e| e.supports(c.framework)) {
                    let stem = format!("{date}-{}-{}", e.name(), c.id);
                    let outcome = run::run(e, &c, runs, &out_dir);
                    store(&run::results_dir(), &stem, &outcome)?;
                    match outcome {
                        Ok(r) => {
                            println!(
                                "{:<14} {:<24} {:>8.3} s {:>8.1} fps",
                                r.engine, r.corpus_id, r.median_s, r.fps
                            );
                        }
                        Err(err) => {
                            failed += 1;
                            println!("{:<14} {:<24} FAILED: {err:#}", e.name(), c.id);
                        }
                    }
                }
            }
            ensure!(failed == 0, "{failed} runs failed");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn result() -> run::RunResult {
        run::RunResult {
            engine: "vrf".into(),
            version: "0.1.0".into(),
            corpus_id: "hf-tiny".into(),
            frames: 30,
            median_s: 1.0,
            min_s: 0.9,
            max_s: 1.1,
            fps: 30.0,
            runs: 5,
            warmup: 1,
            machine: "test".into(),
            load_avg_before: 0.1,
        }
    }

    #[test]
    fn a_later_outcome_replaces_the_earlier_one_for_the_same_stem() {
        let dir = std::env::temp_dir().join(format!("vrf-bench-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let json = dir.join("d-vrf-x.json");
        let failed = dir.join("d-vrf-x.failed.txt");

        store(&dir, "d-vrf-x", &Err(anyhow::anyhow!("boom"))).unwrap();
        assert!(failed.is_file() && !json.exists());

        store(&dir, "d-vrf-x", &Ok(result())).unwrap();
        assert!(json.is_file() && !failed.exists());
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
        assert_eq!(written["runs"], 5);
        assert_eq!(written["warmup"], 1);

        store(&dir, "d-vrf-x", &Err(anyhow::anyhow!("again"))).unwrap();
        assert!(failed.is_file() && !json.exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
