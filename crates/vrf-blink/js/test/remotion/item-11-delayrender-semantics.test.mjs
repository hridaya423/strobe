import { describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { VERSIONS, bundleFixture, launch, seek } from './harness.mjs';

const config = { timeoutInMilliseconds: 2500 };

const withComposition = async (version, compositionId, fn, extra = {}) => {
  const run = await launch(bundleFixture(version, 'barrier'), { width: 64, height: 64, config: { ...config, ...extra }, init: { compositionId } });
  try {
    await fn(run.page);
  } finally {
    await run.close();
  }
};

describe('item_11_delayrender_semantics', () => {
  for (const version of VERSIONS) {
    describe(version, () => {
      test('an unresolved handle fails the frame with its label and the timeout minus 2000 ms', () =>
        withComposition(version, 'Hang', async (page) => {
          await seek(page, 2);
          await assert.rejects(seek(page, 3), /A delayRender\(\) "hang" was called but not cleared after 500ms\. See https:\/\/remotion\.dev\/docs\/timeout/);
        }));

      test('a handle open during init fails init', async () => {
        await assert.rejects(
          launch(bundleFixture(version, 'barrier'), { width: 64, height: 64, config, init: { compositionId: 'Never' } }),
          /A delayRender\(\) "never cleared" was called but not cleared after 500ms/,
        );
      });

      test('the timeout follows the configured value', () =>
        withComposition(
          version,
          'Hang',
          async (page) => {
            await seek(page, 2);
            await assert.rejects(seek(page, 3), /not cleared after 1000ms/);
          },
          { timeoutInMilliseconds: 3000 },
        ));

      test('cancelRender aborts the render and stays aborted', () =>
        withComposition(version, 'Cancel', async (page) => {
          await seek(page, 1);
          await assert.rejects(seek(page, 2), /cancelled on purpose/);
          await assert.rejects(seek(page, 5), /cancelled on purpose/);
        }));
    });
  }

  describe('retries and attempts', () => {
    for (const version of ['4.0.169', '4.0.240']) {
      test(`${version}: retries consume remotion_attempt`, async () => {
        await withComposition(version, 'Hang', async (page) => {
          await seek(page, 2);
          await assert.rejects(seek(page, 3, { attempt: 1 }), /Retries left: 2 - Rendering the frame will be retried\./);
        });
        await withComposition(version, 'Hang', async (page) => {
          await seek(page, 2);
          await assert.rejects(seek(page, 3, { attempt: 2 }), /Retries left: 1 - Rendering the frame will be retried\./);
        });
        await withComposition(version, 'Hang', async (page) => {
          await seek(page, 2);
          await assert.rejects(seek(page, 3, { attempt: 3 }), (err) => /not cleared after 500ms/.test(err.message) && !/Retries left/.test(err.message));
        });
      });
    }

    test('4.0.60 predates delayRender retries: the option is ignored and the error is uncaught', () =>
      withComposition('4.0.60', 'Hang', async (page) => {
        await seek(page, 2);
        await assert.rejects(seek(page, 3, { attempt: 1 }), (err) => /not cleared after 500ms/.test(err.message) && !/Retries left/.test(err.message));
      }));
  });

  test('the adapter times out on its own when the page never reports', () =>
    withComposition('4.0.240', 'Silent', async (page) => {
      await seek(page, 2);
      await assert.rejects(seek(page, 3), /Timeout exceeded rendering the component at frame 3\. Open delayRender\(\) handles: "1\. silent"/);
    }));
});
