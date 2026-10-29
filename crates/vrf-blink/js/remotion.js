(() => {
  const config = {
    inputProps: '{}',
    envVariables: null,
    initialFrame: 0,
    proxyPort: 0,
    audioEnabled: true,
    videoEnabled: true,
    timeoutInMilliseconds: 30000,
    sampleRate: 48000,
    ...globalThis.__vrf_rm_config,
  };

  const nativeSetTimeout = globalThis.setTimeout.bind(globalThis);
  const nativeClearTimeout = globalThis.clearTimeout.bind(globalThis);

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

  const waitReady = (frame) =>
    new Promise((resolve, reject) => {
      let settling = false;
      const done = (fn, value) => {
        nativeClearTimeout(timer);
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

  const vrf = (globalThis.__vrf ??= {});
  vrf.rm = {
    config,
    ready: () => waitReady(null),
  };
})();
