use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tags {
    pub matrix: u16,
    pub full_range: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub width: u32,
    pub height: u32,
    pub timescale: i32,
    pub pts: Vec<i64>,
    pub media: Vec<i64>,
    pub keyframes: Vec<usize>,
    pub codec: [u8; 4],
    pub profile: i32,
    pub tags: Option<Tags>,
    pub lead: i64,
    pub edits: usize,
}

struct Segment {
    source: i64,
    length: i64,
    target: i64,
}

const MAX_SAMPLES: usize = 1 << 24;

fn bytes_at(data: &[u8], at: usize, length: usize) -> Result<&[u8]> {
    at.checked_add(length)
        .and_then(|end| data.get(at..end))
        .context("a box is cut short")
}

fn u16_at(data: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(bytes_at(data, at, 2)?.try_into()?))
}

fn u32_at(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(bytes_at(data, at, 4)?.try_into()?))
}

fn u64_at(data: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_be_bytes(bytes_at(data, at, 8)?.try_into()?))
}

fn boxes(data: &[u8]) -> Result<Vec<([u8; 4], &[u8])>> {
    let mut found = Vec::new();
    let mut at = 0;
    while at + 8 <= data.len() {
        let mut size = u32_at(data, at)? as usize;
        let kind: [u8; 4] = data[at + 4..at + 8].try_into()?;
        let mut header = 8;
        if size == 1 {
            size = usize::try_from(u64_at(data, at + 8)?).context("a box is too large")?;
            header = 16;
        } else if size == 0 {
            size = data.len() - at;
        }
        let end = at
            .checked_add(size)
            .filter(|end| *end <= data.len() && size >= header)
            .context("a box is cut short")?;
        found.push((kind, &data[at + header..end]));
        at = end;
    }
    Ok(found)
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Result<Option<&'a [u8]>> {
    Ok(boxes(data)?
        .into_iter()
        .find(|(found, _)| found == kind)
        .map(|(_, body)| body))
}

fn timescale(body: &[u8]) -> Result<i32> {
    let at = if body.first() == Some(&1) { 20 } else { 12 };
    Ok(u32_at(body, at)? as i32)
}

fn movie(path: &Path) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut at = 0;
    while at + 8 <= length {
        let mut header = [0; 16];
        file.seek(SeekFrom::Start(at))?;
        let read = file.read(&mut header)?;
        ensure!(read >= 8, "a box is cut short");
        ensure!(
            at > 0
                || matches!(
                    &header[4..8],
                    b"ftyp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide" | b"pnot"
                ),
            "not an MP4 or MOV file"
        );
        let mut size = u32::from_be_bytes(header[..4].try_into()?) as u64;
        let mut skip = 8;
        if size == 1 {
            ensure!(read == 16, "a box is cut short");
            size = u64::from_be_bytes(header[8..].try_into()?);
            skip = 16;
        } else if size == 0 {
            size = length - at;
        }
        ensure!(
            size >= skip && at.checked_add(size).is_some_and(|end| end <= length),
            "a box is cut short"
        );
        ensure!(
            &header[4..8] != b"moof",
            "fragmented MP4 is not supported"
        );
        if &header[4..8] == b"moov" {
            let mut body = vec![0; (size - skip) as usize];
            file.seek(SeekFrom::Start(at + skip))?;
            file.read_exact(&mut body)?;
            return Ok(body);
        }
        at += size;
    }
    bail!("not an MP4 or MOV file: no movie box")
}

fn rescale(value: i64, from: i64, to: i64) -> i64 {
    if from == to {
        value
    } else {
        (value as f64 * to as f64 / from as f64).round() as i64
    }
}

fn segments(trak: &[u8], movie_scale: i64, track_scale: i64) -> Result<Vec<Segment>> {
    let Some(list) = child(trak, b"edts")?
        .map(|edts| child(edts, b"elst"))
        .transpose()?
        .flatten()
    else {
        return Ok(vec![Segment {
            source: 0,
            length: i64::MAX / 4,
            target: 0,
        }]);
    };
    let version = *list.first().context("the edit list is empty")?;
    let count = u32_at(list, 4)? as usize;
    let size = if version == 1 { 20 } else { 12 };
    let mut found = Vec::new();
    let mut elapsed = 0i64;
    for entry in 0..count {
        let at = 8 + entry * size;
        let (duration, media) = if version == 1 {
            (u64_at(list, at)? as i64, u64_at(list, at + 8)? as i64)
        } else {
            (
                u32_at(list, at)? as i64,
                u32_at(list, at + 4)? as i32 as i64,
            )
        };
        if media >= 0 {
            found.push(Segment {
                source: media,
                length: if duration == 0 && entry + 1 == count {
                    i64::MAX / 4
                } else {
                    rescale(duration, movie_scale, track_scale)
                },
                target: rescale(elapsed, movie_scale, track_scale),
            });
        }
        elapsed = elapsed.checked_add(duration).context("the edit list is too long")?;
    }
    ensure!(!found.is_empty(), "the edit list shows nothing");
    Ok(found)
}

fn runs(body: &[u8], declared: usize, what: &str) -> Result<Vec<(u32, i32)>> {
    let count = u32_at(body, 4)? as usize;
    let table = bytes_at(body, 8, count.checked_mul(8).context("a box is cut short")?)?;
    let runs: Vec<(u32, i32)> = table
        .chunks_exact(8)
        .map(|entry| {
            (
                u32::from_be_bytes([entry[0], entry[1], entry[2], entry[3]]),
                i32::from_be_bytes([entry[4], entry[5], entry[6], entry[7]]),
            )
        })
        .collect();
    let listed: u64 = runs.iter().map(|(samples, _)| u64::from(*samples)).sum();
    ensure!(
        listed <= declared as u64,
        "{what} lists {listed} samples, the track declares {declared}"
    );
    Ok(runs)
}

fn declared_samples(stbl: &[u8]) -> Result<usize> {
    let body = match child(stbl, b"stsz")? {
        Some(body) => body,
        None => child(stbl, b"stz2")?.context("no sample sizes")?,
    };
    let count = u32_at(body, 8)? as usize;
    ensure!(
        count <= MAX_SAMPLES,
        "the track declares {count} samples, more than {MAX_SAMPLES}"
    );
    Ok(count)
}

pub fn read(path: &Path) -> Result<Track> {
    let moov = movie(path)?;
    let header = child(&moov, b"mvhd")?.context("no movie header")?;
    let movie_scale = timescale(header)? as i64;
    ensure!(
        child(&moov, b"mvex")?.is_none(),
        "fragmented MP4 is not supported"
    );
    for (kind, trak) in boxes(&moov)? {
        if &kind != b"trak" {
            continue;
        }
        let Some(mdia) = child(trak, b"mdia")? else {
            continue;
        };
        let handler = child(mdia, b"hdlr")?.context("no handler")?;
        if handler.get(8..12) != Some(b"vide") {
            continue;
        }
        return track(trak, mdia, movie_scale);
    }
    bail!("no video track")
}

fn track(trak: &[u8], mdia: &[u8], movie_scale: i64) -> Result<Track> {
    let scale = timescale(child(mdia, b"mdhd")?.context("no media header")?)?;
    let minf = child(mdia, b"minf")?.context("no media information")?;
    let stbl = child(minf, b"stbl")?.context("no sample table")?;
    let description = child(stbl, b"stsd")?.context("no sample description")?;
    let entry_size = u32_at(description, 8)? as usize;
    let entry = description
        .get(8..8 + entry_size)
        .context("a box is cut short")?;
    ensure!(entry.len() >= 86, "the sample description is too short");
    let codec: [u8; 4] = entry[4..8].try_into()?;
    let (width, height) = (u16_at(entry, 32)? as u32, u16_at(entry, 34)? as u32);
    let mut profile = -1;
    let mut tags = None;
    for (kind, body) in boxes(&entry[86..])? {
        match &kind {
            b"avcC" | b"hvcC" if body.len() > 1 => profile = body[1] as i32,
            b"colr" if body.len() >= 10 && matches!(&body[..4], b"nclx" | b"nclc") => {
                tags = Some(Tags {
                    matrix: u16_at(body, 8)?,
                    full_range: (&body[..4] == b"nclx" && body.len() > 10)
                        .then(|| body[10] & 0x80 != 0),
                });
            }
            _ => {}
        }
    }

    let declared = declared_samples(stbl)?;
    let mut decode = Vec::with_capacity(declared);
    let mut time = 0i64;
    let times = child(stbl, b"stts")?.context("no sample times")?;
    for (count, delta) in runs(times, declared, "the time table")? {
        for _ in 0..count {
            decode.push(time);
            time += delta as i64;
        }
    }
    ensure!(!decode.is_empty(), "no samples");
    let mut offsets = Vec::with_capacity(decode.len());
    if let Some(body) = child(stbl, b"ctts")? {
        for (count, offset) in runs(body, declared, "the composition offsets")? {
            offsets.extend(std::iter::repeat_n(offset as i64, count as usize));
        }
    }
    let sync = match child(stbl, b"stss")? {
        Some(body) => {
            let count = u32_at(body, 4)? as usize;
            let mut flags = vec![false; decode.len()];
            for entry in 0..count {
                let number = u32_at(body, 8 + entry * 4)? as usize;
                if let Some(flag) = number.checked_sub(1).and_then(|n| flags.get_mut(n)) {
                    *flag = true;
                }
            }
            flags
        }
        None => vec![true; decode.len()],
    };

    let segments = segments(trak, movie_scale, scale as i64)?;
    let (lead, edits) = (segments[0].target, segments.len());
    let mut displayed: Vec<(i64, i64, bool)> = Vec::with_capacity(decode.len());
    for (index, dts) in decode.iter().enumerate() {
        let media = dts + offsets.get(index).copied().unwrap_or(0);
        if let Some(segment) = segments
            .iter()
            .find(|segment| media >= segment.source && media < segment.source + segment.length)
        {
            displayed.push((
                segment.target + (media - segment.source),
                media,
                sync[index],
            ));
        }
    }
    ensure!(!displayed.is_empty(), "no displayed samples");
    displayed.sort_by_key(|(movie, _, _)| *movie);
    let pts: Vec<i64> = displayed.iter().map(|(movie, _, _)| *movie).collect();
    let media = displayed.iter().map(|(_, media, _)| *media).collect();
    let keyframes = displayed
        .iter()
        .enumerate()
        .filter(|(_, (_, _, sync))| *sync)
        .map(|(index, _)| index)
        .collect();
    Ok(Track {
        width,
        height,
        timescale: scale,
        pts,
        media,
        keyframes,
        codec,
        profile,
        tags,
        lead,
        edits,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut bytes = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(body);
        bytes
    }

    fn full(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        boxed(kind, &[&[0u8; 4], payload].concat())
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|value| value.to_be_bytes()).collect()
    }

    fn media(stts: &[u32], stsz_count: u32, ctts: Option<&[u32]>) -> Vec<u8> {
        let mut entry = vec![0u8; 86];
        entry[..4].copy_from_slice(&86u32.to_be_bytes());
        entry[4..8].copy_from_slice(b"avc1");
        entry[32..34].copy_from_slice(&64u16.to_be_bytes());
        entry[34..36].copy_from_slice(&48u16.to_be_bytes());
        let mut stbl = full(b"stsd", &[&words(&[1]), entry.as_slice()].concat());
        stbl.extend(full(b"stts", &[words(&[stts.len() as u32 / 2]), words(stts)].concat()));
        stbl.extend(full(b"stsz", &words(&[0, stsz_count])));
        if let Some(ctts) = ctts {
            stbl.extend(full(b"ctts", &[words(&[ctts.len() as u32 / 2]), words(ctts)].concat()));
        }
        let minf = boxed(b"minf", &boxed(b"stbl", &stbl));
        let mut mdia = full(b"mdhd", &[&[0u8; 8][..], &words(&[1000, 0, 0])].concat());
        mdia.extend(minf);
        mdia
    }

    fn parse(mdia_body: &[u8], trak_body: &[u8]) -> Result<Track> {
        track(trak_body, mdia_body, 1000)
    }

    #[test]
    fn a_plain_track_gives_its_presentation_times() -> Result<()> {
        let track = parse(&media(&[3, 1000], 3, None), &[])?;
        assert_eq!(track.pts, [0, 1000, 2000]);
        assert_eq!((track.width, track.height), (64, 48));
        Ok(())
    }

    #[test]
    fn a_time_run_longer_than_the_declared_sample_count_is_refused() {
        let error = parse(&media(&[4_000_000_000, 1], 3, None), &[]).unwrap_err();
        assert!(format!("{error:#}").contains("declares 3"), "{error:#}");
    }

    #[test]
    fn an_offset_run_longer_than_the_declared_sample_count_is_refused() {
        let error = parse(&media(&[3, 1000], 3, Some(&[4_000_000_000, 0])), &[]).unwrap_err();
        assert!(format!("{error:#}").contains("declares 3"), "{error:#}");
    }

    #[test]
    fn a_declared_sample_count_beyond_the_limit_is_refused() {
        let error = parse(&media(&[3, 1000], u32::MAX, None), &[]).unwrap_err();
        assert!(format!("{error:#}").contains("more than"), "{error:#}");
    }

    #[test]
    fn a_box_size_that_overflows_is_an_error() {
        let mut data = vec![0, 0, 0, 1];
        data.extend_from_slice(b"free");
        data.extend_from_slice(&u64::MAX.to_be_bytes());
        assert!(boxes(&data).is_err());
    }

    #[test]
    fn an_empty_edit_list_is_an_error() {
        let trak = boxed(b"edts", &boxed(b"elst", &[]));
        let error = parse(&media(&[3, 1000], 3, None), &trak).unwrap_err();
        assert!(format!("{error:#}").contains("edit list"), "{error:#}");
    }

    #[test]
    fn a_fragmented_file_is_refused_by_name() -> Result<()> {
        let mut moov = full(b"mvhd", &[&[0u8; 8][..], &words(&[1000, 0])].concat());
        moov.extend(boxed(b"mvex", &[]));
        let mut file = boxed(b"ftyp", b"isom\0\0\0\0");
        file.extend(boxed(b"moov", &moov));
        let path = std::env::temp_dir().join(format!("vrf-fragmented-{}.mp4", std::process::id()));
        fs::write(&path, file)?;
        let result = read(&path);
        fs::remove_file(&path)?;
        let error = result.unwrap_err();
        assert!(format!("{error:#}").contains("fragmented MP4 is not supported"), "{error:#}");
        Ok(())
    }
}
