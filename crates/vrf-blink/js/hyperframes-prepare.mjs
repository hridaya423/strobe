import { existsSync, mkdirSync, readFileSync, readdirSync, symlinkSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join } from 'node:path';
import { runInNewContext } from 'node:vm';
import { pathToFileURL } from 'node:url';
import { installNative } from './hyperframes-native.mjs';

const [hyperframesDir, projectDir, outDir, fpsArgument, nativeArgument = '{}'] = process.argv.slice(2);
const fps = Number(fpsArgument);
const nativeOptions = JSON.parse(nativeArgument);
const dist = join(hyperframesDir, 'dist');
const log = console.log;
console.log = console.error;
console.info = console.error;

const chunks = readdirSync(dist)
  .filter((name) => name.startsWith('chunk-') && name.endsWith('.js'))
  .map((name) => ({ name, text: readFileSync(join(dist, name), 'utf8') }));
const chunkWith = (marker) => {
  const chunk = chunks.find(({ text }) => text.includes(marker));
  if (!chunk) throw new Error(`${hyperframesDir} has no chunk containing ${JSON.stringify(marker)}; HyperFrames 0.8.216 is the supported version`);
  return chunk;
};
const load = (chunk) => import(pathToFileURL(join(dist, chunk.name)).href);

const cacheRoot = join(dirname(outDir), 'extract-cache', createHash('sha1').update(nativeArgument).digest('hex').slice(0, 12));
if (nativeOptions.native === true) process.env.HYPERFRAMES_EXTRACT_CACHE_DIR = cacheRoot;
const extractChunk = chunkWith('async function extractVideoFramesRangeUnredacted(');
const native = installNative({
  chunk: extractChunk.name,
  extract: nativeOptions.extract ?? [],
  enabled: nativeOptions.native === true,
  registry: join(cacheRoot, 'vrf-native'),
});
const planChunk = chunkWith('async function plan(projectDir, config, planDir');
const engineChunk = chunkWith('function createFrameLookupTable(');
const audioChunk = chunkWith('function parseAudioElements(');
const stubChunk = chunkWith('var HF_EARLY_STUB_IIFE = ');

const { plan } = await load(planChunk);
const { createFrameLookupTable } = await load(engineChunk);
const { parseAudioElements } = await load(audioChunk);

const stubStart = stubChunk.text.indexOf('var HF_EARLY_STUB_IIFE = ') + 'var HF_EARLY_STUB_IIFE = '.length;
const stubEnd = stubChunk.text.indexOf(';\nfunction getHfEarlyStub', stubStart);
const earlyStub = runInNewContext(stubChunk.text.slice(stubStart, stubEnd));
const runtime = readFileSync(join(dist, 'hyperframe.runtime.iife.js'), 'utf8');

mkdirSync(outDir, { recursive: true });
const planDir = join(outDir, 'plan');
const planned = await plan(projectDir, { fps, format: 'mp4', quality: 'standard' }, planDir, { deferAudio: true });

const compiledDir = join(planDir, 'compiled');
const serveDir = join(outDir, 'serve');
mkdirSync(serveDir, { recursive: true });
const html = readFileSync(join(compiledDir, 'index.html'), 'utf8');
const tag = `<script>${runtime.replaceAll('</script', '<\\/script')}</script>`;
const head = html.search(/<head[^>]*>/i);
const afterHead = head < 0 ? 0 : head + html.slice(head).indexOf('>') + 1;
writeFileSync(join(serveDir, 'index.html'), html.slice(0, afterHead) + tag + html.slice(afterHead));
symlinkSync('../plan/video-frames', join(serveDir, '__hyperframes_video_frames'));
writeFileSync(join(outDir, 'early-stub.js'), earlyStub);

const videosPath = join(planDir, 'meta/videos.json');
const { videos = [], extracted = [] } = existsSync(videosPath) ? JSON.parse(readFileSync(videosPath, 'utf8')) : {};
const pad = (n) => String(n).padStart(5, '0');
for (const entry of extracted) {
  const extension = entry.framePattern.split('.').pop();
  entry.framePaths = new Map(
    Array.from({ length: entry.totalFrames }, (_, index) => [index, `${entry.videoId}/frame_${pad(index + 1)}.${extension}`]),
  );
}
const table = extracted.length > 0 ? createFrameLookupTable(videos, extracted, fps) : null;
const natives = {};
const shown = (id, framePath) => {
  const placeholder = native.read(join(planDir, 'video-frames', framePath));
  if (!placeholder) return framePath;
  natives[id] ??= placeholder.clip;
  return placeholder.slot;
};
const frames = Array.from({ length: planned.totalFrames }, (_, frame) =>
  table ? [...table.getActiveFramePayloads(frame / fps)].map(([id, payload]) => [id, shown(id, payload.framePath)]) : [],
);

const attribute = (tag, name) => tag.match(new RegExp(`\\s${name}="([^"]*)"`))?.[1];
const collectTracks = (file, offset, limit) => {
  const source = readFileSync(join(projectDir, file), 'utf8');
  const tracks = parseAudioElements(source).map((track) => ({
    ...track,
    start: track.start + offset,
    end: track.end > 0 ? Math.min(track.end + offset, limit) : 0,
  }));
  for (const [open] of source.matchAll(/<[^>]*data-composition-src="[^"]+"[^>]*>/g)) {
    const start = Number(attribute(open, 'data-start') ?? 0) + offset;
    const duration = Number(attribute(open, 'data-duration') ?? Infinity);
    tracks.push(...collectTracks(attribute(open, 'data-composition-src'), start, Math.min(limit, start + duration)));
  }
  return tracks;
};

writeFileSync(
  join(outDir, 'prepared.json'),
  JSON.stringify({
    fps,
    width: planned.width,
    height: planned.height,
    totalFrames: planned.totalFrames,
    hasVideo: extracted.length > 0,
    videoMetadata: extracted
      .filter((entry) => entry.metadata?.width > 0)
      .map((entry) => ({ id: entry.videoId, width: entry.metadata.width, height: entry.metadata.height })),
    frames,
    natives,
    tracks: collectTracks('index.html', 0, Infinity),
  }),
);
log(JSON.stringify({ planDir }));
