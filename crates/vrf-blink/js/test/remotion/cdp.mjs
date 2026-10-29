import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const HERE = new URL('.', import.meta.url).pathname;
const REPO = join(HERE, '../../../../..');

export function headlessShell() {
  return (
    process.env.VRF_HEADLESS_SHELL ??
    join(
      process.env.VRF_BENCH_DIR ?? join(REPO, 'bench'),
      'engines/browsers/chrome-headless-shell/mac_arm-156.0.8078.73/chrome-headless-shell-mac-arm64/chrome-headless-shell',
    )
  );
}

const FLAGS = [
  '--force-color-profile=srgb',
  '--font-render-hinting=none',
  '--hide-scrollbars',
  '--no-sandbox',
  '--mute-audio',
  '--disable-background-timer-throttling',
  '--disable-backgrounding-occluded-windows',
  '--disable-renderer-backgrounding',
  '--autoplay-policy=no-user-gesture-required',
  '--remote-debugging-port=0',
];

export class Browser {
  static async launch() {
    const dir = mkdtempSync(join(tmpdir(), 'vrf-rm-'));
    const proc = spawn(headlessShell(), [...FLAGS, `--user-data-dir=${dir}`], { stdio: ['ignore', 'ignore', 'pipe'] });
    const url = await new Promise((resolve, reject) => {
      let buf = '';
      proc.stderr.on('data', (d) => {
        buf += d;
        const m = buf.match(/DevTools listening on (ws:\/\/\S+)/);
        if (m) resolve(m[1]);
      });
      proc.on('exit', (code) => reject(new Error(`browser exited ${code}: ${buf}`)));
    });
    const ws = new WebSocket(url);
    await new Promise((resolve, reject) => {
      ws.onopen = resolve;
      ws.onerror = reject;
    });
    return new Browser(proc, ws, dir);
  }

  constructor(proc, ws, dir) {
    this.proc = proc;
    this.ws = ws;
    this.dir = dir;
    this.nextId = 1;
    this.pending = new Map();
    this.listeners = new Set();
    ws.onmessage = (e) => {
      const msg = JSON.parse(e.data);
      if (msg.id) {
        const p = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        if (msg.error) p.reject(new Error(`${p.method}: ${msg.error.message}`));
        else p.resolve(msg.result);
      } else {
        for (const l of this.listeners) l(msg);
      }
    };
  }

  send(method, params = {}, sessionId) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject, method });
      this.ws.send(JSON.stringify({ id, method, params, sessionId }));
    });
  }

  async newPage({ width, height } = {}) {
    const { targetId } = await this.send('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await this.send('Target.attachToTarget', { targetId, flatten: true });
    const page = new Page(this, sessionId, targetId);
    await page.send('Page.enable');
    await page.send('Runtime.enable');
    if (width) await page.setViewport(width, height);
    return page;
  }

  async close() {
    this.ws.close();
    this.proc.kill();
    await new Promise((resolve) => this.proc.once('exit', resolve));
    rmSync(this.dir, { recursive: true, force: true });
  }
}

export class Page {
  constructor(browser, sessionId, targetId) {
    this.browser = browser;
    this.sessionId = sessionId;
    this.targetId = targetId;
    this.console = [];
    this.exceptions = [];
    this.dialogs = [];
    browser.listeners.add((msg) => {
      if (msg.sessionId !== sessionId) return;
      if (msg.method === 'Page.javascriptDialogOpening') {
        this.dialogs.push(msg.params.message);
        this.send('Page.handleJavaScriptDialog', { accept: false });
      } else if (msg.method === 'Runtime.consoleAPICalled') {
        this.console.push(msg.params.args.map((a) => a.value ?? a.description).join(' '));
      } else if (msg.method === 'Runtime.exceptionThrown') {
        this.exceptions.push(msg.params.exceptionDetails.exception?.description ?? msg.params.exceptionDetails.text);
      }
    });
  }

  send(method, params) {
    return this.browser.send(method, params, this.sessionId);
  }

  setViewport(width, height) {
    return this.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });
  }

  addScript(source) {
    return this.send('Page.addScriptToEvaluateOnNewDocument', { source });
  }

  async goto(url) {
    const loaded = new Promise((resolve) => {
      const l = (msg) => {
        if (msg.sessionId === this.sessionId && msg.method === 'Page.loadEventFired') {
          this.browser.listeners.delete(l);
          resolve();
        }
      };
      this.browser.listeners.add(l);
    });
    await this.send('Page.navigate', { url });
    await loaded;
  }

  async eval(expression) {
    const r = await this.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) {
      throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
    }
    return r.result.value;
  }

  async screenshot({ format = 'png', quality, width, height } = {}) {
    const params = { format, captureBeyondViewport: false, fromSurface: true };
    if (quality) params.quality = quality;
    if (width) params.clip = { x: 0, y: 0, width, height, scale: 1 };
    const { data } = await this.send('Page.captureScreenshot', params);
    return Buffer.from(data, 'base64');
  }
}
