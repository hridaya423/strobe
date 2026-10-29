globalThis.process ??= { env: {} };
globalThis.process.env ??= {};
globalThis.process.env.NODE_ENV = 'production';

(() => {
  const config = {
    inputProps: '{}',
    envVariables: null,
    initialFrame: 0,
    proxyPort: 0,
    serveUrl: null,
    audioEnabled: true,
    videoEnabled: true,
    timeoutInMilliseconds: 30000,
    sampleRate: 48000,
    captureMode: 'raw',
    ...globalThis.__vrf_rm_config,
  };

  const nativeSetTimeout = globalThis.setTimeout.bind(globalThis);
  const nativeClearTimeout = globalThis.clearTimeout.bind(globalThis);
  const nativeSetInterval = globalThis.setInterval.bind(globalThis);
  const nativeClearInterval = globalThis.clearInterval.bind(globalThis);
  const nativeNow = performance.now.bind(performance);
  const PUMP_MS = 4;

  if (config.serveUrl) {
    const nativeFetch = globalThis.fetch.bind(globalThis);
    const proxy = `http://localhost:${config.proxyPort}/proxy?`;
    globalThis.fetch = (input, init) => {
      if (typeof input === 'string' && input.startsWith(proxy)) {
        const url = new URL(input);
        const src = url.searchParams.get('src');
        if (src?.startsWith(`${location.origin}/`)) url.searchParams.set('src', config.serveUrl + src.slice(location.origin.length));
        url.searchParams.set('time', String(Math.max(0, Number(url.searchParams.get('time')))));
        input = url.toString();
      }
      return nativeFetch(input, init);
    };
  }

  let renderReady = false;
  let cancelledError;
  const pageErrors = [];
  const watchers = new Set();
  const notify = () => {
    for (const watcher of [...watchers]) watcher();
  };

  Object.defineProperty(window, 'remotion_renderReady', {
    configurable: true,
    enumerable: true,
    get: () => renderReady,
    set(value) {
      renderReady = value;
      if (value === true) notify();
    },
  });
  Object.defineProperty(window, 'remotion_cancelledError', {
    configurable: true,
    enumerable: true,
    get: () => cancelledError,
    set(value) {
      cancelledError = value;
      if (value !== undefined) notify();
    },
  });
  window.addEventListener('error', (event) => {
    pageErrors.push(event.error?.stack ?? event.message);
    notify();
  });

  window.remotion_puppeteerTimeout = config.timeoutInMilliseconds;
  window.remotion_inputProps = config.inputProps;
  window.remotion_initialFrame = config.initialFrame;
  window.remotion_attempt = 1;
  window.remotion_proxyPort = config.proxyPort;
  window.remotion_audioEnabled = config.audioEnabled;
  window.remotion_videoEnabled = config.videoEnabled;
  window.remotion_sampleRate = config.sampleRate;
  window.remotion_isMainTab = true;
  if (config.envVariables) window.remotion_envVariables = JSON.stringify(config.envVariables);

  window.alert = (message) => {
    window.remotion_cancelledError = `Error: alert() was called with ${JSON.stringify(String(message))}. Dialogs cannot be answered during a render.`;
  };
  window.confirm = (message) => {
    window.remotion_cancelledError = `Error: confirm() was called with ${JSON.stringify(String(message))}. Dialogs cannot be answered during a render.`;
    return false;
  };

  const channel = new MessageChannel();
  const turns = [];
  channel.port1.onmessage = () => turns.shift()();
  const macrotask = () =>
    new Promise((resolve) => {
      turns.push(resolve);
      channel.port2.postMessage(0);
    });

  const openHandles = () =>
    Object.values(window.remotion_delayRenderTimeouts ?? {})
      .map(({ label }, i) => (label === null ? `${i + 1}. (no label)` : `"${i + 1}. ${label}"`))
      .join(', ');

  const clock = () => globalThis.__vrf?.clock;
  const flushClock = () => clock()?.setMs(clock().timeMs());
  const advanceClock = (ms) => clock()?.setMs(Math.max(clock().timeMs(), ms));

  const startPump = () => {
    let last = nativeNow();
    return nativeSetInterval(() => {
      const now = nativeNow();
      advanceClock(clock().timeMs() + now - last);
      last = now;
    }, PUMP_MS);
  };

  const waitReady = (frame) =>
    new Promise((resolve, reject) => {
      let settling = false;
      const pump = clock() ? startPump() : null;
      const done = (fn, value) => {
        nativeClearTimeout(timer);
        nativeClearInterval(pump);
        watchers.delete(check);
        fn(value);
      };
      const timer = nativeSetTimeout(() => {
        const where = frame === null ? '' : ` at frame ${frame}`;
        const handles = openHandles();
        done(reject, new Error(`Timeout exceeded rendering the component${where}. ${handles ? `Open delayRender() handles: ${handles}` : ''}`));
      }, config.timeoutInMilliseconds + 3000);
      const check = async () => {
        if (pageErrors.length) return done(reject, new Error(pageErrors.shift()));
        if (cancelledError !== undefined) return done(reject, new Error(cancelledError));
        if (renderReady !== true || settling) return;
        settling = true;
        await macrotask();
        settling = false;
        if (renderReady === true && cancelledError === undefined && !pageErrors.length) done(resolve);
        else check();
      };
      watchers.add(check);
      check();
    });

  const CAPTURE = {
    raw: { format: 'raw', backgroundOverride: null },
    jpeg: { format: 'jpeg', quality: 80, optimizeForSpeed: true, backgroundOverride: null },
    png: { format: 'png', backgroundOverride: { r: 0, g: 0, b: 0, a: 0 } },
  };

  const LONG_SRC = 400;
  const longSrcs = new Map();

  const collectAssets = () =>
    window.remotion_collectAssets().map((asset) => {
      if (asset.src === undefined || asset.src.length < LONG_SRC) return asset;
      const first = longSrcs.get(asset.src);
      if (first) return { ...asset, src: `same-as-${first.id}-${first.frame}` };
      longSrcs.set(asset.src, { id: asset.id, frame: asset.frame });
      return asset;
    });

  let compositionId = null;
  let queue = Promise.resolve();

  const registered = async (id) => {
    for (let turn = 0; turn < 200; turn++) {
      try {
        if (window.remotion_getCompositionNames().includes(id)) return;
      } catch {}
      await macrotask();
    }
  };

  let compositionFps = null;

  let resolved = null;
  let resolving = null;

  const resolve = async ({ compositionId: id }) => {
    if (compositionId !== null || resolving !== null) throw new Error('init() runs once per page: the bundle keeps the first composition, load a new page for another one');
    resolving = id;
    window.remotion_setBundleMode({ type: 'evaluation' });
    await waitReady(null);
    await registered(id);
    resolved = await window.remotion_calculateComposition(id);
    const { width, height, fps, durationInFrames, defaultCodec } = resolved;
    return { id, width, height, fps, durationInFrames, defaultCodec, serializedProps: resolved.serializedResolvedPropsWithCustomSchema };
  };

  const mount = async () => {
    window.remotion_setBundleMode({
      type: 'composition',
      compositionName: resolving,
      serializedResolvedPropsWithSchema: resolved.serializedResolvedPropsWithCustomSchema,
      compositionDurationInFrames: resolved.durationInFrames,
      compositionFps: resolved.fps,
      compositionHeight: resolved.height,
      compositionWidth: resolved.width,
      compositionDefaultCodec: resolved.defaultCodec,
    });
    await waitReady(null);
    compositionId = resolving;
    compositionFps = resolved.fps;
  };

  const init = async (options) => {
    const meta = await resolve(options);
    await mount();
    return meta;
  };

  const pendingImages = () => [...document.images].filter((img) => !img.complete);

  const barrier = async (frame, attempt) => {
    await waitReady(null);
    advanceClock((frame * 1000) / compositionFps);
    window.remotion_setFrame(frame, compositionId, attempt);
    const ready = waitReady(frame);
    if (clock()) {
      await macrotask();
      flushClock();
    }
    await ready;
    await document.fonts.ready;
    await Promise.all(pendingImages().map((img) => img.decode().catch(() => {})));
    document.body.style.background = config.captureMode === 'png' ? 'transparent' : 'black';
    return { frame, assets: collectAssets() };
  };

  const seek = (frame, { attempt = 1 } = {}) => {
    const result = queue.catch(() => {}).then(() => barrier(frame, attempt));
    queue = result;
    return result;
  };

  const vrf = (globalThis.__vrf ??= {});
  vrf.rm = {
    config,
    ready: () => waitReady(null),
    init,
    resolve,
    mount,
    seek,
    captureParams: () => CAPTURE[config.captureMode],
  };
})();
