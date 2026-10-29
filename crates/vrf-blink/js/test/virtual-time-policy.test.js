import assert from 'node:assert/strict';
import { after, before, describe, test } from 'node:test';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launch, serve, sleep } from './harness.js';

const fixtures = join(dirname(fileURLToPath(import.meta.url)), 'fixtures');
const frameMs = 1000 / 30;

let browser;
let site;
let page;

before(async () => {
  site = await serve(fixtures);
  browser = await launch();
  page = await browser.newPage({ clock: false });
  await page.goto(`${site.origin}/anim.html`);
  await page.evaluate(`
    window.rafs = [];
    window.ticks = [];
    requestAnimationFrame(function frame(t) { rafs.push([t, performance.now(), document.timeline.currentTime]); requestAnimationFrame(frame); });
    setInterval(() => ticks.push(performance.now()), 10);
    undefined;
  `);
});

after(async () => {
  await browser.close();
  site.close();
});

const read = () =>
  page.evaluate(`({ now: performance.now(), date: Date.now(), timeline: document.timeline.currentTime, rafs: rafs.length, ticks: ticks.length, slide: sample().slide })`);

async function advance(budget) {
  const expired = page.once('Emulation.virtualTimeBudgetExpired');
  await page.send('Emulation.setVirtualTimePolicy', { policy: 'advance', budget });
  await expired;
}

describe('Emulation.setVirtualTimePolicy in chrome-headless-shell 156 (no injected clock)', () => {
  test('pause freezes Date.now, performance.now and timers against wall time', async () => {
    await page.send('Emulation.setVirtualTimePolicy', { policy: 'pause', initialVirtualTime: 1000 });
    const first = await read();
    await sleep(300);
    const second = await read();
    assert.equal(first.date, 1000000);
    assert.equal(second.date, first.date);
    assert.equal(second.now, first.now);
    assert.equal(second.ticks, first.ticks);
  });

  test('a budget advances Date.now and timers, but performance.now is quantized to 0.1 ms and starts at a nonzero offset', async () => {
    const start = await read();
    assert.ok(start.now > 0);
    const deltas = [];
    let previous = start;
    for (let i = 0; i < 30; i++) {
      await advance(frameMs);
      const next = await read();
      deltas.push(next.now - previous.now);
      previous = next;
    }
    assert.ok(Math.abs(previous.now - start.now - 1000) < 0.2);
    assert.ok(deltas.some((d) => Math.abs(d - frameMs) > 0.02), 'every step was exactly frame/fps*1000');
    assert.ok(deltas.every((d) => Math.abs(d - frameMs) < 0.2));
    assert.ok(previous.ticks - start.ticks >= 90);
  });

  test('rAF timestamps, document.timeline and CSS animations keep following the wall clock while virtual time is paused', async () => {
    await page.send('Emulation.setVirtualTimePolicy', { policy: 'pause' });
    const before = await read();
    await sleep(300);
    const after = await read();
    assert.equal(after.now, before.now);
    assert.ok(after.rafs - before.rafs >= 5);
    assert.ok(after.timeline - before.timeline > 200);
    assert.ok(after.slide !== before.slide);
  });
});
