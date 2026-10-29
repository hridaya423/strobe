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
        let bin = |name: &str| bench_dir().join("node_modules/.bin").join(name);
        let out = out.display().to_string();
        let s = |p: PathBuf| p.display().to_string();
        match self {
            Engine::Hyperframes => Invocation {
                cwd: bench_dir(),
                argv: vec![s(bin("hyperframes")), "render".into(), s(corpus.project_dir()), "-o".into(), out, "--quiet".into()],
            },
            Engine::Remotion => {
                let dir = corpus.project_dir();
                Invocation {
                    argv: vec![
                        s(dir.join("node_modules/.bin/remotion")),
                        "render".into(),
                        corpus.entry.display().to_string(),
                        corpus.composition.clone().unwrap_or_default(),
                        out,
                    ],
                    cwd: dir,
                }
            }
            Engine::Kinetograph | Engine::Kinetograph4 => {
                let workers = if self == Engine::Kinetograph { "1" } else { "4" };
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
                Invocation { cwd: corpus.project_dir(), argv }
            }
        }
    }
}
