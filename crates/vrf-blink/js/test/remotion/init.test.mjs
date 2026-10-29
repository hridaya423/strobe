import { describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { VERSIONS, bundleFixture, launch, seek } from './harness.mjs';

const INPUT = { w: 640, h: 360, d: 50, fps: 60, label: 'from-input' };

const withPage = async (version, fn) => {
  const run = await launch(bundleFixture(version, 'basic'), { config: { inputProps: JSON.stringify(INPUT) } });
  try {
    await fn(run.page);
  } finally {
    await run.close();
  }
};

const summary = (meta) => ({ width: meta.width, height: meta.height, fps: meta.fps, durationInFrames: meta.durationInFrames });

describe('init_selects_composition', () => {
  for (const version of VERSIONS) {
    describe(version, () => {
      test('static metadata', () =>
        withPage(version, async (page) => {
          const meta = await page.eval(`__vrf.rm.init({ compositionId: 'Basic' })`);
          assert.deepEqual(summary(meta), { width: 320, height: 180, fps: 30, durationInFrames: 90 });
        }));

      test('calculateMetadata sees the input props', () =>
        withPage(version, async (page) => {
          const meta = await page.eval(`__vrf.rm.init({ compositionId: 'Meta' })`);
          assert.deepEqual(summary(meta), { width: 640, height: 360, fps: 60, durationInFrames: 50 });
        }));

      test('input props reach the component', () =>
        withPage(version, async (page) => {
          await page.eval(`__vrf.rm.init({ compositionId: 'Props' })`);
          await seek(page, 0);
          assert.equal(await page.eval(`document.querySelector('[data-props]').textContent`), 'from-input');
          assert.equal(JSON.parse(await page.eval(`document.querySelector('[data-props]').dataset.props`)).label, 'from-input');
        }));

      test('an unknown composition is an error', () =>
        withPage(version, async (page) => {
          await assert.rejects(page.eval(`__vrf.rm.init({ compositionId: 'Nope' })`), /Nope/);
        }));

      test('a second init on the same page is refused', () =>
        withPage(version, async (page) => {
          await page.eval(`__vrf.rm.init({ compositionId: 'Basic' })`);
          await assert.rejects(page.eval(`__vrf.rm.init({ compositionId: 'Props' })`), /once per page/);
        }));
    });
  }
});
