import { execFileSync } from 'node:child_process';
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { launch } from './cdp.js';
import { compileProject, corpusDir, pageScripts, refsDir, serve } from './host.js';
import { benchDir } from './cdp.js';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '../../../../..');
const [id, outRoot, every = '50', gpuFlag = 'gpu'] = process.argv.slice(2);

const toml = readFileSync(join(benchDir, 'corpus', id, 'corpus.toml'), 'utf8');
const field = (name) => /=\s*"?([^"\n]+)"?/.exec(toml.match(new RegExp(`^${name}\\s*=.*$`, 'm'))[0])[1];
const source = join(benchDir, 'corpus', field('source'));
const width = Number(field('width'));
const height = Number(field('height'));
const fps = Number(field('fps'));
const totalFrames = Number(field('frames'));

const hfDist = join(benchDir, 'engines/node_modules/hyperframes/dist');
const engine = await import(pathToFileURL(join(hfDist, 'chunk-VZENYSX7.js')).href);

const { compiledDir, videos } = await compileProject(source, fps);
const videoDir = join(compiledDir, '__hyperframes_video_frames');
let lookup = null;
let metadataHints = [];
if (videos.videos?.length) {
  const extraction = await engine.extractAllVideoFrames(
    videos.videos,
    compiledDir,
    { fps: { num: fps, den: 1 }, outputDir: videoDir, format: 'auto', toneMapHdrToSdr: true, timelineEnd: totalFrames / fps },
    undefined,
    engine.DEFAULT_CONFIG,
    compiledDir,
  );
  lookup = engine.createFrameLookupTable(videos.videos, extraction.extracted, fps);
  metadataHints = extraction.extracted
    .filter((e) => e.metadata?.width > 0)
    .map((e) => ({ id: e.videoId, width: e.metadata.width, height: e.metadata.height }));
}

const server = await serve([compiledDir, source]);
const windowed = process.env.HF_WINDOWED === '1';
const browser = await launch({ gpu: gpuFlag === 'metal' ? 'metal' : gpuFlag === 'gpu', windowSize: windowed ? [width, height] : undefined });
const page = await browser.newPage({ width, height, scripts: pageScripts, emulate: !windowed });
const outDir = join(outRoot, id, 'frames');
mkdirSync(outDir, { recursive: true });
const errors = [];
try {
  await page.goto(`${server.origin}/index.html`);
  const meta = await page.evaluate(`__vrf.hf.init(${JSON.stringify({ fps: [fps, 1], videoMetadataHints: metadataHints })})`);
  const refFrames = new Set(readdirSync(join(refsDir, id, 'frames')).map((n) => Number(n.replace('.png', ''))));
  const step = Number(every);
  const t0 = Date.now();
  let captured = 0;
  for (let frame = 0; frame < totalFrames; frame += step) {
    if (!refFrames.has(frame)) continue;
    const payloads = lookup ? lookup.getActiveFramePayloads(frame / fps) : null;
    const videoEntries = payloads
      ? [...payloads].map(([videoId, p]) => ({ id: videoId, src: `/${relative(compiledDir, p.framePath)}` }))
      : undefined;
    await page.evaluate(`__vrf.hf.seek(${frame}, ${JSON.stringify({ videos: videoEntries })})`);
    const png = await page.screenshot({ clip: { width, height } });
    writeFileSync(join(outDir, `${String(frame).padStart(6, '0')}.png`), png);
    captured++;
  }
  console.log(JSON.stringify({ id, meta: { frames: meta.frames, width: meta.width, height: meta.height }, captured, ms: Date.now() - t0, errors: page.errors.slice(0, 5) }));
} finally {
  await browser.close();
  server.close();
}

const scoreOut = join(outRoot, id, 'score');
mkdirSync(scoreOut, { recursive: true });
execFileSync(
  'cargo',
  ['run', '-q', '-p', 'vrf-bench', '--', 'score', '--a', outDir, '--b', join(refsDir, id, 'frames'), '--frames', `every:${every}`, '--out', join(scoreOut, 'score.json')],
  { cwd: repo, stdio: 'inherit' },
);
