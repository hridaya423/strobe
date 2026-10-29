import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { adapterSource, seek } from '../../../vrf-blink/js/test/remotion/harness.mjs';
import { Browser } from '../../../vrf-blink/js/test/remotion/cdp.mjs';
import { serve } from '../../../vrf-blink/js/test/remotion/serve.mjs';

const HERE = new URL('.', import.meta.url).pathname;
const BUNDLE = join(HERE, '../../../vrf-blink/js/remotion-bundle.mjs');

const [projectDir, entry, compositionId, to] = process.argv.slice(2);
const work = mkdtempSync(join(tmpdir(), 'vrf-rm-assets-'));
let browser, server;
try {
  const bundleDir = join(work, 'bundle');
  execFileSync('node', [BUNDLE, projectDir, entry, bundleDir], { stdio: ['ignore', 'ignore', 'inherit'] });
  server = await serve(bundleDir);
  browser = await Browser.launch();
  const page = await browser.newPage({ width: 320, height: 180 });
  const environment = "globalThis.process = { env: { NODE_ENV: 'production' } };";
  await page.addScript(`${environment}\nglobalThis.__vrf_rm_config = {};\n${adapterSource()}`);
  await page.goto(`${server.url}/index.html`);
  const { fps, durationInFrames } = await page.eval(`__vrf.rm.init(${JSON.stringify({ compositionId })})`);
  const end = to === undefined ? durationInFrames : Number(to);
  const frames = [];
  for (let frame = 0; frame < end; frame++) {
    const { assets } = await seek(page, frame);
    frames.push(assets);
  }
  process.stdout.write(JSON.stringify({ fps, durationInFrames, frames }));
} finally {
  await browser?.close();
  await server?.close();
  rmSync(work, { recursive: true, force: true });
}
