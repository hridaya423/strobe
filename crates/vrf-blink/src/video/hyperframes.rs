use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::{Clip, Clips, Link, Resolve, StartFallback};
use crate::View;

/// The clips the native decoder cannot take. `cacheable` is false when some clip failed to open,
/// because that may not happen on the next render.
pub struct Refused {
    pub clips: Vec<String>,
    pub cacheable: bool,
}

pub struct NativeVideos {
    files: BTreeMap<String, PathBuf>,
    params: Value,
    clips: Arc<Clips>,
}

impl NativeVideos {
    pub fn new(natives: &HashMap<String, Value>, roots: &[&Path]) -> Result<NativeVideos> {
        let mut files = BTreeMap::new();
        let mut params = serde_json::Map::new();
        for (id, clip) in natives {
            let relative = clip["path"]
                .as_str()
                .with_context(|| format!("video {id} has no path"))?;
            let file = roots
                .iter()
                .map(|root| root.join(relative))
                .find(|file| file.is_file())
                .with_context(|| {
                    format!("video {id}: {relative} is in none of the project's folders")
                })?;
            let mut clip = clip.clone();
            clip["path"] = json!(file.to_string_lossy());
            files.insert(relative.to_owned(), file);
            params.insert(id.clone(), clip);
        }
        Ok(NativeVideos {
            files,
            params: Value::Object(params),
            clips: Arc::default(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn params(&self) -> &Value {
        &self.params
    }

    pub fn refused(&self) -> Refused {
        let mut refused = Refused {
            clips: Vec::new(),
            cacheable: true,
        };
        for (relative, file) in &self.files {
            match Clip::open(file) {
                Ok(clip) if clip.refusal().is_none() => {}
                Ok(_) => refused.clips.push(relative.clone()),
                Err(error) => {
                    eprintln!(
                        "cannot open {relative}, its frames are extracted for this render: {error:#}"
                    );
                    refused.clips.push(relative.clone());
                    refused.cacheable = false;
                }
            }
        }
        refused
    }

    pub fn attach(&self, view: &mut View) {
        let files: HashMap<String, PathBuf> = self
            .files
            .values()
            .map(|file| (file.to_string_lossy().into_owned(), file.clone()))
            .collect();
        let resolve: Resolve = Arc::new(move |src| {
            let file = src.rsplit_once('#').map_or(src, |(file, _)| file);
            files.get(file).cloned()
        });
        let no_fallback: StartFallback = Arc::new(|| {
            bail!("HyperFrames extracts the frames of clips the native decoder refuses")
        });
        Link::attach_with(view, resolve, self.clips.clone(), no_fallback);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_clip_that_fails_to_open_is_extracted_but_not_remembered() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("vrf-refused-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("broken.mp4"), "not a video")?;
        let natives = HashMap::from([("v".to_owned(), json!({ "path": "broken.mp4" }))]);
        let videos = NativeVideos::new(&natives, &[&dir])?;
        let refused = videos.refused();
        fs::remove_dir_all(&dir)?;
        assert_eq!(refused.clips, ["broken.mp4"]);
        assert!(!refused.cacheable);
        Ok(())
    }
}
