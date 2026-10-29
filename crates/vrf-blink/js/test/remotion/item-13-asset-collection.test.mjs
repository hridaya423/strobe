import { describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { VERSIONS, bundleFixture, launch, seek } from './harness.mjs';

const SAMPLE_RATE = 48000;

const collectAll = async (version, frames) => {
  const run = await launch(bundleFixture(version, 'assets'), { width: 64, height: 64, init: { compositionId: 'Offset' } });
  try {
    const perFrame = [];
    for (let frame = 0; frame < frames; frame++) perFrame.push((await seek(run.page, frame)).assets);
    await new Promise((resolve) => setTimeout(resolve, 50));
    const leftover = await run.page.eval('window.remotion_collectAssets()');
    return { perFrame, leftover, meta: run.meta };
  } finally {
    await run.close();
  }
};

describe('item_13_asset_collection', () => {
  for (const version of VERSIONS) {
    describe(version, () => {
      let collected;

      test('a Sequence-offset clip starts at the right sample with per-frame volume and rate', async () => {
        collected = await collectAll(version, 90);
        const { perFrame, meta } = collected;
        const offset = perFrame.flat().filter((a) => a.src === 'http://assets.invalid/offset.mp3');
        assert.equal(offset.length, 60);
        const first = offset[0];
        assert.equal(first.frame, 30);
        assert.equal(first.mediaFrame, 0);
        assert.equal((first.frame / meta.fps) * SAMPLE_RATE, 48000);
        assert.ok(offset.every((a) => a.playbackRate === 2 && a.type === 'audio'));
        const volumeAt = (frame) => offset.find((a) => a.frame === frame).volume;
        assert.ok(Math.abs(volumeAt(45) - 0.4) < 1e-9);
        assert.ok(Math.abs(volumeAt(46) - 0.41) < 1e-9);
        assert.deepEqual(new Set(offset.map((a) => a.id)).size, 1);
      });

      test('nothing is registered outside the clip windows', () => {
        const { perFrame } = collected;
        assert.deepEqual(perFrame[5], []);
        assert.equal(perFrame[9].length, 0);
        assert.equal(perFrame[10].length, 1);
        assert.equal(perFrame[29].length, 1);
        assert.equal(perFrame[30].length, 1);
      });

      test('a src of 400 characters or more is sent in full once, then by reference', () => {
        const long = collected.perFrame.slice(10, 30).flat();
        assert.equal(long.length, 20);
        assert.ok(long[0].src.length >= 400);
        assert.equal(long[0].src.startsWith('http://assets.invalid/aaaa'), true);
        for (const asset of long.slice(1)) assert.equal(asset.src, `same-as-${long[0].id}-${long[0].frame}`);
        const short = collected.perFrame.slice(30, 90).flat();
        assert.ok(short.every((a) => a.src === 'http://assets.invalid/offset.mp3'));
      });

      test('the page list is drained once per frame', () => {
        assert.deepEqual(collected.leftover, []);
      });
    });
  }
});
