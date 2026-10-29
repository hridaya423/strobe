import { after, before, describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { VERSIONS, bundleFixture, corpusDir, decodePng, launch, seek } from './harness.mjs';

const SLOW = {
  '/slow-font.woff2': ['Mona-Sans.woff2', 'font/woff2'],
  '/slow-image.png': ['NaBlack.png', 'image/png'],
};

const ASSETS = join(corpusDir('transitions-video'), 'public');

const slowAssets = () => (url, _req, res) => {
  const hit = SLOW[url.pathname];
  if (!hit) return false;
  setTimeout(() => {
    res.writeHead(200, { 'content-type': hit[1] });
    res.end(readFileSync(join(ASSETS, hit[0])));
  }, 200);
  return true;
};

describe('item_10_per_frame_barrier', () => {
  for (const version of VERSIONS) {
    describe(version, () => {
      let run;
      before(async () => {
        run = await launch(bundleFixture(version, 'barrier'), {
          width: 64,
          height: 64,
          init: { compositionId: 'Barrier' },
          onRequest: slowAssets(),
        });
      });
      after(() => run.close());

      test('a frame is never captured with delayRender handles open', async () => {
        const { page } = run;
        for (const frame of [1, 5, 2, 9]) {
          const t0 = Date.now();
          const result = await seek(page, frame);
          assert.ok(Date.now() - t0 >= 160, 'the barrier waited for the slowest handle');
          assert.equal(result.frame, frame);
          assert.equal(await page.eval(`document.querySelector('[data-frame]').dataset.frame`), String(frame));
          assert.equal(await page.eval(`document.querySelector('[data-frame]').dataset.done`), '3');
          assert.equal(await page.eval('window.remotion_renderReady'), true);
          assert.equal(await page.eval('Object.keys(window.remotion_delayRenderTimeouts).length'), 0);
          const shot = await decodePng(page, await page.screenshot({ width: 64, height: 64 }));
          assert.deepEqual(shot.at(32, 32), [0, 200, 0, 255]);
        }
      });

      test('the previous frame has cleared before setFrame runs', async () => {
        const { page } = run;
        await page.eval('window.__open.length = 0');
        for (const frame of [3, 4, 5]) await seek(page, frame);
        const open = await page.eval('window.__open');
        assert.deepEqual(open.map((o) => o.frame), [3, 4, 5]);
        assert.deepEqual(open.map((o) => o.open), [0, 0, 0]);
      });

      test('setFrame itself holds a handle until the next animation frame', async () => {
        const { page } = run;
        await seek(page, 6);
        const observed = await page.eval(`(async () => {
          window.remotion_setFrame(7, 'Barrier', 1);
          const heldAfterSet = window.remotion_renderReady;
          await __vrf.rm.ready();
          return { heldAfterSet, readyAfter: window.remotion_renderReady };
        })()`);
        assert.deepEqual(observed, { heldAfterSet: false, readyAfter: true });
      });

      test('one awaited call per frame', async () => {
        const { page } = run;
        let evaluates = 0;
        const original = page.send.bind(page);
        page.send = (method, params) => {
          if (method === 'Runtime.evaluate') evaluates += 1;
          return original(method, params);
        };
        await seek(page, 11);
        page.send = original;
        assert.equal(evaluates, 1);
      });

      test('the barrier holds when animation frames are flushed immediately', async () => {
        const other = await launch(bundleFixture(version, 'barrier'), { width: 64, height: 64 });
        try {
          await other.page.eval(`window.__raf = 0; window.requestAnimationFrame = (cb) => { window.__raf += 1; Promise.resolve().then(() => cb(performance.now())); return 1; }`);
          await other.page.eval(`__vrf.rm.init({ compositionId: 'Passive' })`);
          for (const frame of [4, 1, 9, 2, 15]) {
            await seek(other.page, frame);
            assert.equal(await other.page.eval(`document.querySelector('[data-n]').dataset.n`), '2', `frame ${frame} complete`);
            assert.equal(await other.page.eval(`document.querySelector('[data-n]').dataset.frame`), String(frame));
          }
          assert.ok((await other.page.eval('window.__raf')) > 0, 'the page used the replaced requestAnimationFrame');
        } finally {
          await other.close();
        }
      });

      for (const [composition, check] of [
        [
          'Fonts',
          async (page) => {
            assert.equal(await page.eval('document.fonts.status'), 'loaded');
            assert.deepEqual(await page.eval(`[...new Set([...document.fonts].filter((f) => f.family.includes('SlowFace')).map((f) => f.status))]`), ['loaded']);
          },
        ],
        [
          'Images',
          async (page) => {
            assert.deepEqual(await page.eval(`(() => { const i = document.querySelector('[data-img]'); return [i.complete, i.naturalWidth > 0]; })()`), [true, true]);
          },
        ],
      ]) {
        test(`${composition === 'Fonts' ? 'fonts requested' : 'images without a delayRender'} while rendering have loaded`, async () => {
          const other = await launch(bundleFixture(version, 'barrier'), { width: 200, height: 60, init: { compositionId: composition }, onRequest: slowAssets() });
          try {
            await seek(other.page, 1);
            await check(other.page);
          } finally {
            await other.close();
          }
        });
      }
    });
  }
});
