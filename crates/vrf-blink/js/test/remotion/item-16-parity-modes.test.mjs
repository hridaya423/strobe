import { describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { VERSIONS, bundleFixture, capture, decodeImage, launch, seek } from './harness.mjs';

const SIZE = { width: 320, height: 180 };
const FRAME = 41;

const shoot = async (version, captureMode) => {
  const run = await launch(bundleFixture(version, 'basic'), { ...SIZE, config: { captureMode }, init: { compositionId: 'Basic' } });
  try {
    await seek(run.page, FRAME);
    const shot = await capture(run.page, SIZE);
    const again = await capture(run.page, SIZE);
    const image = await decodeImage(run.page, shot);
    return { shot, again, image, spec: await run.page.eval('__vrf.rm.captureParams()') };
  } finally {
    await run.close();
  }
};

const quality = (jpeg) => {
  let at = 2;
  while (at < jpeg.length) {
    assert.equal(jpeg[at], 0xff);
    const marker = jpeg[at + 1];
    const length = jpeg.readUInt16BE(at + 2);
    if (marker === 0xdb) return jpeg[at + 5];
    at += 2 + length;
  }
  return null;
};

const error = (a, b) => {
  let sum = 0;
  let over32 = 0;
  for (let i = 0; i < a.data.length; i += 4) {
    let worst = 0;
    for (let c = 0; c < 3; c++) {
      const d = Math.abs(a.data[i + c] - b.data[i + c]);
      sum += d;
      if (d > worst) worst = d;
    }
    if (worst > 32) over32 += 1;
  }
  const pixels = a.data.length / 4;
  return { mean: sum / (pixels * 3), over32: over32 / pixels };
};

describe('item_16_parity_modes', () => {
  for (const version of VERSIONS) {
    describe(version, () => {
      test('raw mode is lossless and repeatable, jpeg mode is q80 and tracks it within its own tolerance', async () => {
        const raw = await shoot(version, 'raw');
        const jpeg = await shoot(version, 'jpeg');
        assert.deepEqual(raw.shot.bytes, raw.again.bytes);
        assert.equal(raw.shot.format, 'png');
        assert.equal(jpeg.shot.format, 'jpeg');
        assert.equal(jpeg.shot.bytes[0], 0xff);
        assert.equal(jpeg.shot.bytes[1], 0xd8);
        assert.equal(quality(jpeg.shot.bytes), 6, 'luminance quantisation entry for quality 80');
        assert.deepEqual(raw.image.at(5, 5).slice(0, 3), [FRAME * 2, 40, 255 - FRAME * 2]);
        const { mean, over32 } = error(raw.image, jpeg.image);
        assert.ok(mean > 0, 'jpeg degrades the frame');
        assert.ok(mean < 2, `mean error ${mean}`);
        assert.ok(over32 < 0.01, `pixels off by more than 32: ${over32}`);
      });

      test('png mode keeps alpha where raw and jpeg composite over black', async () => {
        const png = await shoot(version, 'png');
        assert.equal(png.shot.format, 'png');
        assert.deepEqual(png.spec.backgroundOverride, { r: 0, g: 0, b: 0, a: 0 });
        assert.equal(png.image.at(5, 5)[3], 255);
      });
    });
  }
});
