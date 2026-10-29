use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Mov,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
    ProRes422Hq,
    ProRes422,
}

impl Codec {
    fn sample_entry(self) -> &'static [u8; 4] {
        match self {
            Codec::H264 => b"avc1",
            Codec::Hevc => b"hvc1",
            Codec::Av1 => b"av01",
            Codec::ProRes422Hq => b"apch",
            Codec::ProRes422 => b"apcn",
        }
    }

    fn config_box(self) -> Option<&'static [u8; 4]> {
        match self {
            Codec::H264 => Some(b"avcC"),
            Codec::Hevc => Some(b"hvcC"),
            Codec::Av1 => Some(b"av1C"),
            Codec::ProRes422Hq | Codec::ProRes422 => None,
        }
    }
}

/// The `colr` `nclx` tags written into a sample entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colour {
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    pub full_range: bool,
}

impl Colour {
    pub const BT709: Colour = Colour {
        primaries: 1,
        transfer: 1,
        matrix: 1,
        full_range: false,
    };
}

/// One sample entry. `config` is the body of the `avcC`, `hvcC` or `av1C` box (the decoder configuration
/// record, without the box header) and is empty for ProRes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamConfig {
    pub codec: Codec,
    pub width: u16,
    pub height: u16,
    pub colour: Colour,
    pub config: Vec<u8>,
}

/// One coded frame in length-prefixed (AVCC or HVCC) form, with times in ticks of `1 / timescale`
/// seconds. `dts + duration` of one sample must equal the `dts` of the next, across chunks too.
#[derive(Debug, Clone)]
pub struct Sample {
    pub data: Vec<u8>,
    pub pts: i64,
    pub dts: i64,
    pub duration: u32,
    pub keyframe: bool,
}

impl TryFrom<&vrf_encode::StreamInfo> for StreamConfig {
    type Error = anyhow::Error;

    fn try_from(info: &vrf_encode::StreamInfo) -> Result<Self> {
        Ok(StreamConfig {
            codec: match info.codec {
                vrf_encode::Codec::H264 => Codec::H264,
                vrf_encode::Codec::Hevc => Codec::Hevc,
                vrf_encode::Codec::Av1 => Codec::Av1,
                vrf_encode::Codec::ProRes422 => Codec::ProRes422,
            },
            width: u16::try_from(info.width).context("width does not fit 16 bits")?,
            height: u16::try_from(info.height).context("height does not fit 16 bits")?,
            colour: Colour::BT709,
            config: info.config.clone(),
        })
    }
}

impl Sample {
    /// Converts an encoder packet, whose times are in seconds, to ticks of `timebase`. Fails if
    /// a time is not a whole number of ticks.
    pub fn from_packet(
        packet: vrf_encode::Packet,
        timebase: vrf_encode::Rational,
        duration: u32,
    ) -> Result<Sample> {
        let ticks = |time: vrf_encode::Rational| -> Result<i64> {
            let scaled = i128::from(time.num) * i128::from(timebase.den);
            let whole = scaled % i128::from(time.den * timebase.num) == 0;
            ensure!(
                whole,
                "{}/{} s is not a whole tick of {}/{}",
                time.num,
                time.den,
                timebase.num,
                timebase.den
            );
            i64::try_from(scaled / i128::from(time.den * timebase.num)).context("time out of range")
        };
        Ok(Sample {
            pts: ticks(packet.pts)?,
            dts: ticks(packet.dts)?,
            data: packet.data,
            duration,
            keyframe: packet.keyframe,
        })
    }
}

#[derive(Clone)]
struct Chunk {
    entry: u32,
    samples: u32,
    offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioCodec {
    /// AAC-LC. `audio_specific_config` goes in the `esds` box and `max_packet_bytes` sets its
    /// decoder buffer size.
    Aac {
        audio_specific_config: Vec<u8>,
        max_packet_bytes: u32,
    },
    /// 16-bit little-endian PCM (`sowt`), MOV only.
    PcmS16Le,
}

/// A complete audio track. AAC `packets` hold one AAC frame of 1024 samples each. PCM `packets`
/// hold any whole number of interleaved sample frames.
///
/// `priming` samples at the start of the decoded stream are hidden by an edit list, and the
/// track ends after `frames` samples more.
#[derive(Debug, Clone)]
pub struct AudioTrack {
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub channels: u16,
    pub priming: u32,
    pub frames: u64,
    pub packets: Vec<Vec<u8>>,
}

impl From<&vrf_encode::AacStream> for AudioTrack {
    fn from(stream: &vrf_encode::AacStream) -> AudioTrack {
        AudioTrack {
            codec: AudioCodec::Aac {
                audio_specific_config: stream.audio_specific_config.clone(),
                max_packet_bytes: stream.max_packet_bytes,
            },
            sample_rate: stream.config.sample_rate,
            channels: stream.config.channels as u16,
            priming: stream.priming_frames,
            frames: stream.frames,
            packets: stream.packets.clone(),
        }
    }
}

impl AudioTrack {
    /// Quantises interleaved `f32` samples to 16 bits by rounding, clipping at full scale.
    pub fn pcm_s16le(samples: &[f32], sample_rate: u32, channels: u16) -> AudioTrack {
        let bytes = samples
            .iter()
            .flat_map(|&v| ((v * 32768.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes())
            .collect();
        AudioTrack {
            codec: AudioCodec::PcmS16Le,
            sample_rate,
            channels,
            priming: 0,
            frames: (samples.len() / usize::from(channels)) as u64,
            packets: vec![bytes],
        }
    }

    fn pcm_frame_bytes(&self) -> usize {
        2 * usize::from(self.channels)
    }

    fn packet_frames(&self, packet: &[u8]) -> u64 {
        match self.codec {
            AudioCodec::Aac { .. } => u64::from(vrf_encode::AAC_FRAME_SAMPLES),
            AudioCodec::PcmS16Le => (packet.len() / self.pcm_frame_bytes()) as u64,
        }
    }

    fn packet_units(&self, packet: &[u8]) -> u32 {
        match self.codec {
            AudioCodec::Aac { .. } => 1,
            AudioCodec::PcmS16Le => self.packet_frames(packet) as u32,
        }
    }

    fn media_frames(&self) -> u64 {
        self.packets.iter().map(|p| self.packet_frames(p)).sum()
    }
}

const INTERLEAVE_SECONDS: f64 = 0.5;

enum Piece {
    Video { from: u64, len: u64 },
    Audio { first: usize, last: usize },
}

struct Plan {
    video: Vec<Chunk>,
    audio: Vec<Chunk>,
    pieces: Vec<Piece>,
}

/// Writes an MP4 or MOV file with `moov` before `mdat`.
///
/// Sample data goes to `<path>.mdat` while writing, and `finish` assembles the final file from it.
/// Each chunk started with [`Muxer::start_chunk`] becomes one run of samples that points at its own
/// `stsd` entry. Chunks with equal configs share an entry. The presentation timeline starts at the
/// smallest pts: when B-frames put it after the first dts, an edit list hides the gap.
///
/// An audio track set with [`Muxer::set_audio`] is written in `finish`, interleaved with the video
/// in runs of about half a second. When audio is present the movie timescale is the least common
/// multiple of the video timescale and the sample rate, so the audio edit list is exact.
pub struct Muxer {
    path: PathBuf,
    tmp_path: PathBuf,
    container: Container,
    timescale: u32,
    wide: bool,
    mdat: BufWriter<File>,
    mdat_len: u64,
    entries: Vec<StreamConfig>,
    chunks: Vec<Chunk>,
    open: Option<Chunk>,
    sizes: Vec<u32>,
    durations: Vec<u32>,
    offsets: Vec<i32>,
    sync: Vec<u32>,
    first_dts: i64,
    next_dts: i64,
    min_pts: i64,
    max_end: i64,
    audio: Option<AudioTrack>,
}

impl Muxer {
    pub fn create(path: &Path, container: Container, timescale: u32) -> Result<Muxer> {
        ensure!(timescale > 0, "timescale must be positive");
        let mut tmp_name: OsString = path.as_os_str().to_owned();
        tmp_name.push(".mdat");
        let tmp_path = PathBuf::from(tmp_name);
        let mdat = File::create(&tmp_path).with_context(|| format!("create {tmp_path:?}"))?;
        Ok(Muxer {
            path: path.to_owned(),
            tmp_path,
            container,
            timescale,
            wide: false,
            mdat: BufWriter::new(mdat),
            mdat_len: 0,
            entries: Vec::new(),
            chunks: Vec::new(),
            open: None,
            sizes: Vec::new(),
            durations: Vec::new(),
            offsets: Vec::new(),
            sync: Vec::new(),
            first_dts: 0,
            next_dts: 0,
            min_pts: i64::MAX,
            max_end: i64::MIN,
            audio: None,
        })
    }

    /// Adds the audio track. The first call wins; there is one audio track per file.
    pub fn set_audio(&mut self, audio: AudioTrack) -> Result<()> {
        ensure!(self.audio.is_none(), "the file already has an audio track");
        ensure!(!audio.packets.is_empty(), "audio track has no packets");
        ensure!(
            (1..=2).contains(&audio.channels),
            "audio is mono or stereo, got {} channels",
            audio.channels
        );
        ensure!(
            (1..=u32::from(u16::MAX)).contains(&audio.sample_rate),
            "sample rate {} does not fit the sample entry",
            audio.sample_rate
        );
        if audio.codec == AudioCodec::PcmS16Le {
            ensure!(
                self.container == Container::Mov,
                "PCM audio is only allowed in a MOV container"
            );
            let whole = audio
                .packets
                .iter()
                .all(|p| p.len() % audio.pcm_frame_bytes() == 0);
            ensure!(whole, "PCM packet is not whole sample frames");
        }
        self.audio = Some(audio);
        Ok(())
    }

    /// Writes `co64` and a 64-bit `mdat` size even when 32 bits would do. They are used
    /// automatically once the file passes 4 GiB.
    pub fn force_wide_offsets(&mut self) {
        self.wide = true;
    }

    /// Starts a run of samples from one encoder. The first sample written must be a keyframe.
    pub fn start_chunk(&mut self, config: StreamConfig) -> Result<()> {
        if config.codec.config_box().is_some() {
            ensure!(
                !config.config.is_empty(),
                "{:?} needs its config record",
                config.codec
            );
        } else {
            ensure!(
                self.container == Container::Mov,
                "{:?} is only allowed in a MOV container",
                config.codec
            );
        }
        if let Some(first) = self.entries.first() {
            ensure!(
                first.codec == config.codec,
                "cannot mix {:?} and {:?} in one track",
                first.codec,
                config.codec
            );
            ensure!(
                (first.width, first.height) == (config.width, config.height),
                "chunk is {}x{}, the track is {}x{}",
                config.width,
                config.height,
                first.width,
                first.height
            );
        }
        self.close_chunk();
        let entry = match self.entries.iter().position(|e| *e == config) {
            Some(i) => i,
            None => {
                self.entries.push(config);
                self.entries.len() - 1
            }
        };
        self.open = Some(Chunk {
            entry: entry as u32 + 1,
            samples: 0,
            offset: 0,
        });
        Ok(())
    }

    pub fn write(&mut self, sample: &Sample) -> Result<()> {
        let chunk = self.open.as_mut().context("write before start_chunk")?;
        ensure!(!sample.data.is_empty(), "empty sample");
        ensure!(sample.duration > 0, "sample duration must be positive");
        if chunk.samples == 0 {
            ensure!(sample.keyframe, "a chunk must start with a keyframe");
        }
        if self.sizes.is_empty() {
            self.first_dts = sample.dts;
        } else {
            ensure!(
                sample.dts == self.next_dts,
                "dts {} is not contiguous, expected {}",
                sample.dts,
                self.next_dts
            );
        }
        let size = u32::try_from(sample.data.len()).context("sample over 4 GiB")?;
        let offset = i32::try_from(sample.pts - sample.dts).context("pts - dts out of range")?;
        self.mdat.write_all(&sample.data)?;
        if chunk.samples == 0 {
            chunk.offset = self.mdat_len;
        }
        chunk.samples += 1;
        self.mdat_len += sample.data.len() as u64;
        self.sizes.push(size);
        self.durations.push(sample.duration);
        self.offsets.push(offset);
        if sample.keyframe {
            self.sync.push(self.sizes.len() as u32);
        }
        self.next_dts = sample.dts + i64::from(sample.duration);
        self.min_pts = self.min_pts.min(sample.pts);
        self.max_end = self.max_end.max(sample.pts + i64::from(sample.duration));
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        self.close_chunk();
        ensure!(!self.sizes.is_empty(), "no samples written");
        self.mdat.flush()?;

        let ftyp = self.ftyp();
        let plan = self.plan();
        let mut wide = self.wide;
        let mut moov = self.moov(wide, 0, &plan)?;
        if !wide
            && ftyp.len() as u64 + moov.len() as u64 + 8 + self.mdat_len + self.audio_len()
                > u32::MAX as u64
        {
            wide = true;
            moov = self.moov(true, 0, &plan)?;
        }
        let header_len = if wide { 16 } else { 8 };
        let base = (ftyp.len() + moov.len() + header_len) as u64;
        moov = self.moov(wide, base, &plan)?;

        let mdat_len = self.mdat_len + self.audio_len();
        let mut out = BufWriter::new(
            File::create(&self.path).with_context(|| format!("create {:?}", self.path))?,
        );
        out.write_all(&ftyp)?;
        out.write_all(&moov)?;
        if wide {
            out.write_all(&1u32.to_be_bytes())?;
            out.write_all(b"mdat")?;
            out.write_all(&(mdat_len + 16).to_be_bytes())?;
        } else {
            out.write_all(&(mdat_len as u32 + 8).to_be_bytes())?;
            out.write_all(b"mdat")?;
        }
        let mut video = File::open(&self.tmp_path)?;
        for piece in &plan.pieces {
            match *piece {
                Piece::Video { from, len } => {
                    video.seek(SeekFrom::Start(from))?;
                    io::copy(&mut (&mut video).take(len), &mut out)?;
                }
                Piece::Audio { first, last } => {
                    for packet in &self.audio.as_ref().unwrap().packets[first..last] {
                        out.write_all(packet)?;
                    }
                }
            }
        }
        out.flush()?;
        Ok(())
    }

    fn audio_len(&self) -> u64 {
        self.audio
            .iter()
            .flat_map(|a| &a.packets)
            .map(|p| p.len() as u64)
            .sum()
    }

    fn plan(&self) -> Plan {
        let Some(audio) = &self.audio else {
            return Plan {
                video: self.chunks.clone(),
                audio: Vec::new(),
                pieces: vec![Piece::Video {
                    from: 0,
                    len: self.mdat_len,
                }],
            };
        };

        let slice_ticks = ((INTERLEAVE_SECONDS * f64::from(self.timescale)) as i64).max(1);
        let mut video: Vec<(f64, Chunk, u64)> = Vec::new();
        let (mut index, mut tick) = (0usize, 0i64);
        for chunk in &self.chunks {
            let end = index + chunk.samples as usize;
            let (mut start, mut offset, mut bytes, mut samples) = (tick, chunk.offset, 0u64, 0u32);
            while index < end {
                bytes += u64::from(self.sizes[index]);
                tick += i64::from(self.durations[index]);
                samples += 1;
                index += 1;
                if tick - start >= slice_ticks || index == end {
                    let at = start as f64 / f64::from(self.timescale);
                    let slice = Chunk {
                        entry: chunk.entry,
                        samples,
                        offset,
                    };
                    video.push((at, slice, bytes));
                    offset += bytes;
                    (start, bytes, samples) = (tick, 0, 0);
                }
            }
        }

        let slice_frames = (INTERLEAVE_SECONDS * f64::from(audio.sample_rate)) as u64;
        let mut sound: Vec<(f64, Chunk, usize, usize)> = Vec::new();
        let (mut first, mut position, mut frames, mut units) = (0usize, 0u64, 0u64, 0u32);
        for (i, packet) in audio.packets.iter().enumerate() {
            let packet_frames = audio.packet_frames(packet);
            frames += packet_frames;
            units += audio.packet_units(packet);
            if frames >= slice_frames || i + 1 == audio.packets.len() {
                let at = position.saturating_sub(u64::from(audio.priming)) as f64
                    / f64::from(audio.sample_rate);
                let slice = Chunk {
                    entry: 1,
                    samples: units,
                    offset: 0,
                };
                sound.push((at, slice, first, i + 1));
                (first, position, frames, units) = (i + 1, position + frames, 0, 0);
            }
        }

        let mut plan = Plan {
            video: Vec::new(),
            audio: Vec::new(),
            pieces: Vec::new(),
        };
        let (mut v, mut a, mut offset) = (0usize, 0usize, 0u64);
        while v < video.len() || a < sound.len() {
            let video_next = a == sound.len() || (v < video.len() && video[v].0 <= sound[a].0);
            if video_next {
                let (_, chunk, len) = &video[v];
                plan.pieces.push(Piece::Video {
                    from: chunk.offset,
                    len: *len,
                });
                plan.video.push(Chunk {
                    offset,
                    ..chunk.clone()
                });
                offset += len;
                v += 1;
            } else {
                let (_, chunk, first, last) = &sound[a];
                plan.pieces.push(Piece::Audio {
                    first: *first,
                    last: *last,
                });
                plan.audio.push(Chunk {
                    offset,
                    ..chunk.clone()
                });
                offset += audio.packets[*first..*last]
                    .iter()
                    .map(|p| p.len() as u64)
                    .sum::<u64>();
                a += 1;
            }
        }
        plan
    }

    fn close_chunk(&mut self) {
        if let Some(chunk) = self.open.take()
            && chunk.samples > 0
        {
            self.chunks.push(chunk);
        }
    }

    fn ftyp(&self) -> Vec<u8> {
        bx(b"ftyp", |w| {
            match self.container {
                Container::Mov => {
                    w.bytes(b"qt  ").u32(0x200).bytes(b"qt  ");
                }
                Container::Mp4 => {
                    let codec = self.entries[0].codec.sample_entry();
                    w.bytes(b"isom").u32(512);
                    w.bytes(b"isom").bytes(b"iso2").bytes(codec).bytes(b"mp41");
                }
            };
        })
    }

    fn moov(&self, wide: bool, base: u64, plan: &Plan) -> Result<Vec<u8>> {
        let first = &self.entries[0];
        let start = self.min_pts - self.first_dts;
        ensure!(start >= 0, "pts precedes the first dts");
        let presentation =
            u32::try_from(self.max_end - self.min_pts).context("duration does not fit 32 bits")?;
        let media = u32::try_from(self.next_dts - self.first_dts)
            .context("duration does not fit 32 bits")?;

        let movie_timescale = match &self.audio {
            Some(audio) => lcm(self.timescale, audio.sample_rate)?,
            None => self.timescale,
        };
        let to_movie = |ticks: u32| {
            u32::try_from(u64::from(ticks) * u64::from(movie_timescale / self.timescale))
                .context("duration does not fit 32 bits")
        };
        let video_duration = to_movie(presentation)?;
        let audio_trak = match &self.audio {
            Some(audio) => Some(self.audio_trak(audio, wide, base, plan, movie_timescale)?),
            None => None,
        };
        let movie_duration = match &self.audio {
            Some(audio) => video_duration.max(audio_movie_duration(audio, movie_timescale)?),
            None => video_duration,
        };

        let mvhd = full(b"mvhd", 0, 0, |w| {
            w.u32(0).u32(0).u32(movie_timescale).u32(movie_duration);
            w.u32(0x0001_0000).u16(0x0100).bytes(&[0; 10]);
            w.bytes(&MATRIX)
                .bytes(&[0; 24])
                .u32(if audio_trak.is_some() { 3 } else { 2 });
        });
        let tkhd = full(b"tkhd", 0, 3, |w| {
            w.u32(0).u32(0).u32(1).u32(0).u32(video_duration);
            w.bytes(&[0; 8]).u16(0).u16(0).u16(0).u16(0);
            w.bytes(&MATRIX);
            w.u32(u32::from(first.width) << 16)
                .u32(u32::from(first.height) << 16);
        });
        let edts = if start > 0 {
            bx(b"edts", |w| {
                w.bytes(&full(b"elst", 0, 0, |w| {
                    w.u32(1)
                        .u32(video_duration)
                        .i32(start as i32)
                        .u32(0x0001_0000);
                }));
            })
        } else {
            Vec::new()
        };
        let mdhd = full(b"mdhd", 0, 0, |w| {
            w.u32(0)
                .u32(0)
                .u32(self.timescale)
                .u32(media)
                .u16(0x55c4)
                .u16(0);
        });
        let hdlr = full(b"hdlr", 0, 0, |w| {
            w.u32(0)
                .bytes(b"vide")
                .bytes(&[0; 12])
                .bytes(b"VideoHandler\0");
        });
        let vmhd = full(b"vmhd", 0, 1, |w| {
            w.bytes(&[0; 8]);
        });
        let dinf = bx(b"dinf", |w| {
            w.bytes(&full(b"dref", 0, 0, |w| {
                w.u32(1).bytes(&full(b"url ", 0, 1, |_| {}));
            }));
        });
        let stbl = self.stbl(wide, base, plan)?;
        let minf = bx(b"minf", |w| {
            w.bytes(&vmhd).bytes(&dinf).bytes(&stbl);
        });
        let mdia = bx(b"mdia", |w| {
            w.bytes(&mdhd).bytes(&hdlr).bytes(&minf);
        });
        let trak = bx(b"trak", |w| {
            w.bytes(&tkhd).bytes(&edts).bytes(&mdia);
        });
        Ok(bx(b"moov", |w| {
            w.bytes(&mvhd)
                .bytes(&trak)
                .bytes(&audio_trak.unwrap_or_default());
        }))
    }

    fn audio_trak(
        &self,
        audio: &AudioTrack,
        wide: bool,
        base: u64,
        plan: &Plan,
        movie_timescale: u32,
    ) -> Result<Vec<u8>> {
        let duration = audio_movie_duration(audio, movie_timescale)?;
        let media = u32::try_from(audio.media_frames()).context("duration does not fit 32 bits")?;
        let tkhd = full(b"tkhd", 0, 3, |w| {
            w.u32(0).u32(0).u32(2).u32(0).u32(duration);
            w.bytes(&[0; 8]).u16(0).u16(0).u16(0x0100).u16(0);
            w.bytes(&MATRIX).u32(0).u32(0);
        });
        let edts = if audio.priming > 0 {
            bx(b"edts", |w| {
                w.bytes(&full(b"elst", 0, 0, |w| {
                    w.u32(1).u32(duration).u32(audio.priming).u32(0x0001_0000);
                }));
            })
        } else {
            Vec::new()
        };
        let mdhd = full(b"mdhd", 0, 0, |w| {
            w.u32(0)
                .u32(0)
                .u32(audio.sample_rate)
                .u32(media)
                .u16(0x55c4)
                .u16(0);
        });
        let hdlr = full(b"hdlr", 0, 0, |w| {
            w.u32(0)
                .bytes(b"soun")
                .bytes(&[0; 12])
                .bytes(b"SoundHandler\0");
        });
        let smhd = full(b"smhd", 0, 0, |w| {
            w.u16(0).u16(0);
        });
        let dinf = bx(b"dinf", |w| {
            w.bytes(&full(b"dref", 0, 0, |w| {
                w.u32(1).bytes(&full(b"url ", 0, 1, |_| {}));
            }));
        });
        let stsd = full(b"stsd", 0, 0, |w| {
            w.u32(1).bytes(&audio_entry(audio, self.container));
        });
        let aac = matches!(audio.codec, AudioCodec::Aac { .. });
        let stts = full(b"stts", 0, 0, |w| {
            if aac {
                w.u32(1)
                    .u32(audio.packets.len() as u32)
                    .u32(vrf_encode::AAC_FRAME_SAMPLES);
            } else {
                w.u32(1).u32(media).u32(1);
            }
        });
        let stsz = full(b"stsz", 0, 0, |w| {
            if aac {
                w.u32(0).u32(audio.packets.len() as u32);
                for packet in &audio.packets {
                    w.u32(packet.len() as u32);
                }
            } else {
                w.u32(audio.pcm_frame_bytes() as u32).u32(media);
            }
        });
        let (stsc, stco) = chunk_tables(&plan.audio, wide, base)?;
        let roll = if aac {
            let sgpd = full(b"sgpd", 1, 0, |w| {
                w.bytes(b"roll").u32(2).u32(1).i16(-1);
            });
            let sbgp = full(b"sbgp", 0, 0, |w| {
                w.bytes(b"roll")
                    .u32(1)
                    .u32(audio.packets.len() as u32)
                    .u32(1);
            });
            [sgpd, sbgp].concat()
        } else {
            Vec::new()
        };
        let stbl = bx(b"stbl", |w| {
            w.bytes(&stsd)
                .bytes(&stts)
                .bytes(&stsc)
                .bytes(&stsz)
                .bytes(&stco)
                .bytes(&roll);
        });
        let minf = bx(b"minf", |w| {
            w.bytes(&smhd).bytes(&dinf).bytes(&stbl);
        });
        let mdia = bx(b"mdia", |w| {
            w.bytes(&mdhd).bytes(&hdlr).bytes(&minf);
        });
        Ok(bx(b"trak", |w| {
            w.bytes(&tkhd).bytes(&edts).bytes(&mdia);
        }))
    }

    fn stbl(&self, wide: bool, base: u64, plan: &Plan) -> Result<Vec<u8>> {
        let stsd = full(b"stsd", 0, 0, |w| {
            w.u32(self.entries.len() as u32);
            for entry in &self.entries {
                w.bytes(&sample_entry(entry));
            }
        });
        let stts = full(b"stts", 0, 0, |w| {
            let runs = runs(&self.durations);
            w.u32(runs.len() as u32);
            for (count, duration) in runs {
                w.u32(count).u32(duration);
            }
        });
        let ctts = if self.offsets.iter().any(|&o| o != 0) {
            let signed = self.offsets.iter().any(|&o| o < 0);
            full(b"ctts", u8::from(signed), 0, |w| {
                let runs = runs(&self.offsets);
                w.u32(runs.len() as u32);
                for (count, offset) in runs {
                    w.u32(count).i32(offset);
                }
            })
        } else {
            Vec::new()
        };
        let stss = if self.sync.len() < self.sizes.len() {
            full(b"stss", 0, 0, |w| {
                w.u32(self.sync.len() as u32);
                for &index in &self.sync {
                    w.u32(index);
                }
            })
        } else {
            Vec::new()
        };
        let stsz = full(b"stsz", 0, 0, |w| {
            w.u32(0).u32(self.sizes.len() as u32);
            for &size in &self.sizes {
                w.u32(size);
            }
        });
        let (stsc, stco) = chunk_tables(&plan.video, wide, base)?;
        Ok(bx(b"stbl", |w| {
            w.bytes(&stsd)
                .bytes(&stts)
                .bytes(&ctts)
                .bytes(&stss)
                .bytes(&stsc)
                .bytes(&stsz)
                .bytes(&stco);
        }))
    }
}

impl Drop for Muxer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.tmp_path);
    }
}

fn chunk_tables(chunks: &[Chunk], wide: bool, base: u64) -> Result<(Vec<u8>, Vec<u8>)> {
    let stsc = full(b"stsc", 0, 0, |w| {
        let mut rows: Vec<(u32, u32, u32)> = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            if rows
                .last()
                .is_none_or(|&(_, samples, entry)| (samples, entry) != (chunk.samples, chunk.entry))
            {
                rows.push((i as u32 + 1, chunk.samples, chunk.entry));
            }
        }
        w.u32(rows.len() as u32);
        for (first_chunk, samples, entry) in rows {
            w.u32(first_chunk).u32(samples).u32(entry);
        }
    });
    let offsets = chunks.iter().map(|c| base + c.offset);
    let stco = if wide {
        full(b"co64", 0, 0, |w| {
            w.u32(chunks.len() as u32);
            for offset in offsets {
                w.u64(offset);
            }
        })
    } else {
        let mut narrow = Vec::with_capacity(chunks.len());
        for offset in offsets {
            narrow.push(u32::try_from(offset).context("chunk offset needs co64")?);
        }
        full(b"stco", 0, 0, |w| {
            w.u32(narrow.len() as u32);
            for offset in narrow {
                w.u32(offset);
            }
        })
    };
    Ok((stsc, stco))
}

fn audio_movie_duration(audio: &AudioTrack, movie_timescale: u32) -> Result<u32> {
    let ticks = audio.frames * u64::from(movie_timescale / audio.sample_rate);
    u32::try_from(ticks).context("duration does not fit 32 bits")
}

fn lcm(a: u32, b: u32) -> Result<u32> {
    let (mut x, mut y) = (a, b);
    while y != 0 {
        (x, y) = (y, x % y);
    }
    u32::try_from(u64::from(a) / u64::from(x) * u64::from(b))
        .context("movie timescale does not fit 32 bits")
}

fn audio_entry(audio: &AudioTrack, container: Container) -> Vec<u8> {
    let aac = match &audio.codec {
        AudioCodec::Aac {
            audio_specific_config,
            max_packet_bytes,
        } => {
            let bytes: u64 = audio.packets.iter().map(|p| p.len() as u64).sum();
            let rate = u64::from(audio.sample_rate);
            let average = (bytes * 8 * rate / audio.media_frames()) as u32;
            let maximum = (u64::from(*max_packet_bytes) * 8 * rate
                / u64::from(vrf_encode::AAC_FRAME_SAMPLES)) as u32;
            Some(esds(
                audio_specific_config,
                *max_packet_bytes,
                maximum,
                average,
            ))
        }
        AudioCodec::PcmS16Le => None,
    };
    let quicktime = aac.is_some() && container == Container::Mov;
    let entry = if aac.is_some() { b"mp4a" } else { b"sowt" };
    bx(entry, |w| {
        w.bytes(&[0; 6]).u16(1);
        w.u16(u16::from(quicktime)).u16(0).u32(0);
        w.u16(audio.channels)
            .u16(16)
            .u16(if quicktime { 0xfffe } else { 0 })
            .u16(0);
        w.u32(audio.sample_rate << 16);
        let Some(esds) = aac else {
            return;
        };
        if quicktime {
            w.u32(vrf_encode::AAC_FRAME_SAMPLES).u32(0).u32(0).u32(2);
            w.bytes(&bx(b"wave", |w| {
                w.bytes(&bx(b"frma", |w| {
                    w.bytes(b"mp4a");
                }));
                w.bytes(&bx(b"mp4a", |w| {
                    w.u32(0);
                }));
                w.bytes(&esds).u32(8).u32(0);
            }));
            let layout: u32 = if audio.channels == 1 {
                0x0064_0001
            } else {
                0x0065_0002
            };
            w.bytes(&full(b"chan", 0, 0, |w| {
                w.u32(layout).u32(0).u32(0).u32(0);
            }));
        } else {
            w.bytes(&esds);
        }
    })
}

fn esds(config: &[u8], buffer: u32, maximum: u32, average: u32) -> Vec<u8> {
    full(b"esds", 0, 0, |w| {
        let specific = 2 + config.len();
        let decoder = 13 + specific;
        w.u8(0x03).u8((3 + 2 + decoder + 3) as u8).u16(0).u8(0);
        w.u8(0x04).u8(decoder as u8).u8(0x40).u8(0x15);
        w.u8((buffer >> 16) as u8).u16(buffer as u16);
        w.u32(maximum).u32(average);
        w.u8(0x05).u8(config.len() as u8).bytes(config);
        w.u8(0x06).u8(1).u8(2);
    })
}

const MATRIX: [u8; 36] = {
    let mut m = [0u8; 36];
    m[1] = 1;
    m[17] = 1;
    m[32] = 0x40;
    m
};

fn runs<T: Copy + PartialEq>(values: &[T]) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for &value in values {
        match out.last_mut() {
            Some((count, last)) if *last == value => *count += 1,
            _ => out.push((1, value)),
        }
    }
    out
}

fn sample_entry(entry: &StreamConfig) -> Vec<u8> {
    let prores = entry.codec.config_box().is_none();
    bx(entry.codec.sample_entry(), |w| {
        w.bytes(&[0; 6]).u16(1);
        if prores {
            w.u16(0).u16(0).bytes(b"appl").u32(0).u32(0x200);
        } else {
            w.bytes(&[0; 16]);
        }
        w.u16(entry.width).u16(entry.height);
        w.u32(0x0048_0000).u32(0x0048_0000).u32(0).u16(1);
        w.bytes(&[0; 32]).u16(0x18).i16(-1);
        if let Some(name) = entry.codec.config_box() {
            w.bytes(&bx(name, |w| {
                w.bytes(&entry.config);
            }));
        } else {
            w.bytes(&bx(b"fiel", |w| {
                w.u8(1).u8(0);
            }));
        }
        let c = entry.colour;
        w.bytes(&bx(b"colr", |w| {
            w.bytes(b"nclx")
                .u16(c.primaries)
                .u16(c.transfer)
                .u16(c.matrix);
            w.u8(u8::from(c.full_range) << 7);
        }));
    })
}

#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }

    fn u16(&mut self, v: u16) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    fn i16(&mut self, v: i16) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    fn u32(&mut self, v: u32) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    fn i32(&mut self, v: i32) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    fn u64(&mut self, v: u64) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }

    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }
}

fn bx(typ: &[u8; 4], body: impl FnOnce(&mut W)) -> Vec<u8> {
    let mut w = W::default();
    body(&mut w);
    let mut out = Vec::with_capacity(w.0.len() + 8);
    out.extend_from_slice(&(w.0.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(typ);
    out.extend_from_slice(&w.0);
    out
}

fn full(typ: &[u8; 4], version: u8, flags: u32, body: impl FnOnce(&mut W)) -> Vec<u8> {
    bx(typ, |w| {
        w.u32(u32::from(version) << 24 | flags);
        body(w);
    })
}
