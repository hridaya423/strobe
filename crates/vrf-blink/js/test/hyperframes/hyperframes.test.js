import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { after, before, describe, test } from 'node:test';
import { compileProject, composition, openHf, pngPixel, project } from './host.js';
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

const clipDiv = (id, attrs) => `<div id="${id}" class="clip" ${attrs} style="position:absolute;width:10px;height:10px;background:#fff"></div>`;
const idleTimeline = (end) => `const tl = gsap.timeline({paused:true}); tl.set({}, {}, ${end}); window.__timelines = {main: tl};`;

describe('item 20: clip windows', () => {
  test('item_20_overlapping_clips', async () => {
    const dir = project({
      'index.html': composition({
        body: clipDiv('a', 'data-start="0" data-duration="0.5" data-track-index="1"') + clipDiv('b', 'data-start="0.3" data-duration="0.5" data-track-index="1"'),
        script: idleTimeline(1),
      }),
    });
    const hf = await openHf(browser, dir);
    const state = async (frame) => {
      await hf.seek(frame);
      return [await shown(hf, '#a'), await shown(hf, '#b')];
    };
    assert.deepEqual(await state(0), [true, false]);
    assert.deepEqual(await state(2), [true, false]);
    assert.deepEqual(await state(4), [true, true]);
    assert.deepEqual(await state(6), [false, true]);
    assert.deepEqual(await state(9), [false, false]);
    await hf.close();
  });

  test('item_20_relative_start_references_with_offsets', async () => {
    const dir = project({
      'index.html': composition({
        duration: 2,
        body:
          clipDiv('a', 'data-start="0.5" data-duration="0.4"') +
          clipDiv('after', 'data-start="a + 0.2" data-duration="0.3"') +
          clipDiv('before', 'data-start="a - 0.2" data-duration="0.2"'),
        script: idleTimeline(2),
      }),
    });
    const hf = await openHf(browser, dir);
    const state = async (frame) => {
      await hf.seek(frame);
      return [await shown(hf, '#a'), await shown(hf, '#after'), await shown(hf, '#before')];
    };
    assert.deepEqual(await state(2), [false, false, false]);
    assert.deepEqual(await state(5), [true, false, false]);
    assert.deepEqual(await state(7), [true, false, true], 'a - 0.2 starts 0.2 s before a ends, at 0.7 s');
    assert.deepEqual(await state(9), [false, false, false]);
    assert.deepEqual(await state(10), [false, false, false]);
    assert.deepEqual(await state(11), [false, true, false], 'a + 0.2 starts 0.2 s after a ends, at 1.1 s');
    assert.deepEqual(await state(14), [false, false, false]);
    await hf.close();
  });

  test('item_20_track_index_does_not_affect_visibility', async () => {
    const make = (trackA, trackB) =>
      project({
        'index.html': composition({
          body: clipDiv('a', `data-start="0" data-duration="0.6" data-track-index="${trackA}"`) + clipDiv('b', `data-start="0.2" data-duration="0.6" data-track-index="${trackB}"`),
          script: idleTimeline(1),
        }),
      });
    const states = [];
    for (const dir of [make(1, 1), make(1, 9), make(7, 2)]) {
      const hf = await openHf(browser, dir);
      const row = [];
      for (const frame of [1, 3, 7, 9]) {
        await hf.seek(frame);
        row.push([await shown(hf, '#a'), await shown(hf, '#b')]);
      }
      states.push(row);
      await hf.close();
    }
    assert.deepEqual(states[1], states[0]);
    assert.deepEqual(states[2], states[0]);
  });

  const nested = (innerDuration) => {
    const dir = project({
      'index.html': composition({
        duration: 2,
        body: '<div id="host" data-composition-id="inner" data-composition-src="compositions/inner.html" data-start="0.2" data-duration="0.6" style="position:absolute;inset:0"></div>',
        script: idleTimeline(2),
      }),
      'compositions/inner.html': `<template id="inner-template"><div id="inner-root" data-composition-id="inner" data-width="320" data-height="180" data-duration="${innerDuration}">
        <div id="ib" style="position:absolute;left:0;top:60px;width:20px;height:20px;background:#0f0"></div>
        <script>window.__timelines = window.__timelines || {}; const t = gsap.timeline({paused:true}); t.to("#ib", {x: 100, duration: ${innerDuration}, ease:"none"}); window.__timelines["inner"] = t;</script>
      </div></template>`,
    });
    return dir;
  };

  test('item_20_nested_shorter_inner_timeline_holds_last_state', async () => {
    const { compiledDir } = await compileProject(nested(0.3), 10);
    const hf = await openHf(browser, compiledDir);
    await hf.seek(1);
    assert.equal(await shown(hf, '#host'), false);
    await hf.seek(2);
    assert.equal(await shown(hf, '#host'), true);
    await hf.seek(6);
    assert.equal(await hf.page.evaluate('new DOMMatrix(getComputedStyle(document.querySelector("#ib")).transform).m41'), 100);
    await hf.seek(8);
    assert.equal(await shown(hf, '#host'), false, 'host window of 0.6 s ends at 0.8 s');
    await hf.close();
  });

  test('item_20_nested_longer_inner_timeline_is_cut_at_the_host_window', async () => {
    const { compiledDir } = await compileProject(nested(1.5), 10);
    const hf = await openHf(browser, compiledDir);
    await hf.seek(5);
    const mid = await hf.page.evaluate('new DOMMatrix(getComputedStyle(document.querySelector("#ib")).transform).m41');
    assert.ok(Math.abs(mid - (100 * 0.3) / 1.5) < 0.5, `inner local time is host-relative (0.3 s of 1.5 s), got ${mid}`);
    await hf.seek(8);
    assert.equal(await shown(hf, '#host'), false);
    await hf.close();
  });
});

describe('item 21: virtual time', () => {
  const sampled = (extraScript = '') =>
    project({
      'index.html': composition({
        body: box,
        script: tween + 'window.sample = () => ({date: Date.now(), perf: performance.now(), newDate: new Date().getTime(), raf: window.lastRaf}); window.lastRaf = null; const loop = (ts) => { window.lastRaf = ts; requestAnimationFrame(loop); }; requestAnimationFrame(loop);' + extraScript,
      }),
    });

  test('item_21_virtual_time', async () => {
    const hf = await openHf(browser, sampled());
    for (const frame of [0, 1, 4, 9]) {
      await hf.seek(frame);
      const sample = await hf.page.evaluate('window.sample()');
      const expected = (frame / 10) * 1000;
      assert.deepEqual([sample.date, sample.perf, sample.newDate, sample.raf], [expected, expected, expected, expected], `frame ${frame}`);
    }
    await hf.close();
  });

  test('item_21_rational_fps_sets_the_clock_from_the_exact_ratio', async () => {
    const hf = await openHf(browser, sampled(), { init: { fps: [30000, 1001] } });
    await hf.seek(7);
    const sample = await hf.page.evaluate('window.sample()');
    assert.ok(Math.abs(sample.date - (7 * 1001 * 1000) / 30000) < 1e-9);
    assert.deepEqual(await hf.page.evaluate('__vrf.clock.calls.at(-1).slice(0, 3)'), [7, 30000, 1001]);
    await hf.close();
  });

  test('item_21_clock_is_set_once_per_seek_after_the_timeline_seek', async () => {
    const hf = await openHf(browser, sampled());
    await hf.page.evaluate(`(() => {
      window.trace = [];
      const original = window.__player.renderSeek;
      window.__player.renderSeek = (t, o) => { window.trace.push('seek'); return original.call(window.__player, t, o); };
      const set = window.__vrf.clock.set;
      window.__vrf.clock.set = (...args) => { window.trace.push('clock'); return set.apply(window.__vrf.clock, args); };
    })()`);
    await hf.seek(3);
    await hf.seek(4);
    assert.deepEqual(await hf.page.evaluate('window.trace'), ['seek', 'clock', 'seek', 'clock']);
    await hf.close();
  });

  test('item_21_math_random_stays_native', async () => {
    const hf = await openHf(browser, sampled());
    await hf.seek(2);
    assert.match(await hf.page.evaluate('Math.random.toString()'), /\[native code\]/);
    const a = await hf.page.evaluate('Math.random()');
    await hf.seek(2);
    assert.notEqual(await hf.page.evaluate('Math.random()'), a);
    await hf.close();
  });
});

describe('item 22: capture image path', () => {
  const captureFor = async (init) => {
    const hf = await openHf(browser, basic(), { init });
    const capture = hf.meta.capture;
    await hf.close();
    return capture;
  };

  test('item_22_capture_image_path', async () => {
    assert.deepEqual(await captureFor({ quality: 'standard' }), { format: 'jpeg', quality: 100, transparent: false });
    assert.deepEqual(await captureFor({ quality: 'high' }), { format: 'jpeg', quality: 100, transparent: false });
    assert.deepEqual(await captureFor({ quality: 'draft' }), { format: 'jpeg', quality: 80, transparent: false });
    assert.deepEqual(await captureFor({ alpha: true }), { format: 'png', quality: null, transparent: true });
    assert.deepEqual(await captureFor({ motionBlur: true }), { format: 'png', quality: null, transparent: false });
  });

  test('item_22_alpha_clears_page_backgrounds_for_the_driver', async () => {
    const dir = project({ 'index.html': composition({ body: box, script: tween }).replace('<style>', '<style>html{background:#fff}') });
    const hf = await openHf(browser, dir, { init: { alpha: true } });
    assert.equal(hf.meta.needsDefaultBackgroundOverride, true);
    assert.equal(await styleOf(hf, 'body', 'backgroundColor'), 'rgba(0, 0, 0, 0)');
    assert.equal(await hf.page.evaluate('getComputedStyle(document.documentElement).backgroundColor'), 'rgba(0, 0, 0, 0)');
    await hf.close();
  });

  test('item_22_opaque_capture_leaves_page_backgrounds_alone', async () => {
    const hf = await openHf(browser, basic());
    assert.equal(hf.meta.needsDefaultBackgroundOverride, false);
    assert.equal(await styleOf(hf, 'body', 'backgroundColor'), 'rgb(0, 0, 0)');
    await hf.close();
  });
});

describe('item 23: video pre-extraction page side', () => {
  const videoProject = () => {
    const dir = project({
      'index.html': composition({
        body: '<video id="v" src="clip.mp4" muted data-start="0.2" data-duration="0.5" data-media-start="1" data-playback-rate="2" style="position:absolute;left:10px;top:20px;width:160px;height:90px;opacity:0.5;object-fit:cover;border-radius:8px;filter:blur(1px)"></video>',
        script: idleTimeline(1),
      }),
    });
    execFileSync('ffmpeg', ['-v', 'error', '-f', 'lavfi', '-i', 'testsrc=size=160x90:rate=10:duration=2', '-pix_fmt', 'yuv420p', '-c:v', 'libx264', join(dir, 'clip.mp4')]);
    mkdirSync(join(dir, 'frames'));
    execFileSync('ffmpeg', ['-v', 'error', '-i', join(dir, 'clip.mp4'), '-q:v', '2', join(dir, 'frames/%05d.jpg')]);
    return dir;
  };
  const sibling = (hf, prop) =>
    hf.page.evaluate(`(() => { const img = document.querySelector('#v').nextElementSibling; return img && ${prop}; })()`);

  test('item_23_injected_image_copies_the_listed_visual_styles', async () => {
    const hf = await openHf(browser, videoProject(), { init: { skipVideoIds: ['v'] } });
    await hf.seek(3, { videos: [{ id: 'v', src: '/frames/00004.jpg' }] });
    assert.equal(await sibling(hf, 'img.classList.contains("__render_frame__")'), true);
    assert.equal(await sibling(hf, 'img.id'), '__render_frame_v__');
    assert.equal(await sibling(hf, 'img.complete && img.naturalWidth'), 160, 'frame is decoded when seek resolves');
    const style = await hf.page.evaluate(`(() => {
      const img = document.querySelector('#v').nextElementSibling; const s = getComputedStyle(img);
      return {fit: s.objectFit, radius: s.borderRadius, filter: s.filter, opacity: s.opacity, visibility: s.visibility,
        left: s.left, top: s.top, width: s.width, height: s.height, position: s.position,
        videoVisibility: getComputedStyle(document.querySelector('#v')).visibility};
    })()`);
    assert.deepEqual(style, { fit: 'cover', radius: '8px', filter: 'blur(1px)', opacity: '0.5', visibility: 'visible', left: '10px', top: '20px', width: '160px', height: '90px', position: 'absolute', videoVisibility: 'hidden' });
    await hf.close();
  });

  test('item_23_frame_swaps_only_when_the_source_changes', async () => {
    const hf = await openHf(browser, videoProject(), { init: { skipVideoIds: ['v'] } });
    await hf.seek(3, { videos: [{ id: 'v', src: '/frames/00004.jpg' }] });
    await hf.page.evaluate('window.__img = document.querySelector("#v").nextElementSibling; window.__loads = 0; window.__img.addEventListener("load", () => window.__loads++)');
    await hf.seek(4, { videos: [{ id: 'v', src: '/frames/00004.jpg' }] });
    await hf.seek(5, { videos: [{ id: 'v' }] });
    assert.equal(await hf.page.evaluate('window.__loads'), 0);
    await hf.seek(6, { videos: [{ id: 'v', src: '/frames/00007.jpg' }] });
    assert.match(await sibling(hf, 'img.getAttribute("src")'), /00007\.jpg$/);
    await hf.close();
  });

  test('item_23_video_outside_its_window_shows_no_frame', async () => {
    const hf = await openHf(browser, videoProject(), { init: { skipVideoIds: ['v'] } });
    await hf.seek(3, { videos: [{ id: 'v', src: '/frames/00004.jpg' }] });
    await hf.seek(8, { videos: [] });
    assert.equal(await sibling(hf, 'getComputedStyle(img).visibility'), 'hidden');
    assert.equal(await hf.page.evaluate('getComputedStyle(document.querySelector("#v")).visibility'), 'hidden');
    await hf.close();
  });

  test('item_23_video_descriptors_for_host_side_frame_choice', async () => {
    const hf = await openHf(browser, videoProject(), { init: { skipVideoIds: ['v'] } });
    const [video] = await hf.page.evaluate('__vrf.hf.videos()');
    assert.deepEqual(
      { id: video.id, src: video.src, start: video.start, duration: video.duration, mediaStart: video.mediaStart, playbackRate: video.playbackRate, loop: video.loop },
      { id: 'v', src: 'clip.mp4', start: '0.2', duration: '0.5', mediaStart: 1, playbackRate: 2, loop: false },
    );
    await hf.close();
  });

  test('item_23_vfr_sources_with_long_gaps', { skip: 'frame choice and ffmpeg extraction are host side (Task 5.1)' }, () => {});
});

describe('item 24: static dedup equivalence', () => {
  const staticProject = ({ body = box, extraScript = '', duration = 2, style = '' } = {}) =>
    project({
      'index.html': composition({
        duration,
        body,
        style,
        script: `const tl = gsap.timeline({paused:true}); tl.to("#box", {x: 100, duration: 0.3, ease:"none"}, 0); tl.to("#box", {y: 20, duration: 0.1, ease:"none"}, 1.5); window.__timelines = {main: tl};${extraScript}`,
      }),
    });
  const staticFrames = (hf) => hf.page.evaluate('__vrf.hf.staticFrames()');

  test('item_24_static_dedup_equivalence', async () => {
    const hf = await openHf(browser, staticProject());
    const result = await staticFrames(hf);
    assert.equal(result.eligible, true);
    assert.deepEqual(result.frames, [5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 18]);
    for (const frame of result.frames) {
      await hf.seek(frame - 1);
      const before = await hf.page.screenshot();
      await hf.seek(frame);
      assert.ok(before.equals(await hf.page.screenshot()), `frame ${frame} must equal frame ${frame - 1} byte for byte`);
    }
    await hf.close();
  });

  test('item_24_never_static_next_to_a_tween', async () => {
    const hf = await openHf(browser, staticProject());
    const { frames } = await staticFrames(hf);
    for (const tweenFrame of [0, 1, 2, 3, 4, 15, 16, 17]) assert.ok(!frames.includes(tweenFrame), `frame ${tweenFrame} borders a tween`);
    await hf.close();
  });

  test('item_24_clip_boundaries_plus_minus_one_are_animated', async () => {
    const hf = await openHf(
      browser,
      staticProject({ body: box + clipDiv('c', 'data-start="1" data-duration="0.5"'), duration: 3 }),
    );
    const { frames } = await staticFrames(hf);
    for (const edge of [10, 15]) for (const near of [edge - 1, edge, edge + 1]) assert.ok(!frames.includes(near), `frame ${near} is next to a clip edge`);
    assert.ok(frames.includes(12 + 1) && frames.includes(20));
    await hf.close();
  });

  const ineligible = (name, options, reason, init = {}) =>
    test(`item_24_ineligible_with_${name}`, async () => {
      const hf = await openHf(browser, staticProject(options), { init });
      const result = await staticFrames(hf);
      assert.equal(result.eligible, false);
      assert.deepEqual(result.frames, []);
      assert.match(result.reason, reason);
      await hf.close();
    });

  ineligible('canvas', { body: box + '<canvas width="10" height="10"></canvas>' }, /canvas/);
  ineligible('iframe', { body: box + '<iframe srcdoc="x" width="10" height="10"></iframe>' }, /iframe/);
  ineligible('video', { body: box + '<video id="v" data-start="0" data-duration="1"></video>' }, /video/, { skipVideoIds: ['v'] });
  ineligible('running_css_animation', { body: box + '<div style="width:5px;height:5px;animation:spin 1s linear infinite"></div>', style: '@keyframes spin{to{opacity:.5}}' }, /CSS\/WAAPI/);
  ineligible('timeline_call_side_effect', { extraScript: 'tl.call(() => {}, [], 0.5); tl.eventCallback("onComplete", () => {});' }, /tl\.call|no GSAP|side effect/);
  ineligible('reference_clip_start', { body: box + clipDiv('c', 'data-start="box + 0.2" data-duration="0.5"') }, /unresolvable clip start/);

  test('item_24_composition_without_tweens_is_ineligible', async () => {
    const dir = project({ 'index.html': composition({ body: box, script: 'const tl = gsap.timeline({paused:true}); window.__timelines = {main: tl};' }) });
    const hf = await openHf(browser, dir);
    const result = await staticFrames(hf);
    assert.equal(result.eligible, false);
    assert.match(result.reason, /no GSAP tweens/);
    await hf.close();
  });
});

describe('shared items 1 to 8', () => {
  test('item_01_output_geometry_rounds_up_to_even', async () => {
    const dir = project({ 'index.html': composition({ width: 321, height: 181, body: box, script: tween }) });
    const hf = await openHf(browser, dir);
    assert.deepEqual([hf.meta.width, hf.meta.height, hf.meta.outWidth, hf.meta.outHeight], [321, 181, 322, 182]);
    await hf.close();
    const even = await openHf(browser, basic());
    assert.deepEqual([even.meta.outWidth, even.meta.outHeight], [320, 180]);
    await even.close();
  });

  test('item_02_frame_count_and_timing', async () => {
    const framesFor = async (duration, init) => {
      const dir = project({ 'index.html': composition({ duration, body: box, script: `const tl = gsap.timeline({paused:true}); tl.to("#box", {x: 5, duration: ${duration}}); window.__timelines = {main: tl};` }) });
      const hf = await openHf(browser, dir, { init });
      const frames = hf.meta.frames;
      await hf.close();
      return frames;
    };
    assert.equal(await framesFor(1), 10);
    assert.equal(await framesFor(1.05), 11);
    assert.equal(await framesFor(1.00005), 10, 'within 1e-3 frames of an integer counts as that integer');
    assert.equal(await framesFor(1.004), 11);
    assert.equal(await framesFor(1, { fps: [30000, 1001] }), 30);
    assert.equal(await framesFor(10.01, { fps: [30000, 1001] }), 300);
  });

  test('item_02_frame_i_is_i_over_fps_exactly_for_rational_rates', async () => {
    const hf = await openHf(browser, basic(), { init: { fps: [30000, 1001] } });
    await spyRenderSeek(hf);
    for (const i of [0, 1, 7, 29, 100]) await hf.seek(i);
    const times = await seeks(hf);
    [0, 1, 7, 29, 100].forEach((i, k) => assert.ok(Math.abs(times[k] - (i * 1001) / 30000) < 1e-12, `frame ${i}: ${times[k]}`));
    await hf.close();
  });

  test('item_03_sparse_rendering_matches_the_full_render', async () => {
    const full = await openHf(browser, basic());
    const reference = new Map();
    for (let i = 0; i < 10; i++) {
      await full.seek(i);
      reference.set(i, await full.page.screenshot());
    }
    await full.close();
    const sparse = await openHf(browser, basic());
    for (const i of [7, 2, 9, 0, 5]) {
      await sparse.seek(i);
      assert.ok((await sparse.page.screenshot()).equals(reference.get(i)), `frame ${i} rendered out of order equals the sequential render`);
    }
    await sparse.close();
  });

  test('item_04_colour_pipeline_end_state', { skip: 'encoder side: yuv420p, CRF 18 and BT.709 tags are Task 2.x/3.7' }, () => {});
  test('item_05_audio_mix_model', { skip: 'Task 5.2' }, () => {});
  test('item_06_video_frame_choice', { skip: 'host side lookup, Task 5.1; the page applies the chosen frames (item 23)' }, () => {});
  test('item_07_video_colour_conversion', { skip: 'host side decode, Task 5.1' }, () => {});

  const veil = () =>
    project({
      'index.html': composition({
        body: '<div id="veil" style="position:absolute;inset:0;background:rgba(255,0,0,0.5)"></div>',
        script: idleTimeline(1),
        style: 'html,body,#root{background:transparent}',
      }),
    });

  test('item_08_opaque_output_keeps_the_browser_default_background_like_the_reference', async () => {
    const hf = await openHf(browser, veil());
    assert.equal(hf.meta.needsDefaultBackgroundOverride, false);
    await hf.seek(0);
    const [r, g, b] = pngPixel(await hf.page.screenshot(), 10, 10);
    assert.ok(r === 255 && Math.abs(g - 128) <= 1 && Math.abs(b - 128) <= 1, `got ${[r, g, b]}`);
    await hf.close();
  });

  test('item_08_alpha_output_keeps_the_overlay_translucent', async () => {
    const hf = await openHf(browser, veil(), { init: { alpha: true } });
    assert.equal(hf.meta.needsDefaultBackgroundOverride, true);
    await hf.page.send('Emulation.setDefaultBackgroundColorOverride', { color: { r: 0, g: 0, b: 0, a: 0 } });
    await hf.seek(0);
    const [r, g, b, a] = pngPixel(await hf.page.screenshot(), 10, 10);
    assert.ok(r >= 254 && g <= 1 && b <= 1 && Math.abs(a - 128) <= 1, `got ${[r, g, b, a]}`);
    await hf.close();
  });
});
