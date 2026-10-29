const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

globalThis.__vrf_video_send = () => {};
(0, eval)(fs.readFileSync(path.join(__dirname, '../../video.js'), 'utf8'));
const { chooseFrame, swsRgb, swsConfig } = globalThis.__vrf.video;

const VFR = [0, 33, 66, 166, 183, 216, 249, 348, 366, 399, 432, 532, 549, 582];
const CFR = Array.from({ length: 120 }, (_, i) => i * 512);

test("the nearest frame wins, as in Remotion's frame server (probed on vfr1080.mp4)", () => {
  const remotion = [
    [0, 0], [32, 1], [33, 1], [65, 2], [66, 2], [100, 2], [165, 3], [166, 3], [200, 5], [215, 5], [216, 5],
    [248, 6], [249, 6], [250, 6], [300, 7], [333, 7], [348, 7], [349, 7], [400, 9],
  ];
  for (const [ms, index] of remotion) assert.equal(chooseFrame(VFR, ms), index, `${ms} ms`);
});

test("ties and unrounded targets follow Remotion on a constant frame rate (probed on cfr1080.mp4)", () => {
  const ts = 15360;
  const remotion = [
    [0.016, 0], [0.0166, 0], [0.016666666666666666, 1], [0.01667, 1], [0.0168, 1],
    [0.05, 2], [0.0499, 1], [0.050000001, 2], [0.083333333333333, 2], [0.1, 3],
  ];
  for (const [seconds, index] of remotion) assert.equal(chooseFrame(CFR, seconds * ts), index, `${seconds} s`);
});

test('targets are truncated to whole ticks and switch at the midpoint between two frames (probed on every gap near 4.5 s)', () => {
  const gap = [4425, 4458, 4558, 4575, 4608, 4641, 4741, 4758, 4791];
  for (const [tick, index] of [[4441, 0], [4442, 1], [4566, 2], [4567, 3], [4591, 3], [4592, 4], [4749, 6], [4750, 7]]) {
    assert.equal(chooseFrame(gap, tick), index, `${tick}`);
  }
  assert.equal(chooseFrame(gap, 4566.666666666666), 2);
  assert.equal(chooseFrame(gap, 4566.999), 2);
});

test('before the first frame the earliest is shown, after the last the last', () => {
  assert.equal(chooseFrame([23040, 23552, 24064], 0), 0);
  assert.equal(chooseFrame([23040, 23552, 24064], -5), 0);
  assert.equal(chooseFrame(VFR, 10_000), VFR.length - 1);
});

test('every output frame of a constant frame rate clip shows its own frame', () => {
  for (let frame = 0; frame < 120; frame++) assert.equal(chooseFrame(CFR, (frame / 30) * 15360), frame, `frame ${frame}`);
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
