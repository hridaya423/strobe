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

const spyRenderSeek = (hf) =>
  hf.page.evaluate(`(() => {
    window.__seeks = [];
    const original = window.__player.renderSeek;
    window.__player.renderSeek = (t, o) => { window.__seeks.push(t); return original.call(window.__player, t, o); };
  })()`);
const seeks = (hf) => hf.page.evaluate('window.__seeks');
const shown = (hf, selector) =>
  hf.page.evaluate(`(() => { const s = getComputedStyle(document.querySelector(${JSON.stringify(selector)})); return s.visibility !== 'hidden' && s.display !== 'none'; })()`);

describe('item 18: time quantization', () => {
  test('item_18_time_quantization', async () => {
    const hf = await openHf(browser, basic());
    await spyRenderSeek(hf);
    for (let i = 0; i < 10; i++) await hf.seek(i);
    assert.deepEqual(await seeks(hf), Array.from({ length: 10 }, (_, i) => i / 10));
    await hf.close();
  });

  test('item_18_mid_frame_time_floors_to_frame', async () => {
    const hf = await openHf(browser, basic());
    await spyRenderSeek(hf);
    for (let i = 0; i < 9; i++) await hf.page.evaluate(`__vrf.hf.seekTime(${(i + 0.5) / 10})`);
    assert.deepEqual(await seeks(hf), Array.from({ length: 9 }, (_, i) => i / 10));
    await hf.close();
  });

  test('item_18_float_noise_snaps_to_frame', async () => {
    const hf = await openHf(browser, basic());
    await spyRenderSeek(hf);
    for (const t of [0.3 - 1e-12, 0.3 + 1e-12, 0.7 - 1e-10]) await hf.page.evaluate(`__vrf.hf.seekTime(${t})`);
    assert.deepEqual(await seeks(hf), [0.3, 0.3, 0.7]);
    await hf.close();
  });

  test('item_18_sub_frame_divisions_round_on_the_finer_grid', async () => {
    const hf = await openHf(browser, basic());
    await spyRenderSeek(hf);
    await hf.page.evaluate('__vrf.hf.seekTime(0.131, {subFrameDivisions: 4})');
    assert.deepEqual(await seeks(hf), [Math.round(0.131 * 40) / 40]);
    await hf.close();
  });

  test('item_18_clip_window_boundary_epsilon', async () => {
    const clip = (start) =>
      project({
        'index.html': composition({
          fps: 30,
          duration: 2,
          body: `<div id="c" class="clip" data-start="${start}" data-duration="1" style="position:absolute;width:10px;height:10px;background:#fff"></div>`,
          script: 'const tl = gsap.timeline({paused:true}); tl.set({}, {}, 2); window.__timelines = {main: tl};',
        }),
      });
    const snapped = await openHf(browser, clip('0.33336'));
    await snapped.seek(10);
    assert.equal(await shown(snapped, '#c'), true, 'start within 1e-3 frames of frame 10 snaps onto it');
    await snapped.close();
    const unsnapped = await openHf(browser, clip('0.3337'));
    await unsnapped.seek(10);
    assert.equal(await shown(unsnapped, '#c'), false, 'start 0.011 frames late stays hidden at frame 10');
    await unsnapped.seek(11);
    assert.equal(await shown(unsnapped, '#c'), true);
    await unsnapped.close();
  });
});

const styleOf = (hf, selector, prop) =>
  hf.page.evaluate(`getComputedStyle(document.querySelector(${JSON.stringify(selector)})).${prop}`);

describe('item 19: seek semantics', () => {
  test('item_19_from_tween_applies_start_state_at_frame_zero', async () => {
    const dir = project({
      'index.html': composition({
        body: box,
        script: 'const tl = gsap.timeline({paused:true}); tl.from("#box", {opacity: 0, x: 100, duration: 1, ease:"none"}); window.__timelines = {main: tl};',
      }),
    });
    const hf = await openHf(browser, dir);
    await hf.seek(0);
    assert.equal(await styleOf(hf, '#box', 'opacity'), '0');
    assert.equal(await boxX(hf), 100);
    await hf.seek(5);
    assert.equal(await boxX(hf), 50);
    await hf.close();
  });

  test('item_19_callbacks_fire_once_when_the_playhead_crosses_them_forward', async () => {
    const dir = project({
      'index.html': composition({
        body: box,
        script:
          'window.hits = 0; const tl = gsap.timeline({paused:true}); tl.to("#box", {x: 200, duration: 1, ease:"none"}); tl.call(() => { window.hits++; }, [], 0.5); window.__timelines = {main: tl};',
      }),
    });
    const hf = await openHf(browser, dir);
    const hits = () => hf.page.evaluate('window.hits');
    for (let i = 0; i < 5; i++) await hf.seek(i);
    assert.equal(await hits(), 0);
    await hf.seek(5);
    assert.equal(await hits(), 1);
    for (let i = 6; i < 10; i++) await hf.seek(i);
    assert.equal(await hits(), 1);
    await hf.close();
  });

  test('item_19_callback_fires_once_when_a_seek_jumps_over_it', async () => {
    const dir = project({
      'index.html': composition({
        body: box,
        script:
          'window.hits = 0; const tl = gsap.timeline({paused:true}); tl.call(() => { window.hits++; }, [], 0.5); tl.set({}, {}, 1); window.__timelines = {main: tl};',
      }),
    });
    const hf = await openHf(browser, dir);
    await hf.seek(2);
    await hf.seek(9);
    assert.equal(await hf.page.evaluate('window.hits'), 1);
    await hf.close();
  });

  test('item_19_holds_last_frame_beyond_timeline_end', async () => {
    const dir = project({
      'index.html': composition({
        duration: 2,
        body: box,
        script: 'const tl = gsap.timeline({paused:true}); tl.to("#box", {x: 200, duration: 1, ease:"none"}); window.__timelines = {main: tl};',
      }),
    });
    const hf = await openHf(browser, dir);
    assert.equal(hf.meta.frames, 20);
    await hf.seek(10);
    assert.equal(await boxX(hf), 200);
    await hf.seek(19);
    assert.equal(await boxX(hf), 200);
    await hf.close();
  });

  test('item_19_sibling_timelines_seek_before_the_master', async () => {
    const dir = project({
      'index.html': composition({
        body:
          box +
          '<div id="side" data-composition-id="side" data-start="0" data-duration="1" style="position:absolute;width:10px;height:10px"></div>',
        script: `window.order = [];
          const side = gsap.timeline({paused:true, onUpdate: () => window.order.push('side')}); side.to("#side", {x: 50, duration: 1, ease:"none"});
          const main = gsap.timeline({paused:true, onUpdate: () => window.order.push('main')}); main.to("#box", {x: 200, duration: 1, ease:"none"});
          window.__timelines = {side, main};`,
      }),
    });
    const hf = await openHf(browser, dir);
    await hf.page.evaluate('window.order.length = 0');
    await hf.seek(5);
    const order = await hf.page.evaluate('window.order');
    assert.ok(order.includes('side') && order.includes('main'));
    assert.ok(order.indexOf('side') < order.indexOf('main'), `order was ${order}`);
    await hf.close();
  });

  test('item_19_css_animation_adapter_seeks_in_the_same_pass', async () => {
    const dir = project({
      'index.html': composition({
        body: '<div id="pulse" style="position:absolute;width:20px;height:20px;background:#fff;animation:fade 1s linear both"></div>',
        style: '@keyframes fade{from{opacity:0}to{opacity:1}}',
        script: 'const tl = gsap.timeline({paused:true}); tl.set({}, {}, 1); window.__timelines = {main: tl};',
      }),
    });
    const hf = await openHf(browser, dir);
    await hf.seek(5);
    assert.ok(Math.abs(Number(await styleOf(hf, '#pulse', 'opacity')) - 0.5) < 0.01);
    await hf.seek(2);
    assert.ok(Math.abs(Number(await styleOf(hf, '#pulse', 'opacity')) - 0.2) < 0.01);
    await hf.close();
  });

  test('item_19_waapi_adapter_seeks_in_the_same_pass', async () => {
    const dir = project({
      'index.html': composition({
        body: '<div id="slide" style="position:absolute;width:20px;height:20px;background:#fff"></div>',
        script: `document.querySelector('#slide').animate([{opacity: 0}, {opacity: 1}], {duration: 1000, fill: 'both', easing: 'linear'});
          const tl = gsap.timeline({paused:true}); tl.set({}, {}, 1); window.__timelines = {main: tl};`,
      }),
    });
    const hf = await openHf(browser, dir);
    await hf.seek(5);
    assert.ok(Math.abs(Number(await styleOf(hf, '#slide', 'opacity')) - 0.5) < 0.01);
    await hf.seek(8);
    assert.ok(Math.abs(Number(await styleOf(hf, '#slide', 'opacity')) - 0.8) < 0.01);
    await hf.close();
  });

  test('item_19_random_access_equals_sequential_playback', async () => {
    const hf = await openHf(browser, basic());
    for (let i = 0; i <= 7; i++) await hf.seek(i);
    const sequential = await boxX(hf);
    await hf.seek(0);
    await hf.seek(7);
    assert.equal(await boxX(hf), sequential);
    await hf.close();
  });
});
