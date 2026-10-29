import assert from 'node:assert/strict';
import { after, before, describe, test } from 'node:test';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gsapPath, launch, playFrames, playUnclocked, serve, sleep } from './harness.js';

const fixtures = join(dirname(fileURLToPath(import.meta.url)), 'fixtures');
const pacings = [1, 50];

let browser;
let site;

before(async () => {
  site = await serve(fixtures, { '/gsap.min.js': gsapPath });
  browser = await launch();
});

after(async () => {
  await browser.close();
  site.close();
});

async function open(path, options) {
  const page = await browser.newPage(options);
  await page.goto(site.origin + path);
  return page;
}

async function withPage(path, body, options) {
  const page = await open(path, options);
  try {
    return await body(page);
  } finally {
    await page.close();
  }
}

const exactMs = (frame, num, den) => Number((BigInt(frame) * 1000n * BigInt(den) * 10n ** 12n) / BigInt(num)) / 1e12;

describe('Date.now', () => {
  test('a page printing Date.now per frame shows frame/fps*1000 at 1 ms and 50 ms pacing', async () => {
    const frames = 30;
    const runs = [];
    for (const pacingMs of pacings) {
      runs.push(
        await withPage('/early.html', async (page) => {
          await playFrames(page, { frames, pacingMs });
          return page.evaluate('window.sample()');
        }),
      );
    }
    const expectedDateNow = Array.from({ length: frames }, (_, f) => Math.floor((f * 1000) / 30));
    for (const log of runs) {
      assert.deepEqual(log.dateNow, expectedDateNow);
      assert.deepEqual(log.ctor, expectedDateNow);
      assert.deepEqual(log.raf, log.perfNow);
      assert.deepEqual(log.raf, Array.from({ length: frames }, (_, f) => (f * 1000) / 30));
    }
    assert.deepEqual(runs[0], runs[1]);
  });

  test('without the clock the same page differs between pacings', async () => {
    const runs = [];
    for (const pacingMs of pacings) {
      runs.push(
        await withPage(
          '/early.html',
          async (page) => {
            await sleep(30 * pacingMs);
            return (await page.evaluate('window.sample()')).dateNow.length;
          },
          { clock: false },
        ),
      );
    }
    assert.notEqual(runs[0], runs[1]);
  });

  test('values captured before any page work keep following the virtual clock', async () => {
    await withPage('/early.html', async (page) => {
      await page.evaluate('__vrf.clock.set(0, 30, 1)');
      await page.evaluate('__vrf.clock.set(1, 30, 1)');
      await sleep(150);
      const log = await page.evaluate('window.sample()');
      assert.deepEqual(log.dateNow, [0, 33]);
      assert.deepEqual(log.timeout, []);
      await page.evaluate('__vrf.clock.set(3000, 30, 1)');
      assert.deepEqual((await page.evaluate('window.sample()')).timeout, [100]);
    });
  });

  test('new Date(), Date(), subclassing and static members behave', async () => {
    await withPage('/blank.html', async (page) => {
      await page.evaluate('__vrf.clock.set(5, 30, 1)');
      const out = await page.evaluate(`({
        now: Date.now(),
        ctor: new Date().getTime(),
        call: Date().slice(0, 15),
        explicit: new Date(2020, 0, 1).getFullYear(),
        utc: Date.UTC(2000, 0, 1),
        parse: Date.parse('2000-01-01T00:00:00Z'),
        instanceOf: new Date() instanceof Date,
        ctorIdentity: Date.prototype.constructor === Date && new Date().constructor === Date,
        tag: Object.prototype.toString.call(new Date()),
        sub: new (class Sub extends Date {})().getTime(),
        nameLength: [Date.name, Date.length],
      })`);
      assert.deepEqual(out, {
        now: 166,
        ctor: 166,
        call: 'Thu Jan 01 1970',
        explicit: 2020,
        utc: 946684800000,
        parse: 946684800000,
        instanceOf: true,
        ctorIdentity: true,
        tag: '[object Date]',
        sub: 166,
        nameLength: ['Date', 7],
      });
    });
  });

  test('epochMs offsets Date.now and performance.timeOrigin together', async () => {
    await withPage('/blank.html', async (page) => {
      const out = await page.evaluate(`(() => {
        __vrf.clock.epochMs = 1.7e12;
        const r = [];
        for (const f of [0, 1, 7, 100]) {
          __vrf.clock.set(f, 24, 1);
          r.push([Date.now(), performance.timeOrigin, performance.now()]);
        }
        return r;
      })()`);
      [0, 1, 7, 100].forEach((f, i) => {
        const [now, origin, perf] = out[i];
        assert.equal(origin, 1.7e12);
        assert.equal(perf, (f * 1000) / 24);
        assert.equal(now, Math.floor(origin + perf));
      });
    });
  });
});

describe('time source', () => {
  test('frame/fps*1000 is exact for rational fps such as 30000/1001', async () => {
    await withPage('/blank.html', async (page) => {
      const frames = Array.from({ length: 301 }, (_, i) => i);
      const out = await page.evaluate(`(() => {
        const r = [];
        for (let f = 0; f <= 300; f++) {
          __vrf.clock.set(f, 30000, 1001);
          r.push([performance.now(), Date.now(), document.timeline.currentTime]);
        }
        __vrf.clock.set(30000, 30000, 1001);
        r.push([performance.now(), Date.now(), document.timeline.currentTime]);
        return r;
      })()`);
      for (const f of frames) {
        const [perf, date, timeline] = out[f];
        assert.ok(Math.abs(perf - exactMs(f, 30000, 1001)) < 1e-9, `frame ${f}`);
        assert.equal(date, Math.floor(perf));
        assert.equal(timeline, perf);
      }
      assert.equal(out[3][0], 100.1);
      assert.deepEqual(out[301], [1001000, 1001000, 1001000]);
    });
  });

  test('performance.now, timeOrigin and Date.now stay consistent and ignore wall time', async () => {
    await withPage('/blank.html', async (page) => {
      await page.evaluate('__vrf.clock.set(5, 30, 1)');
      const before = await page.evaluate('[performance.now(), Date.now(), performance.timeOrigin]');
      await sleep(120);
      const after = await page.evaluate('[performance.now(), Date.now(), performance.timeOrigin]');
      assert.deepEqual(before, after);
      assert.equal(before[0], 5000 / 30);
      assert.equal(Math.floor(before[2] + before[0]), before[1]);
    });
  });

  test('setMs takes a raw time and time never goes backwards', async () => {
    await withPage('/blank.html', async (page) => {
      await page.evaluate('__vrf.clock.setMs(100.5)');
      assert.equal(await page.evaluate('performance.now()'), 100.5);
      await page.evaluate('__vrf.clock.setMs(100.5)');
      const err = await page.evaluate(`(() => {
        try { __vrf.clock.set(2, 30, 1); } catch (e) { return e.name; }
      })()`);
      assert.equal(err, 'RangeError');
      assert.equal(await page.evaluate('performance.now()'), 100.5);
    });
  });
});

describe('requestAnimationFrame', () => {
  test('fires once per advanced frame with the virtual timestamp', async () => {
    await withPage('/blank.html', async (page) => {
      await page.evaluate(`
        window.log = [];
        requestAnimationFrame((t) => log.push(['a', t, performance.now(), document.timeline.currentTime]));
        requestAnimationFrame((t) => {
          log.push(['b', t]);
          requestAnimationFrame((t2) => log.push(['nested', t2]));
        });
        cancelAnimationFrame(requestAnimationFrame(() => log.push(['cancelled'])));
        undefined;
      `);
      await sleep(100);
      assert.deepEqual(await page.evaluate('log'), []);
      await page.evaluate('__vrf.clock.set(0, 30, 1)');
      assert.deepEqual(await page.evaluate('log'), [
        ['a', 0, 0, 0],
        ['b', 0],
      ]);
      await sleep(100);
      assert.equal(await page.evaluate('log.length'), 2);
      await page.evaluate('__vrf.clock.set(1, 30, 1)');
      assert.deepEqual((await page.evaluate('log')).slice(2), [['nested', 1000 / 30]]);
      await page.evaluate('__vrf.clock.set(2, 30, 1)');
      assert.equal(await page.evaluate('log.length'), 3);
    });
  });

  test('a callback can cancel a later one in the same frame and a throwing callback does not stop the rest', async () => {
    await withPage('/blank.html', async (page) => {
      const out = await page.evaluate(`(() => {
        const log = [];
        const errors = [];
        window.addEventListener('error', (e) => errors.push(e.message));
        let later;
        requestAnimationFrame(() => { log.push('first'); cancelAnimationFrame(later); });
        later = requestAnimationFrame(() => log.push('cancelled'));
        requestAnimationFrame(() => { throw new Error('boom'); });
        requestAnimationFrame(() => log.push('last'));
        __vrf.clock.set(0, 30, 1);
        return { log, errors };
      })()`);
      assert.deepEqual(out.log, ['first', 'last']);
      assert.equal(out.errors.length, 1);
      assert.match(out.errors[0], /boom/);
    });
  });

  test('registering again at the same time runs on the next set of the same time', async () => {
    await withPage('/blank.html', async (page) => {
      const out = await page.evaluate(`(() => {
        __vrf.clock.set(4, 30, 1);
        const log = [];
        requestAnimationFrame((t) => log.push(t));
        __vrf.clock.set(4, 30, 1);
        requestAnimationFrame((t) => log.push(t));
        __vrf.clock.set(4, 30, 1);
        return log;
      })()`);
      assert.deepEqual(out, [4000 / 30, 4000 / 30]);
    });
  });
});

describe('timers', () => {
  test('fire in due-time order with the clock at their due time, ties in creation order', async () => {
    await withPage('/blank.html', async (page) => {
      const log = await page.evaluate(`(() => {
        const log = [];
        const note = (name) => log.push([name, performance.now(), Date.now()]);
        setTimeout(() => note('t100'), 100);
        setTimeout(() => note('t70a'), 70);
        setTimeout(() => note('t70b'), 70);
        setTimeout(() => note('t50'), 50);
        let n = 0;
        const iv = setInterval(() => { note('i30'); if (++n === 4) clearInterval(iv); }, 30);
        const doomed = setTimeout(() => note('doomed'), 20);
        clearTimeout(doomed);
        setTimeout((a, b) => log.push(['args', a, b]), 10, 'x', 'y');
        for (let f = 0; f <= 6; f++) __vrf.clock.set(f, 30, 1);
        return log;
      })()`);
      assert.deepEqual(log, [
        ['args', 'x', 'y'],
        ['i30', 30, 30],
        ['t50', 50, 50],
        ['i30', 60, 60],
        ['t70a', 70, 70],
        ['t70b', 70, 70],
        ['i30', 90, 90],
        ['t100', 100, 100],
        ['i30', 120, 120],
      ]);
    });
  });

  test('wall time alone fires nothing, a set fires what is due', async () => {
    await withPage('/blank.html', async (page) => {
      await page.evaluate('window.fired = []; setTimeout(() => fired.push(performance.now()), 10); undefined');
      await sleep(150);
      assert.deepEqual(await page.evaluate('fired'), []);
      await page.evaluate('__vrf.clock.set(1, 30, 1)');
      assert.deepEqual(await page.evaluate('fired'), [10]);
    });
  });

  test('setTimeout 0 chains are clamped to 4 ms after five nested levels', async () => {
    await withPage('/blank.html', async (page) => {
      await page.evaluate('window.n = 0; (function r() { n++; setTimeout(r, 0); })(); undefined');
      await page.evaluate('__vrf.clock.set(0, 30, 1)');
      assert.equal(await page.evaluate('n'), 6);
      await page.evaluate('__vrf.clock.set(1, 30, 1)');
      assert.equal(await page.evaluate('n'), 14);
    });
  });

  test('ids are positive integers, clear* accepts junk, a throwing timer does not stop the rest', async () => {
    await withPage('/blank.html', async (page) => {
      const out = await page.evaluate(`(() => {
        const errors = [];
        window.addEventListener('error', (e) => errors.push(e.message));
        const log = [];
        const a = setTimeout(() => { throw new Error('boom'); }, 5);
        const b = setInterval(() => {}, 5);
        clearInterval(b);
        clearTimeout(undefined);
        clearTimeout(123456);
        setTimeout(() => log.push('after'), 6);
        __vrf.clock.set(1, 30, 1);
        return { ids: [Number.isInteger(a) && a > 0, b > a], log, errors: errors.length };
      })()`);
      assert.deepEqual(out, { ids: [true, true], log: ['after'], errors: 1 });
    });
  });
});

describe('animations', () => {
  const frames = 24;
  const fps = [12, 1];

  function expectedState(f) {
    const ms = (f * 1000) / fps[0];
    const fade = Math.min(1, Math.max(0, (f - 3) / 12));
    return { slide: ((ms % 2000) / 2000) * 240, fade, fadeX: 200 * fade, webTime: ms, bar: (ms % 1000) * 0.3 };
  }

  test('CSS animation, CSS transition, WAAPI and rAF-driven style are exact and independent of pacing', async () => {
    const runs = [];
    for (const pacingMs of pacings) {
      runs.push(await withPage('/anim.html', (page) => playFrames(page, { frames, fps, pacingMs, shots: true })));
    }
    for (const { states } of runs) {
      states.forEach((s, f) => {
        const want = expectedState(f);
        assert.ok(Math.abs(s.slide - want.slide) < 1e-3, `slide frame ${f}: ${s.slide} vs ${want.slide}`);
        assert.ok(Math.abs(s.fade - want.fade) < 1e-6, `fade frame ${f}: ${s.fade} vs ${want.fade}`);
        assert.ok(Math.abs(s.fadeX - want.fadeX) < 1e-3, `fadeX frame ${f}: ${s.fadeX} vs ${want.fadeX}`);
        assert.ok(Math.abs(s.webTime - want.webTime) < 1e-6, `webTime frame ${f}: ${s.webTime}`);
        assert.ok(Math.abs(parseFloat(s.bar) - want.bar) < 1e-3, `bar frame ${f}: ${s.bar}`);
      });
      assert.ok(Math.abs(states[9].web - 120) < 1e-3);
    }
    assert.deepEqual(runs[0].states, runs[1].states);
    assert.deepEqual(runs[0].hashes, runs[1].hashes);
    assert.equal(new Set(runs[0].hashes).size, frames);
  });

  test('without the clock the same page renders differently at different pacings', async () => {
    const runs = [];
    for (const pacingMs of pacings) {
      runs.push(await withPage('/anim.html', (page) => playUnclocked(page, { frames: 8, pacingMs }), { clock: false }));
    }
    assert.notDeepEqual(runs[0].hashes, runs[1].hashes);
  });

  test('WAAPI: currentTime, finished, playbackRate and page-initiated pause/play', async () => {
    await withPage('/blank.html', async (page) => {
      await page.evaluate(`
        window.el = document.body.appendChild(document.createElement('div'));
        el.style.cssText = 'width:10px;height:10px';
        window.anim = el.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 1000, fill: 'forwards' });
        window.fast = el.animate([{ marginLeft: '0px' }, { marginLeft: '100px' }], { duration: 1000, fill: 'forwards' });
        fast.playbackRate = 2;
        window.finishedAt = null;
        anim.finished.then(() => { finishedAt = performance.now(); });
        window.read = () => [anim.currentTime, Number(getComputedStyle(el).opacity), fast.currentTime, anim.playState];
        undefined;
      `);
      const at = async (f) => {
        await page.evaluate(`__vrf.clock.set(${f}, 30, 1)`);
        return page.evaluate('read()');
      };
      const near = (got, want) => got.forEach((v, i) => assert.ok(typeof v === 'string' || Math.abs(v - want[i]) < 1e-6, `${got} vs ${want}`));

      near(await at(0), [0, 0, 0, '']);
      near(await at(14), [14000 / 30, 14 / 30, 1000 * (14 / 15), '']);
      near(await at(15), [500, 0.5, 1000, '']);
      assert.equal(await page.evaluate('fast.playState'), 'finished');
      assert.equal(await page.evaluate('anim.pause(), 1'), 1);
      near(await at(20), [500, 0.5, 1000, '']);
      assert.equal(await page.evaluate('anim.playState'), 'paused');
      await page.evaluate('anim.play()');
      near(await at(25), [500 + 5000 / 30, 0.5 + 5 / 30, 1000, '']);
      for (let f = 26; f < 35; f++) await at(f);
      assert.equal(await page.evaluate('finishedAt'), null);
      await at(35);
      assert.equal(await page.evaluate('finishedAt'), 35000 / 30);
      assert.equal(await page.evaluate('anim.playState'), 'finished');
      near(await at(40), [1000, 1, 1000, '']);
    });
  });

  test('a CSS animation started after the clock was already running starts at its creation frame', async () => {
    await withPage('/blank.html', async (page) => {
      await page.evaluate(`
        document.head.insertAdjacentHTML('beforeend', '<style>@keyframes grow { from { width: 0px } to { width: 100px } } #g { height: 5px; animation: grow 1000ms linear forwards }</style>');
        undefined;
      `);
      await page.evaluate('__vrf.clock.set(30, 30, 1)');
      await page.evaluate('requestAnimationFrame(() => { const g = document.body.appendChild(document.createElement("div")); g.id = "g"; getComputedStyle(g).width; }); undefined');
      const widths = [];
      for (const f of [31, 45, 60, 75]) {
        await page.evaluate(`__vrf.clock.set(${f}, 30, 1)`);
        widths.push(await page.evaluate('getComputedStyle(document.getElementById("g")).width'));
      }
      [0, 14, 29, 44].forEach((elapsed, i) => {
        const want = Math.min(100, (elapsed / 30) * 100);
        assert.ok(Math.abs(parseFloat(widths[i]) - want) <= 1 / 64, `${widths[i]} vs ${want}`);
      });
      assert.equal(widths[0], '0px');
    });
  });
});

describe('GSAP', () => {
  const frames = 41;
  const fps = [24, 1];
  const power2InOut = (p) => (p < 0.5 ? 4 * p ** 3 : 1 - (-2 * p + 2) ** 3 / 2);

  test('a free-running GSAP page renders the same state and pixels at frame N regardless of pacing', async () => {
    const runs = [];
    for (const pacingMs of pacings) {
      runs.push(await withPage('/gsap.html', (page) => playFrames(page, { frames, fps, pacingMs, shots: true })));
    }
    assert.deepEqual(runs[0].states, runs[1].states);
    assert.deepEqual(runs[0].hashes, runs[1].hashes);
    runs[0].states.forEach((s, f) => {
      const ms = Math.floor((f * 1000) / 24);
      const want = power2InOut(Math.min(1, ms / 1000)) * 240;
      assert.ok(Math.abs(s.a[0] - want) < 1, `frame ${f}: ${s.a[0]} vs ${want}`);
      assert.equal(s.aBackground, f >= 12 ? 'rgb(0, 136, 0)' : 'rgb(204, 51, 0)');
    });
    assert.equal(new Set(runs[0].hashes).size, frames);
  });

  test('without the clock the GSAP page differs between pacings', async () => {
    const runs = [];
    for (const pacingMs of pacings) {
      runs.push(await withPage('/gsap.html', (page) => playUnclocked(page, { frames: 8, pacingMs }), { clock: false }));
    }
    assert.notDeepEqual(runs[0].hashes, runs[1].hashes);
  });
});

describe('iframes', () => {
  test('same-origin iframes follow the parent clock, including ones added later', async () => {
    await withPage('/frames.html', async (page) => {
      for (let f = 0; f < 4; f++) await page.evaluate(`__vrf.clock.set(${f}, 30, 1)`);
      await page.evaluate('addDynamic()');
      for (let f = 4; f < 8; f++) await page.evaluate(`__vrf.clock.set(${f}, 30, 1)`);
      const out = await page.evaluate(`({
        static: document.getElementById('static').contentWindow.sample(),
        inline: document.getElementById('inline').contentWindow.log,
        dynamic: dynamic.contentWindow.sample(),
        dynamicNow: dynamic.contentWindow.performance.now(),
      })`);
      const want = (f) => [Math.floor((f * 1000) / 30), (f * 1000) / 30];
      assert.deepEqual(out.static, [0, 1, 2, 3, 4, 5, 6, 7].map(want));
      assert.deepEqual(out.inline, [0, 1, 2, 3, 4, 5, 6, 7].map((f) => want(f)[0]));
      assert.deepEqual(out.dynamic, [4, 5, 6, 7].map(want));
      assert.equal(out.dynamicNow, 7000 / 30);
    });
  });

  test('an empty iframe created by script already has the virtual clock', async () => {
    await withPage('/frames.html', async (page) => {
      await page.evaluate('__vrf.clock.set(6, 30, 1)');
      const out = await page.evaluate(`(() => {
        const frame = document.body.appendChild(document.createElement('iframe'));
        return [frame.contentWindow.Date.now(), frame.contentWindow.performance.now()];
      })()`);
      assert.deepEqual(out, [200, 200]);
    });
  });
});

describe('requestVideoFrameCallback', () => {
  test('fires with the virtual time and the sought media time, not on wall time', async () => {
    await withPage('/blank.html', async (page) => {
      await page.evaluate(`
        window.video = document.body.appendChild(document.createElement('video'));
        video.muted = true;
        window.log = [];
        window.arm = () => video.requestVideoFrameCallback((now, meta) => log.push([now, meta.mediaTime, meta.presentationTime, meta.width]));
        fetch('/video.webm').then((r) => r.blob()).then((blob) => new Promise((resolve) => {
          video.addEventListener('loadeddata', resolve, { once: true });
          video.src = URL.createObjectURL(blob);
        }));
      `);
      await page.evaluate('arm(); undefined');
      await sleep(100);
      assert.deepEqual(await page.evaluate('log'), []);
      await page.evaluate('__vrf.clock.set(3, 30, 1)');
      assert.deepEqual(await page.evaluate('log'), [[100, 0, 100, 64]]);
      await page.evaluate('arm(); __vrf.clock.set(4, 30, 1)');
      assert.equal(await page.evaluate('log.length'), 1);
      await page.evaluate('video.currentTime = 0.5; new Promise((r) => video.addEventListener("seeked", r, { once: true }))');
      await page.evaluate('__vrf.clock.set(5, 30, 1)');
      const log = await page.evaluate('log');
      assert.equal(log.length, 2);
      assert.equal(log[1][0], 5000 / 30);
      assert.ok(Math.abs(log[1][1] - 0.5) < 0.11);
    });
  });
});

describe('not covered', () => {
  test('dedicated workers see the virtual clock', { todo: 'workers have their own realm; the driver would need OnWorkerContextCreated plus shared memory for a synchronous time' }, async () => {
    await withPage('/blank.html', async (page) => {
      await page.evaluate('__vrf.clock.set(6, 30, 1)');
      const seen = await page.evaluate(`new Promise((resolve) => {
        const url = URL.createObjectURL(new Blob(['postMessage(Date.now())']));
        new Worker(url).onmessage = (e) => resolve(e.data);
      })`);
      assert.equal(seen, 200);
    });
  });
});
