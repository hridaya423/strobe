import { after, before, describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { VERSIONS, bundleFixture, launch } from './harness.mjs';

const SITE_VERSION = { '4.0.60': '10', '4.0.169': '11', '4.0.240': '11' };

describe('item_09_entry_contract', () => {
  for (const version of VERSIONS) {
    describe(version, () => {
      let run;
      const config = {
        inputProps: JSON.stringify({ label: 'from-config' }),
        envVariables: { FOO: 'bar' },
        initialFrame: 7,
        proxyPort: 4321,
        audioEnabled: false,
        videoEnabled: true,
        timeoutInMilliseconds: 12000,
      };
      before(async () => {
        run = await launch(bundleFixture(version, 'basic'), { config });
      });
      after(() => run.close());

      test('an unmodified bundle boots and registers compositions', async () => {
        const { page } = run;
        assert.equal(await page.eval('typeof window.getStaticCompositions'), 'function');
        assert.equal(await page.eval('window.remotion_version'), version);
        assert.equal(await page.eval('window.siteVersion'), SITE_VERSION[version]);
        const ids = await page.eval(`(async () => {
          window.remotion_setBundleMode({ type: 'evaluation' });
          await __vrf.rm.ready();
          return (await window.getStaticCompositions()).map((c) => c.id);
        })()`);
        assert.deepEqual(ids, ['Basic', 'Props', 'Transparent', 'Meta']);
      });

      test('pre-boot globals carry the render settings', async () => {
        const { page } = run;
        const g = JSON.parse(
          await page.eval(`JSON.stringify({
            inputProps: window.remotion_inputProps,
            envVariables: window.remotion_envVariables,
            initialFrame: window.remotion_initialFrame,
            attempt: window.remotion_attempt,
            proxyPort: window.remotion_proxyPort,
            audioEnabled: window.remotion_audioEnabled,
            videoEnabled: window.remotion_videoEnabled,
            sampleRate: window.remotion_sampleRate,
            timeout: window.remotion_puppeteerTimeout,
            isMainTab: window.remotion_isMainTab,
          })`),
        );
        assert.deepEqual(g, {
          inputProps: '{"label":"from-config"}',
          envVariables: '{"FOO":"bar"}',
          initialFrame: 7,
          attempt: 1,
          proxyPort: 4321,
          audioEnabled: false,
          videoEnabled: true,
          sampleRate: 48000,
          timeout: 12000,
          isMainTab: true,
        });
      });

      test('the bundle supplies the per-frame globals the barrier drives', async () => {
        const { page } = run;
        const kinds = JSON.parse(
          await page.eval(`(async () => {
            window.remotion_setBundleMode({ type: 'evaluation' });
            await __vrf.rm.ready();
            return JSON.stringify({
              setFrame: typeof window.remotion_setFrame,
              collectAssets: typeof window.remotion_collectAssets,
              setBundleMode: typeof window.remotion_setBundleMode,
              calculateComposition: typeof window.remotion_calculateComposition,
              renderReady: typeof window.remotion_renderReady,
              delayRenderTimeouts: typeof window.remotion_delayRenderTimeouts,
              cancelledError: typeof window.remotion_cancelledError,
            });
          })()`),
        );
        assert.deepEqual(kinds, {
          setFrame: 'function',
          collectAssets: 'function',
          setBundleMode: 'function',
          calculateComposition: 'function',
          renderReady: 'boolean',
          delayRenderTimeouts: 'object',
          cancelledError: 'undefined',
        });
      });

      test('alert and confirm cancel the render', async () => {
        const { page } = run;
        await page.eval(`window.alert('boom')`);
        assert.match(await page.eval('window.remotion_cancelledError'), /alert\(\)/);
        await page.eval('window.remotion_cancelledError = undefined');
        assert.equal(await page.eval(`window.confirm('really?')`), false);
        assert.match(await page.eval('window.remotion_cancelledError'), /confirm\(\)/);
      });
    });
  }
});
