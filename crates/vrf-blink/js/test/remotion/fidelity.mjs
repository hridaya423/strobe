import { spawn } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { Browser } from './cdp.mjs';
import { adapterSource, bundleCorpus, capture, corpusDir } from './harness.mjs';

const HERE = new URL('.', import.meta.url).pathname;
const [id, outDir, step = '50', lead = '0'] = process.argv.slice(2);

const toml = (name) => {
  const text = readFileSync(join(process.env.VRF_REPO ?? join(HERE, '../../../../..'), 'bench/corpus', name, 'corpus.toml'), 'utf8');
  return Object.fromEntries([...text.matchAll(/^(\w+)\s*=\s*"?([^"\n]+)"?\s*$/gm)].map(([, k, v]) => [k, /^\d+$/.test(v) ? Number(v) : v]));
};

const corpus = toml(id);
const name = corpus.source.replace('external/', '');
const bundleDir = bundleCorpus(name, corpus.entry);

const server = spawn('node', [join(HERE, '../../remotion-serve.mjs'), corpusDir(name), bundleDir], { stdio: ['pipe', 'pipe', 'inherit'] });
const { serveUrl, offthreadPort } = await new Promise((resolve, reject) => {
  let buf = '';
  server.stdout.on('data', (d) => {
    buf += d;
    if (buf.includes('\n')) resolve(JSON.parse(buf));
  });
  server.on('exit', (code) => reject(new Error(`remotion-serve exited ${code}`)));
});

mkdirSync(outDir, { recursive: true });
const browser = await Browser.launch();
try {
  const page = await browser.newPage({ width: corpus.width, height: corpus.height });
  await page.addScript(`globalThis.__vrf_rm_config = ${JSON.stringify({ proxyPort: offthreadPort, captureMode: 'png', audioEnabled: false })};\n${adapterSource()}`);
  await page.goto(`${serveUrl}/index.html`);
  const meta = await page.eval(`__vrf.rm.init({ compositionId: ${JSON.stringify(corpus.composition)} })`);
  if (meta.width !== corpus.width || meta.height !== corpus.height || meta.fps !== corpus.fps || meta.durationInFrames !== corpus.frames) {
    throw new Error(`metadata mismatch: ${JSON.stringify(meta)}`);
  }
  const timings = [];
  for (let frame = 0; frame < meta.durationInFrames; frame += Number(step)) {
    for (let before = Math.max(0, frame - Number(lead)); before < frame; before++) {
      await page.eval(`__vrf.rm.seek(${before})`);
      await capture(page, corpus);
    }
    const t0 = performance.now();
    await page.eval(`__vrf.rm.seek(${frame})`);
    const t1 = performance.now();
    const shot = await capture(page, corpus);
    const t2 = performance.now();
    writeFileSync(join(outDir, `${String(frame).padStart(6, '0')}.png`), shot.bytes);
    timings.push({ frame, seekMs: t1 - t0, captureMs: t2 - t1 });
  }
  const mean = (key) => timings.reduce((s, t) => s + t[key], 0) / timings.length;
  console.log(JSON.stringify({ id, lead: Number(lead), frames: timings.length, meta, meanSeekMs: mean('seekMs'), meanCaptureMs: mean('captureMs'), browserErrors: page.exceptions.slice(0, 5) }));
} finally {
  await browser.close();
  server.stdin.end();
}
