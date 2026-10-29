import { describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { VERSIONS, bundleFixture, capture, launch, seek } from './harness.mjs';

const SIZE = { width: 320, height: 180 };

describe('item_15_frame_ordering', () => {
  for (const version of VERSIONS) {
    describe(version, () => {
      test('concurrent seeks on one page run one at a time, in call order', async () => {
        const run = await launch(bundleFixture(version, 'barrier'), { width: 64, height: 64, init: { compositionId: 'Barrier' } });
        try {
          const outcome = await run.page.eval(`(async () => {
            const order = [];
            const original = window.remotion_setFrame;
            window.remotion_setFrame = (frame, ...rest) => {
              order.push(frame);
              return original(frame, ...rest);
            };
            const settled = await Promise.all(
              [7, 3, 5, 12, 1].map((frame) =>
                __vrf.rm.seek(frame).then((result) => ({
                  returned: result.frame,
                  shown: document.querySelector('[data-frame]').dataset.frame,
                  done: document.querySelector('[data-frame]').dataset.done,
                })),
              ),
            );
            return { order, settled };
          })()`);
          assert.deepEqual(outcome.order, [7, 3, 5, 12, 1]);
          assert.deepEqual(
            outcome.settled,
            [7, 3, 5, 12, 1].map((frame) => ({ returned: frame, shown: String(frame), done: '3' })),
          );
        } finally {
          await run.close();
        }
      });

      test('a frame is the same bytes whatever order the workers deal it in', async () => {
        const frames = Array.from({ length: 10 }, (_, i) => i * 7);
        const open = () => launch(bundleFixture(version, 'basic'), { ...SIZE, config: { captureMode: 'png' }, init: { compositionId: 'Basic' } });
        const shoot = async (run, frame) => {
          await seek(run.page, frame);
          return (await capture(run.page, SIZE)).bytes.toString('base64');
        };
        const sequential = await open();
        const workerA = await open();
        const workerB = await open();
        try {
          const reference = [];
          for (const frame of frames) reference.push(await shoot(sequential, frame));
          const shuffled = [63, 7, 49, 0, 35, 56, 14, 21, 42, 28];
          const results = new Map();
          await Promise.all(
            [workerA, workerB].map(async (worker, w) => {
              for (const frame of shuffled.filter((_, i) => i % 2 === w)) results.set(frame, await shoot(worker, frame));
            }),
          );
          const reordered = frames.map((frame) => results.get(frame));
          assert.deepEqual(reordered, reference);
        } finally {
          await Promise.all([sequential.close(), workerA.close(), workerB.close()]);
        }
      });
    });
  }
});
