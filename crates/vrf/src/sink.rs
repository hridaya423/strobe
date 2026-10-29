use anyhow::Result;
use vrf_encode::{Encoder, Packet};

use crate::mux::{Muxer, Sample, StreamConfig};

/// Writes an encoder's packets into a muxer, starting the chunk with the encoder's stream
/// parameters once the first packet is out.
pub struct Sink {
    muxer: Muxer,
    started: bool,
    duration: u32,
}

impl Sink {
    /// `duration` is one frame in the muxer's timescale.
    pub fn new(muxer: Muxer, duration: u32) -> Sink {
        Sink {
            muxer,
            started: false,
            duration,
        }
    }

    pub fn write(&mut self, encoder: &impl Encoder, packets: Vec<Packet>) -> Result<()> {
        if packets.is_empty() {
            return Ok(());
        }
        let info = encoder.stream_info()?;
        if !self.started {
            self.muxer.start_chunk(StreamConfig::try_from(&info)?)?;
            self.started = true;
        }
        for packet in packets {
            self.muxer
                .write(&Sample::from_packet(packet, info.timebase, self.duration)?)?;
        }
        Ok(())
    }

    pub fn into_muxer(self) -> Muxer {
        self.muxer
    }
}
