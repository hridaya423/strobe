use anyhow::{Result, bail, ensure};

/// An `AVCDecoderConfigurationRecord` (the body of an `avcC` box) with 4-byte NAL lengths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcRecord {
    pub profile: u8,
    pub compatibility: u8,
    pub level: u8,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    pub extension: Vec<u8>,
}

fn take<'a>(bytes: &mut &'a [u8], len: usize) -> Result<&'a [u8]> {
    ensure!(bytes.len() >= len, "avcC is truncated");
    let (head, tail) = bytes.split_at(len);
    *bytes = tail;
    Ok(head)
}

fn sets(bytes: &mut &[u8], count: usize) -> Result<Vec<Vec<u8>>> {
    (0..count)
        .map(|_| {
            let len = u16::from_be_bytes(take(bytes, 2)?.try_into()?) as usize;
            Ok(take(bytes, len)?.to_vec())
        })
        .collect()
}

impl AvcRecord {
    pub fn parse(mut bytes: &[u8]) -> Result<AvcRecord> {
        let head = take(&mut bytes, 6)?;
        ensure!(head[0] == 1, "avcC version {} is not 1", head[0]);
        ensure!(head[4] & 3 == 3, "NAL lengths are not 4 bytes");
        let sps = sets(&mut bytes, (head[5] & 0x1f) as usize)?;
        let pps_count = take(&mut bytes, 1)?[0] as usize;
        let pps = sets(&mut bytes, pps_count)?;
        Ok(AvcRecord {
            profile: head[1],
            compatibility: head[2],
            level: head[3],
            sps,
            pps,
            extension: bytes.to_vec(),
        })
    }

    /// Builds the record from parameter sets, taking the profile and level from the first SPS.
    /// High profiles carry the chroma format and bit depths after the sets.
    pub fn from_sets(sps: Vec<Vec<u8>>, pps: Vec<Vec<u8>>) -> Result<AvcRecord> {
        let Some(first) = sps.first().filter(|s| s.len() >= 4) else {
            bail!("no SPS");
        };
        let (profile, compatibility, level) = (first[1], first[2], first[3]);
        let extension = if matches!(profile, 100 | 110 | 122 | 144) {
            vec![0xfc | 1, 0xf8, 0xf8, 0]
        } else {
            Vec::new()
        };
        Ok(AvcRecord {
            profile,
            compatibility,
            level,
            sps,
            pps,
            extension,
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![
            1,
            self.profile,
            self.compatibility,
            self.level,
            0xfc | 3,
            0xe0 | self.sps.len() as u8,
        ];
        for sps in &self.sps {
            out.extend((sps.len() as u16).to_be_bytes());
            out.extend(sps);
        }
        out.push(self.pps.len() as u8);
        for pps in &self.pps {
            out.extend((pps.len() as u16).to_be_bytes());
            out.extend(pps);
        }
        out.extend(&self.extension);
        out
    }

    /// Adds the parameter sets of `other` that this record lacks, so one sample entry decodes
    /// both streams. Their SPS and PPS ids must differ. The level is the higher of the two and
    /// the compatibility flags are those both streams satisfy.
    pub fn merge(&mut self, other: &AvcRecord) -> Result<()> {
        ensure!(
            self.profile == other.profile,
            "profile {} and {} cannot share a sample entry",
            self.profile,
            other.profile
        );
        self.level = self.level.max(other.level);
        self.compatibility &= other.compatibility;
        for set in &other.sps {
            if !self.sps.contains(set) {
                self.sps.push(set.clone());
            }
        }
        for set in &other.pps {
            if !self.pps.contains(set) {
                self.pps.push(set.clone());
            }
        }
        ensure!(
            self.sps.len() < 32 && self.pps.len() < 256,
            "too many parameter sets"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sps(id_tag: u8, level: u8) -> Vec<u8> {
        vec![0x67, 100, 0, level, id_tag]
    }

    #[test]
    fn round_trips_and_merges_two_streams() {
        let a = AvcRecord::from_sets(vec![sps(0xaa, 40)], vec![vec![0x68, 1]]).unwrap();
        assert_eq!(AvcRecord::parse(&a.to_bytes()).unwrap(), a);

        let b = AvcRecord::from_sets(vec![sps(0xbb, 42)], vec![vec![0x68, 2]]).unwrap();
        let mut merged = a.clone();
        merged.merge(&b).unwrap();
        assert_eq!(merged.level, 42);
        assert_eq!(merged.sps.len(), 2);
        assert_eq!(merged.pps.len(), 2);
        assert_eq!(AvcRecord::parse(&merged.to_bytes()).unwrap(), merged);

        merged.merge(&b).unwrap();
        assert_eq!(merged.sps.len(), 2);
    }

    #[test]
    fn rejects_truncated_and_foreign_records() {
        assert!(AvcRecord::parse(&[1, 100, 0]).is_err());
        let record = AvcRecord::from_sets(vec![sps(1, 40)], vec![vec![0x68]]).unwrap();
        let mut bytes = record.to_bytes();
        bytes[0] = 2;
        assert!(AvcRecord::parse(&bytes).is_err());
        let mut other = record.clone();
        other.profile = 66;
        assert!(record.clone().merge(&other).is_err());
    }
}
