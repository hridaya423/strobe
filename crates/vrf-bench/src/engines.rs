use std::path::{Path, PathBuf};

use crate::corpus::{Corpus, Framework, bench_dir};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Engine {
    Hyperframes,
    Remotion,
    /// This repository's `vrf`, built with `cargo build --release -p vrf-cli`.
    Vrf,
}

pub struct Invocation {
    pub cwd: PathBuf,
    pub argv: Vec<String>,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Engine::Hyperframes => "hyperframes",
            Engine::Remotion => "remotion",
            Engine::Vrf => "vrf",
        }
    }

    pub fn version(self, corpus: &Corpus) -> String {
        match self {
            Engine::Hyperframes => HYPERFRAMES.into(),
            Engine::Remotion => {
                installed_remotion(&corpus.project_dir()).unwrap_or_else(|| REMOTION.into())
            }
            Engine::Vrf => env!("CARGO_PKG_VERSION").into(),
        }
    }

    pub fn supports(self, framework: Framework) -> bool {
        match self {
            Engine::Hyperframes => framework == Framework::Hyperframes,
            Engine::Remotion => framework == Framework::Remotion,
            Engine::Vrf => true,
        }
    }

    pub fn invocation(self, corpus: &Corpus, out: &Path) -> Invocation {
        let bin = |name: &str| node_bin(&bench_dir().join("engines"), name);
        let out = out.display().to_string();
        let s = |p: PathBuf| p.display().to_string();
        match self {
            Engine::Hyperframes => Invocation {
                cwd: bench_dir().join("engines"),
                argv: vec![
                    s(bin("hyperframes")),
                    "render".into(),
                    s(corpus.project_dir()),
                    "-o".into(),
                    out,
                    "--quiet".into(),
                ],
            },
            Engine::Remotion => {
                let dir = corpus.project_dir();
                Invocation {
                    argv: vec![
                        s(node_bin(&dir, "remotion")),
                        "render".into(),
                        corpus.entry.display().to_string(),
                        corpus.composition.clone().unwrap_or_default(),
                        out,
                        "--browser-executable".into(),
                        s(headless_shell()),
                    ],
                    cwd: dir,
                }
            }
            Engine::Vrf => {
                let mut argv = vec![
                    s(vrf_binary()),
                    "render".into(),
                    s(corpus.project_dir()),
                    "--no-daemon".into(),
                ];
                if corpus.framework == Framework::Remotion {
                    argv.extend([
                        "--composition".into(),
                        corpus.composition.clone().unwrap_or_default(),
                        "--entry".into(),
                        s(corpus.entry.clone()),
                    ]);
                } else {
                    argv.extend([
                        "--hyperframes".into(),
                        s(bench_dir().join("engines/node_modules/hyperframes")),
                    ]);
                }
                argv.extend(["-o".into(), out]);
                Invocation {
                    cwd: corpus.project_dir(),
                    argv,
                }
            }
        }
    }
}

pub fn installed_remotion(project: &Path) -> Option<String> {
    let text = std::fs::read_to_string(project.join("node_modules/remotion/package.json")).ok()?;
    let package: serde_json::Value = serde_json::from_str(&text).ok()?;
    package.get("version")?.as_str().map(str::to_owned)
}

pub fn vrf_binary() -> PathBuf {
    let file = if cfg!(windows) { "strobe.exe" } else { "strobe" };
    bench_dir().join("../target/release").join(file)
}

pub fn node_bin(dir: &Path, name: &str) -> PathBuf {
    let file = if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        name.to_string()
    };
    dir.join("node_modules/.bin").join(file)
}

pub fn headless_shell() -> PathBuf {
    let dir = bench_dir().join("engines/browsers/chrome-headless-shell");
    let platform = if cfg!(target_os = "macos") {
        "mac_arm"
    } else if cfg!(windows) {
        "win64"
    } else {
        "linux"
    };
    let leaf = if cfg!(target_os = "macos") {
        "chrome-headless-shell-mac-arm64/chrome-headless-shell"
    } else if cfg!(windows) {
        "chrome-headless-shell-win64/chrome-headless-shell.exe"
    } else {
        "chrome-headless-shell-linux64/chrome-headless-shell"
    };
    dir.join(format!("{platform}-{HEADLESS_SHELL}")).join(leaf)
}

pub const HYPERFRAMES: &str = "0.8.216";

pub const REMOTION: &str = "4.0.539";

pub const HEADLESS_SHELL: &str = "156.0.8078.73";
