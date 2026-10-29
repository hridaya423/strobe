use std::path::{Path, PathBuf};

use crate::corpus::{Corpus, Framework, bench_dir};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Engine {
    Hyperframes,
    Remotion,
    Kinetograph,
    #[value(name = "kinetograph-4")]
    Kinetograph4,
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
            Engine::Kinetograph => "kinetograph",
            Engine::Kinetograph4 => "kinetograph-4",
        }
    }

    pub fn version(self) -> &'static str {
        match self {
            Engine::Hyperframes => "0.8.216",
            Engine::Remotion => "4.0.539",
            Engine::Kinetograph | Engine::Kinetograph4 => "0.2.5",
        }
    }

    pub fn supports(self, framework: Framework) -> bool {
        match self {
            Engine::Hyperframes => framework == Framework::Hyperframes,
            Engine::Remotion => framework == Framework::Remotion,
            Engine::Kinetograph | Engine::Kinetograph4 => true,
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
            Engine::Kinetograph | Engine::Kinetograph4 => {
                let workers = if self == Engine::Kinetograph {
                    "1"
                } else {
                    "4"
                };
                let mut argv = vec![
                    s(bin("kinetograph")),
                    "render".into(),
                    s(corpus.entry_path()),
                    "-o".into(),
                    out,
                    "--concurrency".into(),
                    workers.into(),
                ];
                if let Some(comp) = &corpus.composition {
                    argv.extend(["--comp".into(), comp.clone()]);
                }
                Invocation {
                    cwd: corpus.project_dir(),
                    argv,
                }
            }
        }
    }
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

pub const HEADLESS_SHELL: &str = "156.0.8078.73";
