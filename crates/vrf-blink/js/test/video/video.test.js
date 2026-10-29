const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

globalThis.__vrf_video_send = () => {};
(0, eval)(fs.readFileSync(path.join(__dirname, '../../video.js'), 'utf8'));
const { chooseFrame, swsRgb, swsConfig } = globalThis.__vrf.video;

test('latest frame at or before the target, earliest if none precedes', () => {
  const vfr = [0, 33, 66, 166, 183, 216, 249, 349];
  assert.equal(chooseFrame(vfr, 0), 0);
  assert.equal(chooseFrame(vfr, 32), 0);
  assert.equal(chooseFrame(vfr, 33), 1);
  assert.equal(chooseFrame(vfr, 100), 2);
  assert.equal(chooseFrame(vfr, 165), 2);
  assert.equal(chooseFrame(vfr, 166), 3);
  assert.equal(chooseFrame(vfr, 10_000), vfr.length - 1);
  const offset = [23040, 23552, 24064];
  assert.equal(chooseFrame(offset, 0), 0);
  assert.equal(chooseFrame(offset, 23551), 0);
  assert.equal(chooseFrame(offset, 23552), 1);
  assert.equal(chooseFrame(offset, -5), 0);
});

test('duplicate timestamps pick the later frame', () => {
  assert.equal(chooseFrame([0, 10, 10, 20], 10), 2);
  assert.equal(chooseFrame([0, 10, 10, 20], 15), 2);
});

test('container ticks are round(time * timescale), as the compositor computes them', () => {
  const pts = Array.from({ length: 120 }, (_, i) => i * 512);
  for (let frame = 0; frame < 120; frame++) {
    const time = frame / 30;
    assert.equal(chooseFrame(pts, Math.round(time * 15360)), frame, `frame ${frame}`);
  }
});

const oracle = JSON.parse(fs.readFileSync(path.join(__dirname, 'swscale-oracle.json'), 'utf8'));
const matrixOf = (tag) => [tag.replace('f', ''), tag.endsWith('f')];

for (const [tag, samples] of Object.entries(oracle)) {
  test(`the integer model reproduces Remotion's libswscale for ${tag}`, () => {
    const [matrix, full] = matrixOf(tag);
    const config = swsConfig(matrix, full);
    for (const [y, u, v, r, g, b] of samples) assert.deepEqual(swsRgb(config, y, u, v), [r, g, b], `YUV ${y},${u},${v}`);
  });
}
