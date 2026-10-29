import { after, before, describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { VERSIONS, bundleFixture, launch, seek } from './harness.mjs';
import { serve } from './serve.mjs';

const BMP = Buffer.from('424d3a000000000000003600000028000000010000000100000001001800000000000400000000000000000000000000000000000000ff00ff00', 'hex');
const FPS = 30;

const interpolate = (x, input, output) => {
  const [i0, i1, i2] = input;
  const [o0, o1, o2] = output;
  return x < i1 ? o0 + ((x - i0) * (o1 - o0)) / (i1 - i0) : o1 + ((x - i1) * (o2 - o1)) / (i2 - i1);
};

const pageTime = (frame, start, playbackRate) => interpolate(frame, [-1, start, start + 1], [-1, start, start + playbackRate]) / FPS;

describe('item_14_offthread_video_time', () => {
  let stub;
  let requests;
  before(async () => {
    requests = [];
    stub = await serve('/nonexistent', {
      onRequest: (url, _req, res) => {
        requests.push({ time: Number(url.searchParams.get('time')), src: url.searchParams.get('src'), transparent: url.searchParams.get('transparent'), path: url.pathname });
        res.writeHead(200, { 'content-type': 'image/bmp', 'access-control-allow-origin': '*', 'cache-control': 'no-store' });
        res.end(BMP);
        return true;
      },
    });
  });
  after(() => stub.close());

  const timesFor = async (version, compositionId, frames) => {
    const run = await launch(bundleFixture(version, 'offthread'), { width: 64, height: 64, config: { proxyPort: stub.port }, init: { compositionId } });
    try {
      const seen = {};
      for (const frame of frames) {
        requests.length = 0;
        await seek(run.page, frame);
        seen[frame] = requests.map((r) => r.time);
      }
      return seen;
    } finally {
      await run.close();
    }
  };

  const expectTimes = (seen, cases) => {
    for (const [frame, expected] of cases) {
      assert.equal(seen[frame].length, 1, `one request for frame ${frame}`);
      assert.ok(Math.abs(seen[frame][0] - expected) < 1e-9, `frame ${frame}: got ${seen[frame][0]}, formula says ${expected}`);
    }
  };

  for (const version of VERSIONS) {
    describe(version, () => {
      test('playbackRate 0.5 and 2 with startFrom follow the interpolate formula', async () => {
        const seen = await timesFor(version, 'Rates', [1, 15, 29, 31, 40, 61, 75]);
        expectTimes(seen, [
          [1, pageTime(1, 0, 0.5)],
          [15, pageTime(15, 0, 0.5)],
          [29, pageTime(29, 0, 0.5)],
          [31, pageTime(1 + 10, 10, 2)],
          [40, pageTime(10 + 10, 10, 2)],
          [61, pageTime(1, 0, 1)],
          [75, pageTime(15, 0, 1)],
        ]);
      });

      test('a Sequence that starts before zero reads ahead in the source', async () => {
        const seen = await timesFor(version, 'Negative', [5, 20, 30]);
        expectTimes(seen, [
          [5, pageTime(25, 0, 1)],
          [20, pageTime(40, 0, 1)],
          [30, pageTime(50, 0, 1)],
        ]);
      });

      test('a Loop restarts the source time every iteration', async () => {
        const seen = await timesFor(version, 'Looped', [1, 9, 11, 25]);
        expectTimes(seen, [
          [1, pageTime(1, 0, 1)],
          [9, pageTime(9, 0, 1)],
          [11, pageTime(1, 0, 1)],
          [25, pageTime(5, 0, 1)],
        ]);
      });

      test('a negative frame requests a negative time: clamping to 0 is the proxy job', async () => {
        requests.length = 0;
        const run = await launch(bundleFixture(version, 'offthread'), { width: 64, height: 64, config: { proxyPort: stub.port }, init: { compositionId: 'Frozen' } });
        await run.close();
        assert.equal(requests.length, 1);
        assert.ok(Math.abs(requests[0].time - pageTime(-3, 0, 1)) < 1e-9);
        assert.ok(requests[0].time < 0);
        assert.equal(Math.max(0, requests[0].time), 0);
      });

      test('the request goes to the proxy port with src, time and transparent', async () => {
        requests.length = 0;
        await timesFor(version, 'Alpha', [1]);
        assert.ok(requests.length >= 1);
        const last = requests.at(-1);
        assert.equal(last.path, '/proxy');
        assert.equal(last.src, 'http://assets.invalid/clip.mp4');
        assert.equal(last.transparent, 'true');
      });
    });
  }
});
