use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;

use crate::View;

pub const TRACK_JS: &str = include_str!("../js/track.js");

/// Decides, after a view has seeked to a frame, whether the page already shows what was last
/// painted. The page proves it: `track.js` reports any DOM, style, animation or media state that
/// may have changed since the last report, so a frame it calls unchanged needs no begin frame,
/// paint, import or convert. With a plan, only the frames in it are asked.
#[derive(Debug, Clone, Default)]
pub struct Skip {
    plan: Option<Arc<HashSet<u64>>>,
}

impl Skip {
    pub fn tracked() -> Skip {
        Skip { plan: None }
    }

    pub fn planned(frames: impl IntoIterator<Item = u64>) -> Skip {
        Skip {
            plan: Some(Arc::new(frames.into_iter().collect())),
        }
    }

    pub fn unchanged(&self, view: &mut View, frame: u64) -> Result<bool> {
        if self
            .plan
            .as_ref()
            .is_some_and(|plan| !plan.contains(&frame))
        {
            return Ok(false);
        }
        Ok(view.evaluate("__vrf.track.take()")?.is_null())
    }
}
