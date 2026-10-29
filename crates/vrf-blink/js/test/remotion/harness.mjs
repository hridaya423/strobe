import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, symlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { Browser } from './cdp.mjs';
import { serve } from './serve.mjs';

const HERE = new URL('.', import.meta.url).pathname;
const JS = join(HERE, '../..');
const REPO = join(JS, '../../..');
const BENCH = process.env.VRF_BENCH_DIR ?? join(REPO, 'bench');
const CACHE = process.env.VRF_RM_CACHE ?? join(REPO, 'target/remotion-js');

export const PROJECTS = {
  '4.0.60': 'transitions-video',
  '4.0.169': '4-0-trailer',
  '4.0.240': 'github-unwrapped',
};
export const VERSIONS = Object.keys(PROJECTS);

export const corpusDir = (name) => join(BENCH, 'corpus/external', name);
export const adapterSource = () => readFileSync(join(JS, 'remotion.js'), 'utf8');

const bundled = new Map();

function bundle(projectDir, entry, outDir) {
  if (!existsSync(join(outDir, 'index.html'))) {
    mkdirSync(outDir, { recursive: true });
    execFileSync('node', [join(JS, 'remotion-bundle.mjs'), projectDir, entry, outDir], { stdio: ['ignore', 'ignore', 'inherit'] });
  }
  return outDir;
}

export function bundleFixture(version, fixture) {
  const key = `${version}/${fixture}`;
  if (!bundled.has(key)) {
    const proj = join(CACHE, 'proj', key);
    if (!existsSync(join(proj, 'package.json'))) {
      mkdirSync(join(proj, 'src'), { recursive: true });
      writeFileSync(join(proj, 'package.json'), JSON.stringify({ name: `vrf-${fixture}`, private: true }));
      symlinkSync(join(corpusDir(PROJECTS[version]), 'node_modules'), join(proj, 'node_modules'));
    }
    cpSync(join(HERE, 'fixtures', fixture), join(proj, 'src'), { recursive: true });
    const sources = createHash('sha1');
    for (const file of readdirSync(join(HERE, 'fixtures', fixture)).sort()) sources.update(readFileSync(join(HERE, 'fixtures', fixture, file)));
    bundled.set(key, bundle(proj, join(proj, 'src/index.jsx'), join(CACHE, 'out', key, sources.digest('hex').slice(0, 10))));
  }
  return bundled.get(key);
}

export function bundleCorpus(name, entry) {
  const key = `corpus/${name}`;
  if (!bundled.has(key)) {
    const dir = corpusDir(name);
    bundled.set(key, bundle(dir, join(dir, entry), join(CACHE, 'out', key)));
  }
  return bundled.get(key);
}

export async function launch(bundleDir, { config = {}, width = 320, height = 180, onRequest, init } = {}) {
  const source = adapterSource();
  const server = await serve(bundleDir, { onRequest });
  const browser = await Browser.launch();
  const run = {
    browser,
    server,
    config,
    close: async () => {
      await browser.close();
      await server.close();
    },
  };
  try {
    run.page = await browser.newPage({ width, height });
    await run.page.addScript(`globalThis.__vrf_rm_config = ${JSON.stringify(config)};\n${source}`);
    await run.page.goto(`${server.url}/index.html`);
    if (init) run.meta = await run.page.eval(`__vrf.rm.init(${JSON.stringify(init)})`);
  } catch (err) {
    await run.close();
    throw err;
  }
  return run;
}

export const seek = (page, frame, opts = {}) => page.eval(`__vrf.rm.seek(${frame}, ${JSON.stringify(opts)})`);

export const decodePng = (page, bytes) => decodeImage(page, { format: 'png', bytes });

export async function capture(page, { width, height }) {
  const spec = await page.eval('__vrf.rm.captureParams()');
  const png = spec.format === 'raw';
  const params = {
    format: png ? 'png' : spec.format,
    captureBeyondViewport: true,
    fromSurface: true,
    clip: { x: 0, y: 0, width, height, scale: 1 },
  };
  if (spec.quality !== undefined) params.quality = spec.quality;
  if (spec.optimizeForSpeed) params.optimizeForSpeed = true;
  if (spec.backgroundOverride) await page.send('Emulation.setDefaultBackgroundColorOverride', { color: spec.backgroundOverride });
  try {
    const { data } = await page.send('Page.captureScreenshot', params);
    return { format: params.format, bytes: Buffer.from(data, 'base64') };
  } finally {
    if (spec.backgroundOverride) await page.send('Emulation.setDefaultBackgroundColorOverride');
  }
}

export async function decodeImage(page, { format, bytes }) {
  const rgba = await page.eval(`(async () => {
    const blob = await (await fetch('data:image/${format};base64,${bytes.toString('base64')}')).blob();
    const bmp = await createImageBitmap(blob, { premultiplyAlpha: 'none', colorSpaceConversion: 'none' });
    const canvas = new OffscreenCanvas(bmp.width, bmp.height);
    const ctx = canvas.getContext('2d', { willReadFrequently: true, colorSpace: 'srgb' });
    ctx.drawImage(bmp, 0, 0);
    const { data } = ctx.getImageData(0, 0, bmp.width, bmp.height);
    return { width: bmp.width, height: bmp.height, data: Array.from(data) };
  })()`);
  return { ...rgba, at: (x, y) => rgba.data.slice((y * rgba.width + x) * 4, (y * rgba.width + x) * 4 + 4) };
}
