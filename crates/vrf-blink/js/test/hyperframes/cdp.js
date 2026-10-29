import { spawn, execFileSync } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));

const mainCheckout = dirname(
  execFileSync('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: here, encoding: 'utf8' }).trim(),
);
export const benchDir = process.env.VRF_BENCH_DIR ?? join(mainCheckout, 'bench');

function findChrome() {
  if (process.env.CHROME_HEADLESS_SHELL) return process.env.CHROME_HEADLESS_SHELL;
  const root = join(benchDir, 'engines/browsers/chrome-headless-shell');
  const version = readdirSync(root).filter((d) => d.startsWith('mac_') || d.startsWith('linux') || d.startsWith('win')).sort().at(-1);
  const platformDir = readdirSync(join(root, version)).find((d) => d.startsWith('chrome-headless-shell-'));
  return join(root, version, platformDir, 'chrome-headless-shell');
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
    this.errors = [];
    connection.on(sessionId, 'Runtime.exceptionThrown', ({ exceptionDetails }) => {
      this.errors.push(exceptionDetails.exception?.description ?? exceptionDetails.text);
    });
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

  async screenshot({ clip } = {}) {
    const { data } = await this.send('Page.captureScreenshot', {
      format: 'png',
      fromSurface: true,
      captureBeyondViewport: false,
      ...(clip ? { clip: { x: 0, y: 0, ...clip, scale: 1 } } : {}),
    });
    return Buffer.from(data, 'base64');
  }
}

export async function launch({ gpu = false, windowSize } = {}) {
  const profile = mkdtempSync(join(tmpdir(), 'vrf-hf-'));
  const child = spawn(
    findChrome(),
    [
      '--remote-debugging-port=0',
      `--user-data-dir=${profile}`,
      '--no-first-run',
      '--hide-scrollbars',
      '--force-device-scale-factor=1',
      '--force-color-profile=srgb',
      '--font-render-hinting=none',
      '--autoplay-policy=no-user-gesture-required',
      ...(gpu === 'metal' ? ['--enable-webgl', '--ignore-gpu-blocklist', '--use-gl=angle', '--use-angle=metal', '--enable-gpu-rasterization'] : gpu ? [] : ['--disable-gpu']),
      ...(windowSize ? [`--window-size=${windowSize[0]},${windowSize[1]}`] : []),
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
    async newPage({ width = 320, height = 180, scripts = [], emulate = true } = {}) {
      const { targetId } = await connection.send('Target.createTarget', { url: 'about:blank' });
      const { sessionId } = await connection.send('Target.attachToTarget', { targetId, flatten: true });
      const page = new Page(connection, sessionId, targetId);
      await page.send('Page.enable');
      await page.send('Runtime.enable');
      if (emulate) await page.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });
      for (const source of scripts) await page.send('Page.addScriptToEvaluateOnNewDocument', { source });
      return page;
    },
    async closePage(page) {
      await connection.send('Target.closeTarget', { targetId: page.targetId });
    },
    async close() {
      ws.close();
      child.kill();
      await new Promise((resolve) => child.on('exit', resolve));
      rmSync(profile, { recursive: true, force: true });
    },
  };
}
