import { describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { VERSIONS, bundleFixture, capture, decodeImage, launch, seek } from './harness.mjs';

const SIZE = { width: 160, height: 90 };

const shoot = async (version, captureMode) => {
  const run = await launch(bundleFixture(version, 'basic'), { ...SIZE, config: { captureMode }, init: { compositionId: 'Transparent' } });
  try {
    await seek(run.page, 3);
    const body = await run.page.eval(`getComputedStyle(document.body).backgroundColor`);
    const image = await decodeImage(run.page, await capture(run.page, SIZE));
    return { body, image };
  } finally {
    await run.close();
  }
};

describe('item_12_background_rule', () => {
  for (const version of VERSIONS) {
    describe(version, () => {
      test('png capture renders on transparent, over the composition own body background', async () => {
        const { body, image } = await shoot(version, 'png');
        assert.equal(body, 'rgba(0, 0, 0, 0)');
        assert.equal(image.at(120, 45)[3], 0);
        const [r, g, b, a] = image.at(40, 45);
        assert.ok(r > 250 && g === 0 && b === 0 && a > 120 && a < 136, `half-red over nothing: ${[r, g, b, a]}`);
      });

      for (const mode of ['raw', 'jpeg']) {
        test(`${mode} capture renders on black`, async () => {
          const { body, image } = await shoot(version, mode);
          assert.equal(body, 'rgb(0, 0, 0)');
          const [r0, g0, b0, a0] = image.at(120, 45);
          assert.ok(r0 <= 3 && g0 <= 3 && b0 <= 3 && a0 === 255, `empty area is black: ${[r0, g0, b0, a0]}`);
          const [r, g, b] = image.at(40, 45);
          assert.ok(Math.abs(r - 128) <= 6 && g <= 6 && b <= 6, `half-red over black: ${[r, g, b]}`);
        });
      }
    });
  }

  test('capture parameters per mode', async () => {
    const params = {};
    for (const mode of ['raw', 'jpeg', 'png']) {
      const run = await launch(bundleFixture('4.0.240', 'basic'), { config: { captureMode: mode } });
      try {
        params[mode] = await run.page.eval('__vrf.rm.captureParams()');
      } finally {
        await run.close();
      }
    }
    assert.deepEqual(params, {
      raw: { format: 'raw', backgroundOverride: null },
      jpeg: { format: 'jpeg', quality: 80, optimizeForSpeed: true, backgroundOverride: null },
      png: { format: 'png', backgroundOverride: { r: 0, g: 0, b: 0, a: 0 } },
    });
  });
});
