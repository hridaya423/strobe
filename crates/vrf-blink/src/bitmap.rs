use std::collections::VecDeque;
use std::sync::Arc;

use cef::Rect;
use objc2_core_foundation::CFRetained;
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};

const HISTORY: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Region {
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
}

impl Region {
    fn union(self, other: Region) -> Region {
        Region {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }
}

fn damage(rects: &[Rect], width: usize, height: usize) -> Region {
    let full = Region {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    rects
        .iter()
        .filter_map(|rect| {
            let left = rect.x.max(0) as usize;
            let top = rect.y.max(0) as usize;
            let right = (rect.x.saturating_add(rect.width).max(0) as usize).min(width);
            let bottom = (rect.y.saturating_add(rect.height).max(0) as usize).min(height);
            (left < right && top < bottom).then_some(Region {
                left,
                top,
                right,
                bottom,
            })
        })
        .reduce(Region::union)
        .filter(|_| !rects.is_empty())
        .unwrap_or(full)
}

fn since(history: &VecDeque<(u64, Region)>, seen: u64, full: Region) -> Region {
    match history.front() {
        Some((first, _)) if *first <= seen + 1 => history
            .iter()
            .filter(|(sequence, _)| *sequence > seen)
            .map(|(_, region)| *region)
            .reduce(Region::union)
            .unwrap_or(full),
        _ => full,
    }
}

fn new_surface(width: usize, height: usize) -> Option<CFRetained<IOSurfaceRef>> {
    use objc2_core_foundation::{CFDictionary, CFNumber, CFString, CFType};
    use objc2_io_surface::{
        kIOSurfaceBytesPerElement, kIOSurfaceHeight, kIOSurfacePixelFormat, kIOSurfaceWidth,
    };
    let number = |v: i64| CFNumber::new_i64(v);
    let (w, h, bpe, fmt) = (
        number(width as i64),
        number(height as i64),
        number(4),
        number(0x4247_5241),
    );
    let properties = unsafe {
        CFDictionary::<CFString, CFType>::from_slices(
            &[
                kIOSurfaceWidth,
                kIOSurfaceHeight,
                kIOSurfaceBytesPerElement,
                kIOSurfacePixelFormat,
            ],
            &[w.as_ref(), h.as_ref(), bpe.as_ref(), fmt.as_ref()],
        )
    };
    unsafe { IOSurfaceRef::new(properties.as_opaque()) }
}

fn copy(surface: &IOSurfaceRef, buffer: *const u8, width: usize, region: Region) {
    unsafe {
        surface.lock(IOSurfaceLockOptions::empty(), std::ptr::null_mut());
        let stride = surface.bytes_per_row();
        let base = surface.base_address().as_ptr().cast::<u8>();
        for row in region.top..region.bottom {
            std::ptr::copy_nonoverlapping(
                buffer.add((row * width + region.left) * 4),
                base.add(row * stride + region.left * 4),
                (region.right - region.left) * 4,
            );
        }
        surface.unlock(IOSurfaceLockOptions::empty(), std::ptr::null_mut());
    }
}

struct Slot {
    surface: CFRetained<IOSurfaceRef>,
    lease: Arc<()>,
    seen: Option<u64>,
}

/// IOSurfaces for the bitmaps of `OnPaint`, reused once the frame that held one has been
/// dropped. A reused surface gets only the pixels that changed since the frame it last held, and
/// the same surface keeps the same id, so the converter imports it once.
pub struct Bitmaps {
    size: (usize, usize),
    slots: Vec<Slot>,
    history: VecDeque<(u64, Region)>,
    sequence: u64,
}

unsafe impl Send for Bitmaps {}

impl Bitmaps {
    pub fn new() -> Bitmaps {
        Bitmaps {
            size: (0, 0),
            slots: Vec::new(),
            history: VecDeque::new(),
            sequence: 0,
        }
    }

    /// Copies a `width` x `height` BGRA `buffer` whose `dirty` rects changed since the last call
    /// into a free surface. The surface stays out of the pool until the returned lease is
    /// dropped.
    pub fn paint(
        &mut self,
        buffer: *const u8,
        width: usize,
        height: usize,
        dirty: &[Rect],
    ) -> Option<(CFRetained<IOSurfaceRef>, Arc<()>)> {
        if self.size != (width, height) {
            self.size = (width, height);
            self.slots.clear();
            self.history.clear();
        }
        let full = Region {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        self.sequence += 1;
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history
            .push_back((self.sequence, damage(dirty, width, height)));
        let free = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| Arc::strong_count(&slot.lease) == 1)
            .max_by_key(|(_, slot)| slot.seen)
            .map(|(at, _)| at);
        let at = match free {
            Some(at) => at,
            None => {
                self.slots.push(Slot {
                    surface: new_surface(width, height)?,
                    lease: Arc::new(()),
                    seen: None,
                });
                self.slots.len() - 1
            }
        };
        let slot = &mut self.slots[at];
        let region = match slot.seen {
            Some(seen) => since(&self.history, seen, full),
            None => full,
        };
        copy(&slot.surface, buffer, width, region);
        slot.seen = Some(self.sequence);
        Some((slot.surface.clone(), slot.lease.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn read(surface: &IOSurfaceRef, width: usize, height: usize) -> Vec<u8> {
        let mut out = vec![0; width * height * 4];
        unsafe {
            surface.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
            let stride = surface.bytes_per_row();
            let base = surface.base_address().as_ptr().cast::<u8>();
            for row in 0..height {
                std::ptr::copy_nonoverlapping(
                    base.add(row * stride),
                    out.as_mut_ptr().add(row * width * 4),
                    width * 4,
                );
            }
            surface.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
        }
        out
    }

    fn history(regions: &[(u64, Region)]) -> VecDeque<(u64, Region)> {
        regions.iter().copied().collect()
    }

    fn at(left: usize, top: usize, right: usize, bottom: usize) -> Region {
        Region {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn damage_is_the_clipped_union_or_everything() {
        assert_eq!(damage(&[], 10, 8), at(0, 0, 10, 8));
        assert_eq!(
            damage(&[rect(2, 1, 3, 2), rect(6, 5, 20, 20)], 10, 8),
            at(2, 1, 10, 8)
        );
        assert_eq!(damage(&[rect(-4, -4, 6, 5)], 10, 8), at(0, 0, 2, 1));
        assert_eq!(damage(&[rect(20, 20, 3, 3)], 10, 8), at(0, 0, 10, 8));
    }

    #[test]
    fn a_surface_is_brought_up_from_the_frames_it_missed() {
        let full = at(0, 0, 10, 8);
        let kept = history(&[
            (3, at(0, 0, 2, 2)),
            (4, at(5, 5, 6, 6)),
            (5, at(1, 7, 2, 8)),
        ]);
        assert_eq!(since(&kept, 4, full), at(1, 7, 2, 8));
        assert_eq!(since(&kept, 3, full), at(1, 5, 6, 8));
        assert_eq!(since(&kept, 2, full), at(0, 0, 6, 8));
        assert_eq!(since(&kept, 1, full), full);
    }

    #[test]
    fn reused_surfaces_equal_a_full_copy_of_every_frame() {
        let (width, height) = (37, 23);
        let mut pool = Bitmaps::new();
        let mut frame = vec![0u8; width * height * 4];
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |limit: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % limit as u64) as usize
        };
        let mut held = Vec::new();
        let mut reused = 0;
        let mut ids = std::collections::HashSet::new();
        for step in 0..200 {
            let rects: Vec<Rect> = (0..next(3))
                .map(|_| {
                    let (x, y) = (next(width), next(height));
                    rect(x as i32, y as i32, 1 + next(9) as i32, 1 + next(7) as i32)
                })
                .collect();
            for r in &rects {
                for row in r.y as usize..((r.y + r.height) as usize).min(height) {
                    for col in r.x as usize..((r.x + r.width) as usize).min(width) {
                        let p = (row * width + col) * 4;
                        frame[p..p + 4].fill(1 + next(255) as u8);
                    }
                }
            }
            let (surface, lease) = pool
                .paint(frame.as_ptr(), width, height, &rects)
                .expect("a surface");
            reused += usize::from(!ids.insert(surface.id()));
            assert_eq!(read(&surface, width, height), frame, "step {step}");
            held.push((surface, lease));
            if held.len() > 1 + next(3) {
                held.remove(next(held.len()));
            }
        }
        assert!(reused > 100, "{reused} reuses");
        assert!(ids.len() <= 5, "{} surfaces", ids.len());
    }

    #[test]
    fn a_new_size_starts_again() {
        let mut pool = Bitmaps::new();
        let small = vec![7u8; 4 * 4 * 4];
        let (first, _) = pool.paint(small.as_ptr(), 4, 4, &[]).unwrap();
        drop(first);
        let large = vec![9u8; 6 * 5 * 4];
        let (second, _) = pool
            .paint(large.as_ptr(), 6, 5, &[rect(0, 0, 1, 1)])
            .unwrap();
        assert_eq!(read(&second, 6, 5), large);
    }
}
