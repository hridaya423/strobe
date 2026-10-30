#![cfg(any(target_os = "macos", windows))]

#[cfg(target_os = "macos")]
use vrf_encode::audiotoolbox::{AAC_FRAME_SAMPLES, AacConfig, encode_aac, encode_aac_chunks};
#[cfg(windows)]
use vrf_encode::mediafoundation::{AAC_FRAME_SAMPLES, AacConfig, encode_aac, encode_aac_chunks};

fn tone(frames: usize, channels: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|i| {
            let s = (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
            std::iter::repeat_n(s, channels)
        })
        .collect()
}

#[test]
fn one_second_of_stereo_becomes_aac_lc_packets_with_priming() {
    let config = AacConfig {
        sample_rate: 48_000,
        channels: 2,
        bit_rate: 128_000,
    };
    let stream = encode_aac(&tone(48_000, 2), config).unwrap();

    assert_eq!(stream.audio_specific_config, [0x11, 0x90]);
    assert_eq!(stream.frames, 48_000);
    assert!(stream.priming_frames >= AAC_FRAME_SAMPLES);
    let covered = stream.packets.len() as u64 * u64::from(AAC_FRAME_SAMPLES);
    assert!(covered >= u64::from(stream.priming_frames) + stream.frames);
    assert!(covered < u64::from(stream.priming_frames) + stream.frames + 3 * 1024);

    let bytes: usize = stream.packets.iter().map(Vec::len).sum();
    let kbps = bytes * 8 / 1000;
    assert!((64..=192).contains(&kbps), "{kbps} kb/s");
    assert!(stream.packets.iter().all(|p| !p.is_empty()));
    assert!(stream.max_packet_bytes >= stream.packets.iter().map(Vec::len).max().unwrap() as u32);
}

#[test]
fn mono_uses_channel_configuration_one() {
    let config = AacConfig {
        sample_rate: 48_000,
        channels: 1,
        bit_rate: 64_000,
    };
    let stream = encode_aac(&tone(24_000, 1), config).unwrap();
    assert_eq!(stream.audio_specific_config, [0x11, 0x88]);
    assert_eq!(stream.frames, 24_000);
}

#[test]
fn input_that_is_not_whole_frames_is_rejected() {
    let config = AacConfig {
        sample_rate: 48_000,
        channels: 2,
        bit_rate: 128_000,
    };
    assert!(encode_aac(&[0.0; 3], config).is_err());
}

#[test]
fn chunks_of_any_size_encode_to_the_same_stream_as_one_buffer() {
    let config = AacConfig {
        sample_rate: 48_000,
        channels: 2,
        bit_rate: 128_000,
    };
    let samples = tone(100_000, 2);
    let whole = encode_aac(&samples, config).unwrap();
    for size in [2, 1_000, 2_046, 48_000, 199_998] {
        let chunks = samples.chunks(size).map(<[f32]>::to_vec);
        assert_eq!(
            encode_aac_chunks(chunks, config).unwrap(),
            whole,
            "chunks of {size}"
        );
    }
}
