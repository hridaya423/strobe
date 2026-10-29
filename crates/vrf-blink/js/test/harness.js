import { spawn, execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
export const clockSource = readFileSync(resolve(here, '../clock.js'), 'utf8');

const mainCheckout = dirname(
  execFileSync('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: here, encoding: 'utf8' }).trim(),
);
const benchDir = process.env.VRF_BENCH_DIR ?? join(mainCheckout, 'bench');

function findChrome() {
  if (process.env.CHROME_HEADLESS_SHELL) return process.env.CHROME_HEADLESS_SHELL;
  const root = join(benchDir, 'engines/browsers/chrome-headless-shell');
  const version = readdirSync(root).sort().at(-1);
  const platformDir = readdirSync(join(root, version)).find((d) => d.startsWith('chrome-headless-shell-'));
  return join(root, version, platformDir, 'chrome-headless-shell');
}

export const gsapPath = join(benchDir, 'engines/node_modules/hyperframes/dist/vendor/npm/gsap@3.15.0/dist/gsap.min.js');

const types = { '.html': 'text/html', '.js': 'text/javascript', '.webm': 'video/webm' };

export async function serve(root, extra = {}) {
  const server = createServer((req, res) => {
    const path = new URL(req.url, 'http://x').pathname;
    const file = extra[path] ?? join(root, path);
    if (!existsSync(file)) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, { 'content-type': types[extname(file)] ?? 'application/octet-stream' });
    res.end(readFileSync(file));
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  return { origin: `http://127.0.0.1:${server.address().port}`, close: () => server.close() };
}

class Connection {
  constructor(ws) {
    this.ws = ws;
    this.nextId = 1;
    this.pending = new Map();
    this.handlers = new Map();
    ws.addEventListener('message', ({ data }) => {
      const msg = JSON.parse(data);
      if (msg.id) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        if (msg.error) reject(new Error(`${msg.error.message} (${JSON.stringify(msg.error)})`));
        else resolve(msg.result);
        return;
      }
      for (const handler of this.handlers.get(`${msg.sessionId}:${msg.method}`) ?? []) handler(msg.params);
    });
  }

  send(method, params = {}, sessionId) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params, sessionId }));
    });
  }

  on(sessionId, method, handler) {
    const key = `${sessionId}:${method}`;
    this.handlers.set(key, [...(this.handlers.get(key) ?? []), handler]);
  }
}

export class Page {
  constructor(connection, sessionId, targetId) {
    this.connection = connection;
    this.sessionId = sessionId;
    this.targetId = targetId;
  }

  close() {
    return this.connection.send('Target.closeTarget', { targetId: this.targetId });
  }

  send(method, params) {
    return this.connection.send(method, params, this.sessionId);
  }

  once(method) {
    return new Promise((resolve) => {
      this.connection.on(this.sessionId, method, resolve);
    });
  }

  async evaluate(expression) {
    const { result, exceptionDetails } = await this.send('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (exceptionDetails) {
      throw new Error(exceptionDetails.exception?.description ?? exceptionDetails.text);
    }
    return result.value;
  }

  async goto(url) {
    const loaded = this.once('Page.loadEventFired');
    await this.send('Page.navigate', { url });
    await loaded;
  }

  async screenshotHash() {
    const { data } = await this.send('Page.captureScreenshot', { format: 'png' });
    return createHash('sha256').update(data).digest('hex');
  }
}

export async function launch() {
  const profile = mkdtempSync(join(tmpdir(), 'vrf-clock-'));
  const child = spawn(
    findChrome(),
    [
      '--remote-debugging-port=0',
      `--user-data-dir=${profile}`,
      '--no-first-run',
      '--hide-scrollbars',
      '--force-device-scale-factor=1',
      '--force-color-profile=srgb',
      '--disable-gpu',
      'about:blank',
    ],
    { stdio: ['ignore', 'ignore', 'pipe'] },
  );
  const endpoint = await new Promise((resolve, reject) => {
    child.on('exit', (code) => reject(new Error(`browser exited with ${code}`)));
    child.stderr.on('data', (chunk) => {
      const match = /DevTools listening on (ws:\/\/\S+)/.exec(chunk);
      if (match) resolve(match[1]);
    });
  });
  const ws = new WebSocket(endpoint);
  await new Promise((resolve) => ws.addEventListener('open', resolve));
  const connection = new Connection(ws);

  return {
    connection,
    async newPage({ clock = true, width = 320, height = 240 } = {}) {
      const { targetId } = await connection.send('Target.createTarget', { url: 'about:blank' });
      const { sessionId } = await connection.send('Target.attachToTarget', { targetId, flatten: true });
      const page = new Page(connection, sessionId, targetId);
      await page.send('Page.enable');
      await page.send('Runtime.enable');
      await page.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });
      if (clock) await page.send('Page.addScriptToEvaluateOnNewDocument', { source: clockSource });
      return page;
    },
    async close() {
      ws.close();
      child.kill();
      await new Promise((resolve) => child.on('exit', resolve));
      rmSync(profile, { recursive: true, force: true });
    },
  };
}

export const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

export async function playFrames(page, { frames, fps = [30, 1], pacingMs, sample = 'window.sample()', shots = false }) {
  const [num, den] = fps;
  const states = [];
  const hashes = [];
  for (let frame = 0; frame < frames; frame++) {
    await page.evaluate(`__vrf.clock.set(${frame}, ${num}, ${den})`);
    states.push(await page.evaluate(sample));
    if (shots) hashes.push(await page.screenshotHash());
    await sleep(pacingMs);
  }
  return { states, hashes };
}

export async function playUnclocked(page, { frames, pacingMs, sample = 'window.sample()' }) {
  const states = [];
  const hashes = [];
  for (let frame = 0; frame < frames; frame++) {
    states.push(await page.evaluate(sample));
    hashes.push(await page.screenshotHash());
    await sleep(pacingMs);
  }
  return { states, hashes };
}
