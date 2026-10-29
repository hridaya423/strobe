import { describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { VERSIONS, bundleFixture, decodePng, launch, seek } from './harness.mjs';

const withClock = async (version, fixture, composition, fn, options = {}) => {
  const run = await launch(bundleFixture(version, fixture), { ...options, clock: true, init: { compositionId: composition } });
  try {
    await fn(run.page, run);
  } finally {
    await run.close();
  }
};

describe('clock_coupling', () => {
  for (const version of VERSIONS) {
    describe(version, () => {
      test('seek settles on the virtual rAF and renders each frame in order', () =>
        withClock(version, 'basic', 'Basic', async (page) => {
          for (const frame of [0, 1, 2, 7, 8, 30]) {
            const result = await seek(page, frame);
            assert.equal(result.frame, frame);
            assert.equal(await page.eval(`document.querySelector('[data-frame]').dataset.frame`), String(frame));
            assert.equal(await page.eval('window.remotion_renderReady'), true);
            assert.ok((await page.eval('performance.now()')) >= (frame * 1000) / 30);
          }
        }));

      test('the shot after a seek shows that frame', () =>
        withClock(version, 'basic', 'Basic', async (page) => {
          for (const frame of [3, 4, 50]) {
            await seek(page, frame);
            const shot = await decodePng(page, await page.screenshot({ width: 320, height: 180 }));
            assert.deepEqual(shot.at(300, 170).slice(0, 3), [frame * 2, 40, 255 - frame * 2]);
          }
        }));

      test('an earlier frame renders and virtual time stays where it was', () =>
        withClock(version, 'basic', 'Basic', async (page) => {
          await seek(page, 5);
          const before = await page.eval('performance.now()');
          await seek(page, 4);
          assert.equal(await page.eval(`document.querySelector('[data-frame]').dataset.frame`), '4');
          assert.equal(await page.eval('performance.now()'), before);
        }));
    });
  }

  for (const version of ['4.0.169', '4.0.240']) {
    test(`${version}: a delayRender timeout fires on virtual timers and consumes a retry`, async () => {
      const run = await launch(bundleFixture(version, 'barrier'), {
        width: 64,
        height: 64,
        clock: true,
        config: { timeoutInMilliseconds: 2500 },
        init: { compositionId: 'Hang' },
      });
      try {
        await seek(run.page, 2);
        await assert.rejects(seek(run.page, 3, { attempt: 1 }), /Retries left: 2 - Rendering the frame will be retried\./);
      } finally {
        await run.close();
      }
    });
  }
});
