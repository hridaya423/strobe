import { createServer } from 'node:http';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, extname, join, normalize } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { runInNewContext } from 'node:vm';
import { inflateSync } from 'node:zlib';
import { benchDir, launch } from './cdp.js';

const here = dirname(fileURLToPath(import.meta.url));
const hfDist = join(benchDir, 'engines/node_modules/hyperframes/dist');

export const adapterSource = readFileSync(join(here, '../../hyperframes.js'), 'utf8');
export const clockStubSource = readFileSync(join(here, 'clock-stub.js'), 'utf8');
export const runtimeSource = readFileSync(join(hfDist, 'hyperframe.runtime.iife.js'), 'utf8');
export const gsapPath = join(hfDist, 'vendor/npm/gsap@3.15.0/dist/gsap.min.js');
export const corpusDir = join(benchDir, 'corpus/external/hyperframes-launches');
export const refsDir = join(benchDir, 'refs');

const producerChunk = readFileSync(join(hfDist, 'chunk-CX4TSC7T.js'), 'utf8');
const earlyStubStart = producerChunk.indexOf('var HF_EARLY_STUB_IIFE = ') + 'var HF_EARLY_STUB_IIFE = '.length;
const earlyStubEnd = producerChunk.indexOf(';\nfunction getHfEarlyStub', earlyStubStart);
export const earlyStubSource = runInNewContext(producerChunk.slice(earlyStubStart, earlyStubEnd));

const types = {
  '.html': 'text/html',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.json': 'application/json',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.svg': 'image/svg+xml',
  '.woff2': 'font/woff2',
  '.woff': 'font/woff',
  '.ttf': 'font/ttf',
  '.mp4': 'video/mp4',
  '.webm': 'video/webm',
};

export const pageScripts = [earlyStubSource, adapterSource, clockStubSource];

export async function serve(roots, extra = {}) {
  const list = Array.isArray(roots) ? roots : [roots];
  const server = createServer((req, res) => {
    const path = decodeURIComponent(new URL(req.url, 'http://x').pathname);
    if (extra[path]) {
      res.writeHead(200, { 'content-type': types[extname(path)] ?? 'application/octet-stream' });
      res.end(extra[path]);
      return;
    }
    const rel = normalize(path === '/' ? '/index.html' : path);
    const file = list.map((root) => join(root, rel)).find((f) => existsSync(f) && statSync(f).isFile());
    if (!file) {
      res.writeHead(404).end();
      return;
    }
    const type = types[extname(file)] ?? 'application/octet-stream';
    if (extname(file) === '.html' && rel === '/index.html') {
      const html = readFileSync(file, 'utf8');
      const tag = `<script>${runtimeSource.replaceAll('</script', '<\\/script')}</script>`;
      const at = html.search(/<head[^>]*>/i);
      const injected = at < 0 ? tag + html : html.slice(0, at + html.slice(at).indexOf('>') + 1) + tag + html.slice(at + html.slice(at).indexOf('>') + 1);
      res.writeHead(200, { 'content-type': type });
      res.end(injected);
      return;
    }
    const bytes = readFileSync(file);
    const range = req.headers.range && /bytes=(\d+)-(\d*)/.exec(req.headers.range);
    if (range) {
      const start = Number(range[1]);
      const end = range[2] ? Number(range[2]) : bytes.length - 1;
      res.writeHead(206, {
        'content-type': type,
        'accept-ranges': 'bytes',
        'content-range': `bytes ${start}-${end}/${bytes.length}`,
        'content-length': end - start + 1,
      });
      res.end(bytes.subarray(start, end + 1));
      return;
    }
    res.writeHead(200, { 'content-type': type, 'accept-ranges': 'bytes', 'content-length': bytes.length });
    res.end(bytes);
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  return { origin: `http://127.0.0.1:${server.address().port}`, close: () => { server.closeAllConnections(); server.close(); } };
}

export function project(files) {
  const dir = mkdtempSync(join(tmpdir(), 'vrf-hf-project-'));
  cpSync(gsapPath, join(dir, 'gsap.min.js'));
  for (const [name, content] of Object.entries(files)) {
    mkdirSync(dirname(join(dir, name)), { recursive: true });
    writeFileSync(join(dir, name), content);
  }
  return dir;
}

export const composition = ({ width = 320, height = 180, duration = 1, fps = 10, attrs = '', body = '', script = '', style = '' }) => `<!doctype html>
<html><head><meta charset="utf-8"><style>body{margin:0;background:#000}#root{position:relative;width:${width}px;height:${height}px;overflow:hidden;background:#123}${style}</style></head><body>
<div id="root" data-composition-id="main" data-width="${width}" data-height="${height}" data-duration="${duration}" data-fps="${fps}" ${attrs}>${body}</div>
<script src="gsap.min.js"></script>
<script>${script}</script></body></html>`;

export async function compileProject(projectDir, fps) {
  const { plan } = await import(pathToFileURL(join(hfDist, 'chunk-CX4TSC7T.js')).href);
  const planDir = mkdtempSync(join(tmpdir(), 'vrf-hf-plan-'));
  const log = console.log;
  console.log = () => {};
  try {
    await plan(projectDir, { fps, format: 'mp4', quality: 'standard' }, planDir);
  } finally {
    console.log = log;
  }
  return {
    compiledDir: join(planDir, 'compiled'),
    planDir,
    videos: existsSync(join(planDir, 'meta/videos.json')) ? JSON.parse(readFileSync(join(planDir, 'meta/videos.json'), 'utf8')) : [],
    composition: JSON.parse(readFileSync(join(planDir, 'meta/composition.json'), 'utf8')),
  };
}

export async function withBrowser(fn) {
  const browser = await launch();
  try {
    return await fn(browser);
  } finally {
    await browser.close();
  }
}

export async function openHf(browser, root, { width, height, extra, scripts = pageScripts, init } = {}) {
  const server = await serve(root, extra);
  const page = await browser.newPage({ width, height, scripts });
  await page.goto(`${server.origin}/index.html`);
  const meta = init === false ? null : await page.evaluate(`__vrf.hf.init(${JSON.stringify(init ?? {})})`);
  return {
    page,
    meta,
    server,
    seek: (frame, options) => page.evaluate(`__vrf.hf.seek(${frame}, ${JSON.stringify(options ?? {})})`),
    async close() {
      await browser.closePage(page);
      server.close();
    },
  };
}

export function pngPixel(png, x, y) {
  let offset = 8;
  const idat = [];
  let width = 0;
  let channels = 4;
  while (offset < png.length) {
    const length = png.readUInt32BE(offset);
    const type = png.toString('ascii', offset + 4, offset + 8);
    const data = png.subarray(offset + 8, offset + 8 + length);
    if (type === 'IHDR') {
      width = data.readUInt32BE(0);
      channels = data[9] === 6 ? 4 : 3;
    }
    if (type === 'IDAT') idat.push(data);
    offset += 12 + length;
  }
  const raw = inflateSync(Buffer.concat(idat));
  const stride = width * channels;
  const rows = [];
  for (let row = 0, pos = 0; pos < raw.length; row++, pos += stride + 1) {
    const filter = raw[pos];
    const line = Buffer.from(raw.subarray(pos + 1, pos + 1 + stride));
    const up = rows[row - 1];
    for (let i = 0; i < stride; i++) {
      const left = i >= channels ? line[i - channels] : 0;
      const above = up ? up[i] : 0;
      const upLeft = up && i >= channels ? up[i - channels] : 0;
      const paeth = () => {
        const p = left + above - upLeft;
        const pa = Math.abs(p - left);
        const pb = Math.abs(p - above);
        const pc = Math.abs(p - upLeft);
        return pa <= pb && pa <= pc ? left : pb <= pc ? above : upLeft;
      };
      line[i] = (line[i] + [0, left, above, (left + above) >> 1, paeth()][filter]) & 255;
    }
    rows.push(line);
  }
  const at = rows[y].subarray(x * channels, x * channels + channels);
  return [...at];
}
