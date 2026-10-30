use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use windows::Win32::Graphics::Direct3D12::ID3D12Device;

use super::{InputPool, NvencEncoder, Options};
use crate::{Encoder, EncoderConfig, EncoderFrame, Packet, Rational, StreamInfo};

/// Several NVENC sessions that each encode runs of consecutive frames. A frame goes to the
/// session whose pool it was written into, and starts an IDR frame when that session did not
/// encode the frame before it, so every session's run decodes alone. The packets come out in
/// presentation order, so the file is one ordinary stream. Callers split the timeline in chunks
/// with [`NvencSessions::session_of`].
pub struct NvencSessions {
    sessions: Vec<NvencEncoder>,
    last: Vec<Option<u64>>,
    frame_rate: Rational,
    pending: BTreeMap<u64, Packet>,
    next: u64,
}

impl NvencSessions {
    /// Opens `count` sessions with `pool_size` input textures each. Frames are not reordered
    /// across a session's packets, so B-frames are rejected.
    pub fn new(
        device: &ID3D12Device,
        config: EncoderConfig,
        options: Options,
        count: usize,
        pool_size: usize,
    ) -> Result<Self> {
        ensure!(count > 0, "at least one session is needed");
        ensure!(
            options.b_frames == 0,
            "B-frames reorder packets inside one session; use one session with them"
        );
        let frame_rate = config.frame_rate;
        let sessions = (0..count)
            .map(|_| NvencEncoder::with_pool_size(device, config.clone(), options, pool_size))
            .collect::<Result<Vec<_>>>()?;
        Ok(NvencSessions {
            last: vec![None; count],
            sessions,
            frame_rate,
            pending: BTreeMap::new(),
            next: 0,
        })
    }

    /// The session that encodes frame `index`.
    pub fn session_of(index: u64, chunk_frames: u64, count: usize) -> usize {
        (index / chunk_frames.max(1) % count as u64) as usize
    }

    pub fn pools(&self) -> Vec<InputPool> {
        self.sessions
            .iter()
            .map(|session| session.pool().clone())
            .collect()
    }

    fn index(&self, time: Rational) -> u64 {
        let scaled = i128::from(time.num) * i128::from(self.frame_rate.num);
        (scaled / (i128::from(time.den) * i128::from(self.frame_rate.den))) as u64
    }

    fn collect(&mut self, packets: Vec<Packet>) {
        for packet in packets {
            let index = self.index(packet.pts);
            self.pending.insert(index, packet);
        }
    }

    fn release(&mut self, all: bool) -> Vec<Packet> {
        let mut ready = Vec::new();
        if all {
            ready.extend(std::mem::take(&mut self.pending).into_values());
            return ready;
        }
        while let Some(packet) = self.pending.remove(&self.next) {
            ready.push(packet);
            self.next += 1;
        }
        ready
    }
}

impl Encoder for NvencSessions {
    fn submit(&mut self, frame: EncoderFrame, pts: Rational) -> Result<()> {
        let EncoderFrame::D3d12(pool_frame) = &frame;
        let session = self
            .sessions
            .iter()
            .position(|session| pool_frame.pool_is(session.pool()))
            .context("the frame comes from no session's pool")?;
        let index = self.index(pts);
        let continues = index > 0 && self.last[session] == Some(index - 1);
        self.sessions[session].submit_keyframe(frame, pts, !continues)?;
        self.last[session] = Some(index);
        Ok(())
    }

    fn drain(&mut self) -> Result<Vec<Packet>> {
        for session in 0..self.sessions.len() {
            let packets = self.sessions[session].drain()?;
            self.collect(packets);
        }
        Ok(self.release(false))
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        for session in 0..self.sessions.len() {
            let packets = self.sessions[session].flush()?;
            self.collect(packets);
        }
        Ok(self.release(true))
    }

    fn stream_info(&self) -> Result<StreamInfo> {
        self.sessions[0].stream_info()
    }

    fn finish(mut self) -> Result<StreamInfo> {
        let undrained = self.flush()?.len();
        ensure!(
            undrained == 0,
            "finish found {undrained} undrained packets; drain after flush"
        );
        self.stream_info()
    }
}
