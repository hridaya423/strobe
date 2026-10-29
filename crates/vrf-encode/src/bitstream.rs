use anyhow::{Context, Result, bail, ensure};

use crate::Codec;

pub struct Access {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub parameters: Vec<Vec<u8>>,
}

fn annex_b_units(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut at = 0;
    while at + 3 <= stream.len() {
        if stream[at..at + 3] == [0, 0, 1] {
            starts.push(at + 3);
            at += 3;
        } else {
            at += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, start)| {
            let end = starts.get(n + 1).map_or(stream.len(), |next| next - 3);
            let unit = &stream[*start..end];
            let kept = unit
                .iter()
                .rposition(|byte| *byte != 0)
                .map_or(0, |i| i + 1);
            &unit[..kept]
        })
        .filter(|unit| !unit.is_empty())
        .collect()
}

fn leb128(data: &[u8]) -> Result<(usize, usize)> {
    let mut value = 0usize;
    for (n, byte) in data.iter().take(8).enumerate() {
        value |= ((byte & 0x7f) as usize) << (7 * n);
        if byte & 0x80 == 0 {
            return Ok((value, n + 1));
        }
    }
    bail!("unterminated leb128 size in an OBU");
}

fn nal_access(codec: Codec, stream: &[u8]) -> Result<Access> {
    let mut access = Access {
        data: Vec::with_capacity(stream.len()),
        keyframe: false,
        parameters: Vec::new(),
    };
    for unit in annex_b_units(stream) {
        let (parameter, delimiter, idr) = match codec {
            Codec::H264 => {
                let kind = unit[0] & 0x1f;
                (kind == 7 || kind == 8, kind == 9, kind == 5)
            }
            _ => {
                let kind = (unit[0] >> 1) & 0x3f;
                (
                    (32..=34).contains(&kind),
                    kind == 35,
                    kind == 19 || kind == 20,
                )
            }
        };
        access.keyframe |= idr;
        if parameter {
            access.parameters.push(unit.to_vec());
        } else if !delimiter {
            access
                .data
                .extend_from_slice(&(unit.len() as u32).to_be_bytes());
            access.data.extend_from_slice(unit);
        }
    }
    ensure!(
        !access.data.is_empty(),
        "the bitstream holds no picture data"
    );
    Ok(access)
}

fn obu_access(stream: &[u8]) -> Result<Access> {
    let mut access = Access {
        data: Vec::with_capacity(stream.len()),
        keyframe: false,
        parameters: Vec::new(),
    };
    let mut at = 0;
    while at < stream.len() {
        let header = stream[at];
        let kind = (header >> 3) & 0xf;
        let body = at + 1 + ((header >> 2) & 1) as usize;
        let (size, size_len) = if header & 2 != 0 {
            leb128(stream.get(body..).context("truncated OBU")?)?
        } else {
            (stream.len() - body, 0)
        };
        let payload = body + size_len;
        let end = payload + size;
        let obu = stream.get(at..end).context("OBU runs past the packet")?;
        match kind {
            1 => access.parameters.push(obu.to_vec()),
            3 | 6 => {
                let first = stream[payload];
                access.keyframe |= first & 0x80 == 0 && (first >> 5) & 3 == 0;
            }
            _ => {}
        }
        if kind != 2 {
            access.data.extend_from_slice(obu);
        }
        at = end;
    }
    ensure!(
        !access.data.is_empty(),
        "the bitstream holds no picture data"
    );
    Ok(access)
}

pub fn convert(codec: Codec, stream: &[u8]) -> Result<Access> {
    match codec {
        Codec::H264 | Codec::Hevc => nal_access(codec, stream),
        Codec::Av1 => obu_access(stream),
        Codec::ProRes422 => bail!("ProRes is not a hardware bitstream"),
    }
}

fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for byte in nal {
        if zeros >= 2 && *byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if *byte == 0 { zeros + 1 } else { 0 };
        out.push(*byte);
    }
    out
}

fn find(parameters: &[Vec<u8>], codec: Codec, kind: u8) -> Result<&[u8]> {
    parameters
        .iter()
        .find(|nal| match codec {
            Codec::H264 => nal[0] & 0x1f == kind,
            _ => (nal[0] >> 1) & 0x3f == kind,
        })
        .map(Vec::as_slice)
        .with_context(|| format!("the first keyframe has no NAL of type {kind}"))
}

fn avcc(parameters: &[Vec<u8>]) -> Result<Vec<u8>> {
    let sps = find(parameters, Codec::H264, 7)?;
    let pps = find(parameters, Codec::H264, 8)?;
    ensure!(sps.len() >= 4, "SPS is too short");
    let mut out = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe1];
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1);
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(pps);
    if matches!(
        sps[1],
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134
    ) {
        out.extend_from_slice(&[0xfc | 1, 0xf8, 0xf8, 0]);
    }
    Ok(out)
}

fn hvcc(parameters: &[Vec<u8>], ten_bit: bool) -> Result<Vec<u8>> {
    let sps = find(parameters, Codec::Hevc, 33)?;
    let rbsp = unescape(sps);
    ensure!(rbsp.len() >= 15, "SPS is too short");
    let nested = rbsp[2] & 1;
    let profile = &rbsp[3..15];
    let depth = if ten_bit { 2 } else { 0 };
    let mut out = vec![1];
    out.extend_from_slice(profile);
    out.extend_from_slice(&[0xf0, 0, 0xfc, 0xfc | 1, 0xf8 | depth, 0xf8 | depth, 0, 0]);
    out.push(1 << 3 | nested << 2 | 3);
    out.push(3);
    for kind in [32, 33, 34] {
        let nal = find(parameters, Codec::Hevc, kind)?;
        out.push(0x80 | kind);
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        out.extend_from_slice(nal);
    }
    Ok(out)
}

struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn take(&mut self, count: usize) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            let byte = self
                .data
                .get(self.at / 8)
                .context("sequence header ended early")?;
            value = value << 1 | ((byte >> (7 - self.at % 8)) & 1) as u32;
            self.at += 1;
        }
        Ok(value)
    }

    fn uvlc(&mut self) -> Result<()> {
        let mut zeros = 0;
        while self.take(1)? == 0 {
            zeros += 1;
            ensure!(zeros < 32, "bad uvlc in the sequence header");
        }
        self.take(zeros).map(drop)
    }
}

fn av1c(parameters: &[Vec<u8>], ten_bit: bool) -> Result<Vec<u8>> {
    let obu = parameters
        .first()
        .context("the first keyframe has no sequence header")?;
    let body = 1 + ((obu[0] >> 2) & 1) as usize;
    let payload = if obu[0] & 2 != 0 {
        &obu[body + leb128(&obu[body..])?.1..]
    } else {
        &obu[body..]
    };
    let mut bits = Bits {
        data: payload,
        at: 0,
    };
    let profile = bits.take(3)?;
    let still = bits.take(1)?;
    let reduced = bits.take(1)?;
    ensure!(
        still == 0 && reduced == 0,
        "still-picture AV1 is not supported"
    );
    if bits.take(1)? == 1 {
        bits.take(32)?;
        bits.take(32)?;
        if bits.take(1)? == 1 {
            bits.uvlc()?;
        }
    }
    let decoder_model = bits.take(1)?;
    ensure!(
        decoder_model == 0,
        "AV1 decoder model info is not supported"
    );
    bits.take(1)?;
    bits.take(5)?;
    bits.take(12)?;
    let level = bits.take(5)?;
    let tier = if level > 7 { bits.take(1)? } else { 0 };
    let mut out = vec![
        0x81,
        (profile << 5 | level) as u8,
        (tier << 7 | u32::from(ten_bit) << 6 | 1 << 3 | 1 << 2) as u8,
        0,
    ];
    out.extend_from_slice(obu);
    Ok(out)
}

pub fn config_record(codec: Codec, ten_bit: bool, parameters: &[Vec<u8>]) -> Result<Vec<u8>> {
    match codec {
        Codec::H264 => avcc(parameters),
        Codec::Hevc => hvcc(parameters, ten_bit),
        Codec::Av1 => av1c(parameters, ten_bit),
        Codec::ProRes422 => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_units_split_on_three_and_four_byte_start_codes() {
        let stream = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 0,
        ];
        let units = annex_b_units(&stream);
        assert_eq!(units, [&[0x67, 1, 2][..], &[0x68, 3], &[0x65, 4]]);
    }

    #[test]
    fn h264_access_unit_drops_parameter_sets_and_delimiters() {
        let stream = [
            0, 0, 0, 1, 0x09, 0xf0, 0, 0, 0, 1, 0x67, 0x64, 0, 0x1f, 0, 0, 0, 1, 0x68, 0xee, 0x3c,
            0, 0, 0, 1, 0x65, 9, 9, 9,
        ];
        let access = convert(Codec::H264, &stream).unwrap();
        assert!(access.keyframe);
        assert_eq!(access.data, [0, 0, 0, 4, 0x65, 9, 9, 9]);
        assert_eq!(
            access.parameters,
            [vec![0x67, 0x64, 0, 0x1f], vec![0x68, 0xee, 0x3c]]
        );
        let record = config_record(Codec::H264, false, &access.parameters).unwrap();
        assert_eq!(&record[..6], [1, 0x64, 0, 0x1f, 0xff, 0xe1]);
        assert_eq!(&record[6..8], [0, 4]);
        assert_eq!(&record[record.len() - 4..], [0xfd, 0xf8, 0xf8, 0]);
    }

    #[test]
    fn h264_non_idr_slice_is_not_a_keyframe() {
        let stream = [0, 0, 0, 1, 0x41, 0x9a, 1, 2];
        let access = convert(Codec::H264, &stream).unwrap();
        assert!(!access.keyframe);
        assert_eq!(access.data, [0, 0, 0, 4, 0x41, 0x9a, 1, 2]);
    }

    #[test]
    fn hevc_idr_is_a_keyframe_and_the_record_reads_the_profile() {
        let sps = [
            0x42, 0x01, 0x01, 0x01, 0x60, 0, 0, 3, 0, 0x90, 0, 0, 3, 0, 0, 3, 0, 0x5d, 0xa0, 2,
        ];
        let mut stream = vec![0, 0, 0, 1, 0x40, 1, 0x0c];
        stream.extend_from_slice(&[0, 0, 0, 1]);
        stream.extend_from_slice(&sps);
        stream.extend_from_slice(&[0, 0, 0, 1, 0x44, 1, 0xc0, 0, 0, 0, 1, 0x26, 1, 7, 7]);
        let access = convert(Codec::Hevc, &stream).unwrap();
        assert!(access.keyframe);
        assert_eq!(access.data, [0, 0, 0, 4, 0x26, 1, 7, 7]);
        let record = config_record(Codec::Hevc, false, &access.parameters).unwrap();
        assert_eq!(record[0], 1);
        assert_eq!(
            &record[1..13],
            [1, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 0x5d]
        );
        assert_eq!(record[21] & 3, 3);
        assert_eq!(record[22], 3);
        let ten = config_record(Codec::Hevc, true, &access.parameters).unwrap();
        assert_eq!(ten[17], 0xf8 | 2);
    }

    #[test]
    fn av1_packet_loses_the_temporal_delimiter_and_keeps_the_rest() {
        let sequence = [0x0a, 0x06, 0x00, 0x00, 0x00, 0x22, 0x00, 0x00];
        let frame = [0x32, 0x03, 0x10, 0x20, 0x30];
        let mut stream = vec![0x12, 0x00];
        stream.extend_from_slice(&sequence);
        stream.extend_from_slice(&frame);
        let access = convert(Codec::Av1, &stream).unwrap();
        assert!(access.keyframe);
        assert_eq!(access.parameters, [sequence.to_vec()]);
        assert_eq!(access.data.len(), sequence.len() + frame.len());
        assert_eq!(access.data[0], 0x0a);
    }

    #[test]
    fn av1_inter_frame_is_not_a_keyframe() {
        let stream = [0x12, 0x00, 0x32, 0x02, 0x30, 0x01];
        assert!(!convert(Codec::Av1, &stream).unwrap().keyframe);
    }

    #[test]
    fn av1_record_reads_profile_level_and_tier() {
        let sequence = [0x0a, 0x06, 0x00, 0x00, 0x00, 0x22, 0x00, 0x00];
        let record = config_record(Codec::Av1, false, &[sequence.to_vec()]).unwrap();
        assert_eq!(&record[..4], [0x81, 8, 0x8c, 0]);
        assert_eq!(&record[4..], sequence);
        let ten = config_record(Codec::Av1, true, &[sequence.to_vec()]).unwrap();
        assert_eq!(ten[2], 0xcc);
    }

    #[test]
    fn empty_bitstream_is_an_error() {
        assert!(convert(Codec::H264, &[0, 0, 0, 1, 0x09, 0xf0]).is_err());
    }
}
