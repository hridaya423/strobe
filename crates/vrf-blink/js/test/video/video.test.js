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

const frameBuffer = (id, k) => {
  const buffer = new ArrayBuffer(32 + 4 * 2 + 2 * 1 * 2);
  const head = new DataView(buffer);
  head.setUint32(0, 1, true);
  head.setUint32(4, id, true);
  head.setUint32(8, k, true);
  head.setUint32(12, 4, true);
  head.setUint32(16, 2, true);
  return buffer;
};

const metaBuffer = (req, id, count) => {
  const json = new TextEncoder().encode(JSON.stringify({ req, eligible: true, width: 4, height: 2, timescale: 1000, matrix: '709', full: false }));
  const at = 32 + ((json.length + 7) & ~7);
  const buffer = new ArrayBuffer(at + count * 8);
  const head = new DataView(buffer);
  head.setUint32(0, 2, true);
  head.setUint32(4, id, true);
  head.setUint32(8, count, true);
  head.setUint32(12, json.length, true);
  new Uint8Array(buffer, 32, json.length).set(json);
  const pts = new Float64Array(buffer, at, count);
  for (let i = 0; i < count; i++) pts[i] = i * 1000;
  return buffer;
};

const fakeGl = new Proxy({}, { get: (_, key) => (/^[A-Z0-9_]+$/.test(key) ? 0 : () => true) });
const fakeCanvas = () => ({ getContext: () => fakeGl, remove() {}, style: {}, width: 1, height: 1 });

const withVideoPage = async (body) => {
  const saved = { send: globalThis.__vrf_video_send, document: globalThis.document, offscreen: globalThis.OffscreenCanvas };
  const images = [];
  globalThis.document = { images, createElement: fakeCanvas };
  globalThis.OffscreenCanvas = class {
    getContext() {}
    convertToBlob() {
      return Promise.resolve(new Blob([new Uint8Array([1, 2, 3])], { type: 'image/png' }));
    }
  };
  let nextClip = 7;
  globalThis.__vrf_video_send = (json) => {
    const message = JSON.parse(json);
    if (message.t === 'open') globalThis.__vrf_video_push(metaBuffer(message.req, nextClip++, 200));
    if (message.t === 'want' && !message.hit) globalThis.__vrf_video_push(frameBuffer(message.clip, message.k));
  };
  try {
    await body(images);
  } finally {
    globalThis.__vrf_video_send = saved.send;
    globalThis.document = saved.document;
    globalThis.OffscreenCanvas = saved.offscreen;
  }
};

const fetchUrl = async (src, time) => {
  const blob = await (await globalThis.__vrf.video.request(src, time)).blob();
  return URL.createObjectURL(blob);
};

const showImage = (images, src) => {
  const img = { src, className: '', style: { cssText: '' }, parentNode: { insertBefore() {} } };
  images.splice(0, images.length, img);
};

test('frames shown once and replaced are not kept for the life of the page', async () => {
  await withVideoPage(async (images) => {
    const video = globalThis.__vrf.video;
    for (let t = 0; t < 40; t++) {
      showImage(images, await fetchUrl('clip-a.mp4', t));
      assert.equal(video.present(), 1);
    }
    assert.equal(video.sources.size, 1);
  });
});

test('a requested frame stays resolvable until it has been presented, even if its URL is revoked first', async () => {
  await withVideoPage(async (images) => {
    const video = globalThis.__vrf.video;
    const url = await fetchUrl('clip-b.mp4', 3);
    URL.revokeObjectURL(url);
    assert.equal(video.sources.has(url), true);
    showImage(images, url);
    assert.equal(video.present(), 1);
    assert.equal(video.sources.has(url), true);
    images.length = 0;
    video.present();
    assert.equal(video.sources.has(url), false);
  });
});

test('revoking the URL of a frame that was presented and left the page forgets it', async () => {
  await withVideoPage(async (images) => {
    const video = globalThis.__vrf.video;
    const url = await fetchUrl('clip-c.mp4', 5);
    showImage(images, url);
    video.present();
    images.length = 0;
    URL.revokeObjectURL(url);
    assert.equal(video.sources.has(url), false);
  });
});

test('frames requested but never shown are dropped after a few presents', async () => {
  await withVideoPage(async () => {
    const video = globalThis.__vrf.video;
    const url = await fetchUrl('clip-d.mp4', 9);
    for (let i = 0; i < 12; i++) video.present();
    assert.equal(video.sources.has(url), false);
  });
});

test('the decoded frame cache of a clip stays bounded when frames are requested one after another', async () => {
  await withVideoPage(async () => {
    const video = globalThis.__vrf.video;
    for (let t = 0; t < 60; t++) await video.request('clip-e.mp4', t);
    const clip = [...video.clips.values()].at(-1);
    assert.ok(clip.frames.size <= 15, `${clip.frames.size}`);
  });
});
