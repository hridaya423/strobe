use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

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

    pub fn wants(&self, frame: u64) -> bool {
        self.plan.as_ref().is_none_or(|plan| plan.contains(&frame))
    }
}

/// Runs a seek expression and, when `ask` is set, asks the tracker in the same round trip
/// whether the page still shows what was last painted. Returns the seek's value and whether the
/// frame is unchanged; the answer is `false` when not asked.
pub fn evaluate(view: &mut View, seek: &str, ask: bool) -> Result<(Value, bool)> {
    if !ask {
        return Ok((view.evaluate(seek)?, false));
    }
    let mut reply = view.evaluate(&format!(
        "Promise.resolve({seek}).then((value) => ({{ value, unchanged: __vrf.track.take() === null }}))"
    ))?;
    let unchanged = reply["unchanged"].as_bool() == Some(true);
    Ok((reply["value"].take(), unchanged))
}
