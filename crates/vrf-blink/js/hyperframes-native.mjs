import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { registerHooks } from 'node:module';
import { extname, join } from 'node:path';

const MARK = 'async function extractVideoFramesRangeUnredacted(videoPath, videoId, startTime, duration, options, signal, config, outputDirOverride) {';
const HOOK = `${MARK}
  const native = await globalThis.__vrfNative?.(videoPath, startTime, duration, options, outputDirOverride ?? join12(options.outputDir, videoId), videoId, { extractMediaMetadata, extractionFrameCountForDuration, resolvePlayableVideoDuration, resolveFrameFormat, isHdrColorSpace, toFps, fpsToNumber, FRAME_FILENAME_PREFIX });
  if (native) return native;
`;

const CONTAINERS = new Set(['.mp4', '.mov', '.m4v']);
const CODECS = new Set(['h264', 'hevc']);
const PIXEL_FORMATS = new Set(['yuv420p', 'yuvj420p']);
const MATRICES = new Map([
  ['bt709', '709'],
  ['bt470bg', '601'],
  ['smpte170m', '601'],
]);
const SEGMENT_SECONDS = 120;
const COMPILED = '/compiled/';

const ratio = (text) => {
  const [num, den] = String(text).split('/').map(Number);
  return num > 0 && den > 0 ? [num, den] : null;
};

const microseconds = (text) => {
  const [whole, fraction = ''] = String(text).split('.');
  return Number(whole) * 1e6 + Number(fraction.slice(0, 6).padEnd(6, '0'));
};

const probe = (path) => {
  const output = JSON.parse(
    execFileSync(
      'ffprobe',
      ['-v', 'error', '-select_streams', 'v:0', '-show_entries', 'stream=r_frame_rate,time_base,start_time,color_range,color_space:format=start_time', '-of', 'json', path],
      { maxBuffer: 1 << 24 },
    ),
  );
  const stream = output.streams[0];
  return {
    frameRate: ratio(stream.r_frame_rate),
    timeBase: ratio(stream.time_base),
    streamStart: Number(stream.start_time ?? 0),
    containerStartUs: Math.round(Number(output.format?.start_time ?? 0) * 1e6),
    colorRange: stream.color_range,
    colorSpace: stream.color_space ?? '',
  };
};

export const installNative = ({ chunk, extract, enabled, registry }) => {
  const known = new Set();
  const probes = new Map();
  const forced = new Set(extract);

  registerHooks({
    load(url, context, nextLoad) {
      const result = nextLoad(url, context);
      if (!url.endsWith(`/${chunk}`)) return result;
      const text = String(result.source);
      if (!text.includes(MARK)) throw new Error(`${chunk} no longer contains the video extractor; HyperFrames 0.8.216 is the supported version`);
      return { ...result, source: text.replace(MARK, HOOK) };
    },
  });

  globalThis.__vrfNative = async (videoPath, startTime, duration, options, directory, videoId, helpers) => {
    if (!enabled) return null;
    if (options.finalFrameOnly || options.frameRanges || options.tailProbe || options.sdrToHdrTransfer || options.toneMapHdrToSdr) return null;
    const inside = videoPath.lastIndexOf(COMPILED);
    if (inside < 0 || forced.has(videoPath.slice(inside + COMPILED.length))) return null;
    if (!CONTAINERS.has(extname(videoPath).toLowerCase())) return null;
    const metadata = await helpers.extractMediaMetadata(videoPath);
    if (!CODECS.has(metadata.videoCodec) || !PIXEL_FORMATS.has(metadata.pixelFormat) || metadata.hasAlpha) return null;
    if (helpers.isHdrColorSpace(metadata.colorSpace) || helpers.resolveFrameFormat(metadata, options.format) !== 'jpg') return null;
    if (startTime >= helpers.resolvePlayableVideoDuration(metadata)) return null;
    const box = options.shownBox;
    if (box && Math.max(Math.ceil(box.width) / metadata.width, Math.ceil(box.height) / metadata.height) < 0.5) return null;
    const fpsRatio = helpers.toFps(options.fps);
    const fpsNumber = helpers.fpsToNumber(fpsRatio);
    const total = helpers.extractionFrameCountForDuration(duration, fpsRatio, metadata.isVFR);
    if (total > Math.round(SEGMENT_SECONDS * fpsNumber)) return null;
    if (!probes.has(videoPath)) probes.set(videoPath, probe(videoPath));
    const facts = probes.get(videoPath);
    const matrixTag = metadata.colorSpace?.colorSpace ?? facts.colorSpace;
    const matrix = MATRICES.get(matrixTag) ?? (matrixTag && matrixTag !== 'unknown' ? null : metadata.height >= 720 ? '709' : '601');
    if (!matrix || !facts.frameRate || !facts.timeBase || metadata.width % 2 || metadata.height % 2) return null;
    const offset = metadata.mediaTimeOffsetSeconds ?? 0;
    const clip = {
      path: videoPath.slice(inside + COMPILED.length),
      start: startTime,
      seek: startTime - offset,
      lead: (metadata.videoStreamLeadSeconds ?? 0) + offset,
      containerStartUs: facts.containerStartUs,
      frameRate: facts.frameRate,
      vfr: metadata.isVFR,
      fps: [fpsRatio.num, fpsRatio.den],
      matrix,
      full: metadata.pixelFormat === 'yuvj420p' || facts.colorRange === 'pc',
    };
    const text = JSON.stringify(clip);
    const id = createHash('sha1').update(text).digest('hex').slice(0, 12);
    if (!known.has(id)) {
      mkdirSync(registry, { recursive: true });
      writeFileSync(join(registry, `${id}.json`), text);
      known.add(id);
    }
    mkdirSync(directory, { recursive: true });
    const framePaths = new Map();
    for (let slot = 0; slot < total; slot += 1) {
      const name = `${helpers.FRAME_FILENAME_PREFIX}${String(slot + 1).padStart(5, '0')}.jpg`;
      writeFileSync(join(directory, name), `${id}:${slot}`);
      framePaths.set(slot, join(directory, name));
    }
    return {
      videoId,
      srcPath: videoPath,
      outputDir: directory,
      framePattern: `${helpers.FRAME_FILENAME_PREFIX}%05d.jpg`,
      fps: fpsNumber,
      totalFrames: total,
      metadata,
      framePaths,
    };
  };

  return {
    read(file) {
      if (statSync(file).size > 24) return null;
      const text = readFileSync(file, 'utf8');
      const match = /^([0-9a-f]{12}):(\d+)$/.exec(text);
      if (!match) return null;
      return { clip: JSON.parse(readFileSync(join(registry, `${match[1]}.json`), 'utf8')), slot: Number(match[2]) };
    },
  };
};
