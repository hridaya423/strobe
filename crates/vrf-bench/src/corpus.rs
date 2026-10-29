use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Framework {
    Hyperframes,
    Remotion,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Corpus {
    #[serde(skip)]
    pub id: String,
    #[serde(skip)]
    pub root: PathBuf,
    pub framework: Framework,
    pub source: PathBuf,
    pub entry: PathBuf,
    pub composition: Option<String>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub frames: u32,
    pub stresses: Vec<String>,
}

impl Corpus {
    pub fn project_dir(&self) -> PathBuf {
        self.root.join(&self.source)
    }

    pub fn entry_path(&self) -> PathBuf {
        self.project_dir().join(&self.entry)
    }
}

pub fn bench_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench")
}

pub fn load(id: &str) -> Result<Corpus> {
    let root = bench_dir().join("corpus");
    let path = root.join(id).join("corpus.toml");
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut c: Corpus = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    c.id = id.to_string();
    c.root = root;
    Ok(c)
}

pub fn select(spec: &str) -> Result<Vec<Corpus>> {
    if spec != "all" {
        return Ok(vec![load(spec)?]);
    }
    let mut ids: Vec<String> = std::fs::read_dir(bench_dir().join("corpus"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("corpus.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids.iter().map(|id| load(id)).collect()
}
