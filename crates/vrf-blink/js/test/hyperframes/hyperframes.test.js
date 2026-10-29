import assert from 'node:assert/strict';
import { after, before, describe, test } from 'node:test';
import { composition, openHf, project, withBrowser } from './host.js';
import { launch } from './cdp.js';

let browser;
before(async () => {
  browser = await launch();
});
after(async () => {
  await browser.close();
});

const tween = 'const tl = gsap.timeline({paused: true}); tl.to("#box", {x: 200, duration: 1, ease: "none"}); window.__timelines = {main: tl};';
const box = '<div id="box" style="position:absolute;left:0;top:60px;width:60px;height:60px;background:#f84"></div>';
const basic = (extra = {}) => project({ 'index.html': composition({ body: box, script: tween, ...extra }) });

const boxX = (hf) => hf.page.evaluate('new DOMMatrix(getComputedStyle(document.querySelector("#box")).transform).m41');

describe('item 17: entry contract', () => {
  test('item_17_entry_contract', async () => {
    const hf = await openHf(browser, basic());
    assert.equal(await hf.page.evaluate('typeof window.__hf.seek'), 'function');
    assert.ok((await hf.page.evaluate('window.__hf.duration')) > 0);
    assert.deepEqual(
      { width: hf.meta.width, height: hf.meta.height, fps: hf.meta.fps, frames: hf.meta.frames, duration: hf.meta.duration },
      { width: 320, height: 180, fps: [10, 1], frames: 10, duration: 1 },
    );
    await hf.close();
  });

  test('item_17_missing_timeline_fails_with_zero_duration_diagnostic', async () => {
    const dir = project({
      'index.html': composition({ duration: 0, body: box }).replace('data-duration="0"', ''),
    });
    const hf = await openHf(browser, dir, { init: false });
    await assert.rejects(
      hf.page.evaluate('__vrf.hf.init({timeoutMs: 1200})'),
      (error) => /Composition has zero duration/.test(error.message) && /Runtime ready:/.test(error.message),
    );
    await hf.close();
  });

  test('item_17_data_no_timeline_renders', async () => {
    const dir = project({ 'index.html': composition({ body: box, attrs: 'data-no-timeline' }) });
    const hf = await openHf(browser, dir);
    assert.equal(hf.meta.frames, 10);
    await hf.seek(3);
    await hf.close();
  });

  test('item_17_readiness_gates_once_before_frame_zero', async () => {
    const dir = project({
      'index.html': composition({
        body: box + '<div data-composition-id="late" data-start="0" data-duration="1"></div>',
        script: tween + 'setTimeout(() => { const t = gsap.timeline({paused: true}); window.__timelines.late = t; }, 400);',
      }),
    });
    const hf = await openHf(browser, dir, { init: false });
    const before = Date.now();
    await hf.page.evaluate('__vrf.hf.init({})');
    assert.ok(Date.now() - before >= 300, 'init waits for the late sub-composition timeline');
    assert.equal(await hf.page.evaluate('Boolean(window.__timelines.late)'), true);
    await hf.close();
  });
});
