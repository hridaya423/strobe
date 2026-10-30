(() => {
  const real = {
    setTimeout: window.setTimeout.bind(window),
    clearTimeout: window.clearTimeout.bind(window),
    setInterval: window.setInterval.bind(window),
    clearInterval: window.clearInterval.bind(window),
    requestAnimationFrame: window.requestAnimationFrame.bind(window),
    cancelAnimationFrame: window.cancelAnimationFrame.bind(window),
  };
  const realNow = Date.now.bind(Date);
  const sleep = (ms) => new Promise((resolve) => real.setTimeout(resolve, ms));

  const FRAME_BOUNDARY_EPSILON = 1e-3;
  const RENDER_FRAME_PREFIX = '__render_frame_';
  const RENDER_FRAME_SUFFIX = '__';
  const MEDIA_VISUAL_STYLE_PROPERTIES = [
    'width', 'height', 'top', 'left', 'right', 'bottom', 'inset', 'object-fit', 'object-position', 'z-index',
    'opacity', 'visibility', 'filter', 'mix-blend-mode', 'backdrop-filter', 'border-width', 'border-style',
    'border-color', 'border-radius', 'overflow', 'clip-path', 'mask', 'mask-image', 'mask-size', 'mask-position',
    'mask-repeat', 'transform', 'transform-origin', 'translate', 'rotate', 'scale', 'box-sizing',
  ];
  const LAYOUT_PROPERTIES = new Set(['width', 'height', 'top', 'left', 'right', 'bottom', 'inset']);
  const CAPTURE_QUALITY = { draft: 80, standard: 100, high: 100 };

  const state = { fps: [30, 1], ready: false };

  const fpsValue = () => state.fps[0] / state.fps[1];

  const quantizeTime = (seconds, fps, subFrameDivisions = 1) => {
    const grid = Number.isFinite(fps) && fps > 0 ? fps : 30;
    const time = Number.isFinite(seconds) && seconds > 0 ? seconds : 0;
    if (subFrameDivisions <= 1) return Math.floor(time * grid + 1e-9) / grid;
    const subGrid = grid * subFrameDivisions;
    return Math.round(time * subGrid) / subGrid;
  };

  const parseFps = (value) => {
    if (Array.isArray(value)) return value;
    if (typeof value === 'number') return [value, 1];
    const match = /^(\d+)\s*\/\s*(\d+)$/.exec(String(value ?? ''));
    if (match) return [Number(match[1]), Number(match[2])];
    const number = Number(value);
    return Number.isFinite(number) && number > 0 ? [number, 1] : null;
  };

  const durationToFrameCount = (duration, fps) => {
    const raw = duration * fps;
    const nearest = Math.round(raw);
    return Math.abs(raw - nearest) <= FRAME_BOUNDARY_EPSILON ? nearest : Math.ceil(raw);
  };

  const evenUp = (n) => n + (n % 2);

  const renderSeekConfig = {
    mode: 'preview-phase',
    diagnostics: false,
    step: 1 / 120,
    offsetFraction: 0.5,
    fps: 30,
    fpsSource: 'render-options',
    owner: 'runtime',
  };

  window.__HF_RENDER_CAPTURE_MODE = true;
  window.__HF_EXPORT_RENDER_SEEK_CONFIG = renderSeekConfig;
  window.__HF_VIRTUAL_TIME__ = {
    originalSetTimeout: real.setTimeout,
    originalClearTimeout: real.clearTimeout,
    originalSetInterval: real.setInterval,
    originalClearInterval: real.clearInterval,
    originalRequestAnimationFrame: real.requestAnimationFrame,
    originalCancelAnimationFrame: real.cancelAnimationFrame,
    getTime: () => performance.now(),
  };

  const root = () => document.querySelector('[data-composition-id]');

  const declaredDuration = () => {
    const host = root();
    if (!host) return 0;
    const d = Number(host.getAttribute('data-duration'));
    if (Number.isFinite(d) && d > 0) return d;
    let maxEnd = 0;
    for (const sub of document.querySelectorAll('[data-composition-src]')) {
      const start = Number(sub.getAttribute('data-start')) || 0;
      const dur = Number(sub.getAttribute('data-duration')) || 0;
      if (dur > 0) maxEnd = Math.max(maxEnd, start + dur);
    }
    return maxEnd;
  };

  const setClock = (frame) => window.__vrf.clock.set(frame, state.fps[0], state.fps[1]);

  const hf = (window.__hf = window.__hf || {});
  Object.defineProperty(hf, 'duration', {
    configurable: true,
    enumerable: true,
    get() {
      const player = window.__player;
      if (!player || typeof player.getDuration !== 'function') return 0;
      if (window.__hfTimelinesBuilding || !window.__renderReady) return 0;
      const d = player.getDuration();
      return d > 0 ? d : declaredDuration();
    },
  });
  hf.seek = (time, options) => {
    window.__player.renderSeek(time, options);
    setClock(Math.round(Math.max(0, Number(time) || 0) * fpsValue() * 1e6) / 1e6);
  };

  const zeroDurationDiagnostic = () => {
    const player = window.__player;
    const hasTimeline = Boolean(window.__timelines && Object.keys(window.__timelines).length > 0);
    const declared = declaredDuration();
    const hints = [];
    if (!player) hints.push('window.__player was never set: the HyperFrames runtime did not initialize.');
    if (!hasTimeline) {
      hints.push('No GSAP timeline registered (window.__timelines is empty).');
      if (declared <= 0) hints.push('Fix: add data-duration="<seconds>" to your root <div data-composition-id="..."> element.');
    }
    if (window.__renderReady) hints.push('The runtime finished initializing but reported zero duration. This is permanent.');
    return (
      `[FrameCapture] Composition has zero duration.\n  Runtime ready: ${Boolean(window.__renderReady)}, __player: ${Boolean(player)}, ` +
      `__hf.seek: ${typeof hf.seek === 'function'}, GSAP timeline: ${hasTimeline}, data-duration: ${declared > 0 ? `${declared}s` : 'not set'}\n` +
      hints.map((h) => `  → ${h}`).join('\n')
    );
  };

  const waitHfReady = async (timeoutMs) => {
    const deadline = realNow() + timeoutMs;
    for (;;) {
      window.__hfFlushSync?.();
      if (typeof window.__hfStartupError === 'string') throw new Error(window.__hfStartupError);
      if (typeof hf.seek === 'function' && hf.duration > 0) return;
      if (realNow() >= deadline) break;
      await sleep(5);
    }
    if (hf.duration === 0) throw new Error(zeroDurationDiagnostic());
    throw new Error(`[FrameCapture] window.__hf not ready after ${timeoutMs}ms. Page must expose window.__hf = { duration, seek }.`);
  };

  const unregisteredTimelineIds = () => {
    const timelines = window.__timelines || {};
    const missing = [];
    for (const host of document.querySelectorAll('[data-composition-id]')) {
      if (host.hasAttribute('data-no-timeline')) continue;
      const id = host.getAttribute('data-composition-id');
      if (id && !timelines[id]) missing.push(id);
    }
    return missing;
  };

  let authorScan;
  const authorScriptsMayRegisterTimelines = async () => {
    if (document.readyState !== 'complete') return true;
    authorScan ??= (async () => {
      const mayRegister = (code) => code.includes('__timelines') || /\bimport\s*\(|^\s*import\s*[\w{*"']/m.test(code);
      const scripts = [
        ...document.querySelectorAll('script'),
        ...Array.from(document.querySelectorAll('template')).flatMap((t) => Array.from(t.content.querySelectorAll('script'))),
      ];
      for (const script of scripts) {
        if (script.type === 'module') return true;
        if (!script.src) {
          if (mayRegister(script.textContent ?? '')) return true;
          continue;
        }
        const url = new URL(script.src, location.href);
        if (url.origin !== location.origin) return true;
        try {
          if (mayRegister(await (await fetch(url)).text())) return true;
        } catch {
          return true;
        }
      }
      return false;
    })();
    return authorScan;
  };

  const waitSubCompositionTimelines = async (timeoutMs) => {
    const deadline = realNow() + timeoutMs;
    for (;;) {
      window.__hfFlushSync?.();
      const missing = unregisteredTimelineIds();
      if (missing.length === 0 || !(await authorScriptsMayRegisterTimelines())) break;
      if (realNow() >= deadline) {
        console.warn(`[FrameCapture] Sub-composition timelines not registered after ${timeoutMs}ms: ${missing.join(', ')}.`);
        break;
      }
      await sleep(5);
    }
    if (typeof window.__hfForceTimelineRebind === 'function') window.__hfForceTimelineRebind();
  };

  const waitVideosReady = async (timeoutMs, skip) => {
    const pending = () =>
      Array.from(document.querySelectorAll('video'))
        .filter((v) => !skip.has(v.id) && !v.closest('[data-hidden]'))
        .some((v) => v.readyState < 2 && !v.error && v.networkState !== HTMLMediaElement.NETWORK_NO_SOURCE);
    const deadline = realNow() + timeoutMs;
    while (pending() && realNow() < deadline) await sleep(5);
    return !pending();
  };

  const waitImagesReady = async (timeoutMs) => {
    const pending = () =>
      Array.from(document.images).some((img) => {
        const src = img.getAttribute('src') || '';
        return src && !src.startsWith('data:') && !img.complete;
      });
    const deadline = realNow() + timeoutMs;
    while (pending() && realNow() < deadline) await sleep(5);
    await Promise.all(Array.from(document.images).filter((img) => img.complete && img.naturalWidth > 0).map((img) => img.decode().catch(() => {})));
  };

  const ensureRenderFrameSiblings = () => {
    for (const video of document.querySelectorAll('video[data-start]')) {
      const next = video.nextElementSibling;
      if (next !== null && next.classList.contains('__render_frame__')) continue;
      const img = document.createElement('img');
      img.classList.add('__render_frame__');
      img.id = `${RENDER_FRAME_PREFIX}${video.id}${RENDER_FRAME_SUFFIX}`;
      img.style.pointerEvents = 'none';
      img.style.position = 'absolute';
      img.style.visibility = 'hidden';
      video.parentNode.insertBefore(img, video.nextSibling);
    }
  };

  const applyTransparentBackground = () => {
    const style = document.createElement('style');
    style.id = '__hf_transparent_bg__';
    style.textContent = 'html,body{background:transparent !important}';
    document.head.appendChild(style);
  };

  const captureSettings = ({ quality, alpha, motionBlur }) =>
    alpha || motionBlur
      ? { format: 'png', quality: null, transparent: Boolean(alpha) }
      : { format: 'jpeg', quality: CAPTURE_QUALITY[quality], transparent: false };

  const compositionMeta = () => {
    const host = root();
    const width = host ? parseInt(host.getAttribute('data-width') || '1080', 10) : 1080;
    const height = host ? parseInt(host.getAttribute('data-height') || '1920', 10) : 1920;
    const fps = fpsValue();
    const duration = hf.duration;
    return {
      width,
      height,
      outWidth: evenUp(width),
      outHeight: evenUp(height),
      fps: [...state.fps],
      duration,
      frames: durationToFrameCount(duration, fps),
    };
  };

  const applyVideoMetadataHints = async (hints) => {
    const posters = new Set();
    for (const hint of hints) {
      if (!hint.id || !(hint.width > 0) || !(hint.height > 0)) continue;
      const video = document.getElementById(hint.id);
      if (!video || video.hasAttribute('poster')) continue;
      const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${hint.width}" height="${hint.height}"/>`;
      video.poster = `data:image/svg+xml,${encodeURIComponent(svg)}`;
      posters.add(video.poster);
    }
    await Promise.all(
      [...posters].map((src) => {
        const image = new Image();
        image.src = src;
        return image.decode();
      }),
    );
  };

  const init = async ({ fps, timeoutMs = 45000, quality = 'standard', alpha = false, motionBlur = false, skipVideoIds = [], videoMetadataHints = [], nativeVideos = {} } = {}) => {
    for (const [id, params] of Object.entries(nativeVideos)) nativeClips.set(id, { params });
    state.fps = parseFps(fps) ?? parseFps(root()?.getAttribute('data-fps')) ?? [30, 1];
    renderSeekConfig.fps = fpsValue();
    window.__hfFlushSync?.();
    await waitHfReady(timeoutMs);
    await waitSubCompositionTimelines(timeoutMs);
    await waitHfReady(timeoutMs);
    await applyVideoMetadataHints(videoMetadataHints);
    await Promise.all([waitVideosReady(timeoutMs, new Set(skipVideoIds)), waitImagesReady(timeoutMs), document.fonts?.ready]);
    if (alpha) applyTransparentBackground();
    ensureRenderFrameSiblings();
    state.ready = true;
    return { ...compositionMeta(), capture: captureSettings({ quality, alpha, motionBlur }), needsDefaultBackgroundOverride: Boolean(alpha) };
  };

  const decodedCssImages = new Set();
  const decodeCssBackgroundImages = async () => {
    const pending = [];
    for (const el of document.querySelectorAll('*')) {
      const bg = getComputedStyle(el).backgroundImage;
      if (!bg || bg === 'none') continue;
      for (const match of bg.matchAll(/url\((["']?)(.*?)\1\)/g)) {
        const url = match[2];
        if (!url || url.startsWith('data:') || decodedCssImages.has(url)) continue;
        decodedCssImages.add(url);
        const image = new Image();
        image.src = url;
        pending.push(image.decode().catch(() => {}));
      }
    }
    await Promise.all(pending);
  };

  const isVisualAncestorHidden = (el) => {
    for (let parent = el.parentElement; parent !== null && parent !== document.documentElement; parent = parent.parentElement) {
      const computed = getComputedStyle(parent);
      if (computed.display === 'none') return true;
      if (computed.visibility === 'hidden' && (parent.hasAttribute('data-composition-src') || parent.hasAttribute('data-composition-file'))) return true;
    }
    return false;
  };

  const frameSibling = (video) => {
    const next = video.nextElementSibling;
    return next !== null && next.classList.contains('__render_frame__') ? next : null;
  };


  const roundAway = (value) => (value < 0 ? -Math.round(-value) : Math.round(value));

  const divideRounded = (numerator, denominator) => {
    const half = denominator / 2n;
    return numerator >= 0n ? (numerator + half) / denominator : -((half - numerator) / denominator);
  };

  const divideCeil = (numerator, denominator) => {
    const quotient = numerator / denominator;
    return numerator > 0n && numerator % denominator !== 0n ? quotient + 1n : quotient;
  };

  const truncatedMicroseconds = (seconds) => {
    const text = String(Math.abs(seconds));
    const sign = seconds < 0 ? -1n : 1n;
    if (/e/i.test(text)) return sign * BigInt(Math.trunc(Math.abs(seconds) * 1e6));
    const [whole, fraction = ''] = text.split('.');
    return sign * (BigInt(whole) * 1000000n + BigInt(fraction.slice(0, 6).padEnd(6, '0')));
  };

  const extractionSlots = (pts, timescale, clip) => {
    const ticks = BigInt(timescale);
    const [fpsNum, fpsDen] = clip.fps.map(BigInt);
    const offset = truncatedMicroseconds(clip.seek) + BigInt(clip.containerStartUs);
    const shift = divideRounded(offset * ticks, 1000000n);
    const [rateNum, rateDen] = clip.frameRate;
    const rate = rateNum / rateDen;
    const slots = new Array(pts.length);
    for (let index = 0; index < pts.length; index += 1) {
      const shifted = BigInt(pts[index]) - shift;
      if (clip.vfr) {
        const half = 1 / (2 * timescale) <= 0.001 ? 1n : 0n;
        slots[index] = Number(divideCeil((2n * shifted - half) * fpsNum, 2n * ticks * fpsDen));
        continue;
      }
      const micros = Number(divideRounded(shifted * 1000000n, ticks));
      const snapped = roundAway((micros * 1e-6 + clip.start - clip.lead) * rate);
      const retimed = BigInt(Math.trunc((clip.lead + snapped / rate - clip.start) / 1e-6));
      slots[index] = Number(divideCeil((2n * retimed - 1n) * fpsNum, 2000000n * fpsDen));
    }
    return slots;
  };

  const sourceFrameAtSlot = (slots, slot) => {
    let low = 0;
    let high = slots.length;
    while (low < high) {
      const middle = (low + high) >> 1;
      if (slots[middle] <= slot) low = middle + 1;
      else high = middle;
    }
    return Math.max(0, low - 1);
  };

  const nativeClips = new Map();
  const nativeSurfaces = new Map();

  const nativeClip = (id) => {
    const native = nativeClips.get(id);
    if (!native) throw new Error(`video "${id}" has no native decode parameters`);
    const video = globalThis.__vrf.video;
    if (!native.opened) {
      const jpeg = { bt709: String(native.params.matrix) === '709', full: Boolean(native.params.full) };
      native.opened = video.open(`${native.params.path}#${id}`, jpeg).then((clip) => {
        if (!clip) throw new Error(`video "${id}" (${native.params.path}) cannot be decoded natively: ${video.refusal()}`);
        native.slots = extractionSlots(clip.pts, clip.timescale, native.params);
        return clip;
      });
    }
    return native;
  };

  const decodedFrame = async (clip, k) => {
    const { jpeg } = await globalThis.__vrf.video.frame(clip, k);
    const url = URL.createObjectURL(new Blob([jpeg], { type: 'image/jpeg' }));
    const image = new Image();
    image.decoding = 'sync';
    image.src = url;
    try {
      await image.decode();
    } finally {
      URL.revokeObjectURL(url);
    }
    return image;
  };

  const nativeSurface = (id) => {
    let surface = nativeSurfaces.get(id);
    if (!surface) {
      surface = { image: null, shown: null, images: new Map() };
      nativeSurfaces.set(id, surface);
    }
    return surface;
  };

  const prefetchFrame = (surface, clip, k) => {
    if (surface.images.has(k)) return;
    const image = decodedFrame(clip, k);
    image.catch(() => {});
    surface.images.set(k, image);
  };

  const showNativeFrame = async (id, slot) => {
    const native = nativeClip(id);
    const clip = await native.opened;
    const surface = nativeSurface(id);
    const k = sourceFrameAtSlot(native.slots, slot);
    prefetchFrame(surface, clip, k);
    const image = await surface.images.get(k);
    for (const other of surface.images.keys()) if (other < k) surface.images.delete(other);
    if (surface.shown !== k && surface.image) surface.image.remove();
    surface.shown = k;
    surface.image = image;
    for (const ahead of [1, 2]) {
      const next = sourceFrameAtSlot(native.slots, slot + ahead);
      if (next !== k) prefetchFrame(surface, clip, next);
    }
    return surface;
  };

  const releaseNativeSurfaces = (active) => {
    for (const [id, surface] of nativeSurfaces) {
      if (active.has(id)) continue;
      surface.image?.remove();
      nativeSurfaces.delete(id);
    }
  };

  const injectVideoFrames = async (entries) => {
    const active = new Set(entries.map((entry) => entry.id));
    const decodes = [];
    const published = [];
    const natives = [];
    releaseNativeSurfaces(active);
    for (const video of document.querySelectorAll('video[data-start]')) {
      const img = frameSibling(video);
      const visible = active.has(video.id) && !isVisualAncestorHidden(video);
      video.style.setProperty('visibility', 'hidden', 'important');
      video.style.setProperty('pointer-events', 'none', 'important');
      if (img && !visible) img.style.setProperty('visibility', 'hidden', 'important');
    }
    for (const entry of entries) {
      const video = document.getElementById(entry.id);
      if (!video) continue;
      let img = frameSibling(video);
      if (!img) {
        img = document.createElement('img');
        img.classList.add('__render_frame__');
        img.id = `${RENDER_FRAME_PREFIX}${entry.id}${RENDER_FRAME_SUFFIX}`;
        img.style.pointerEvents = 'none';
        video.parentNode.insertBefore(img, video.nextSibling);
      }
      if (isVisualAncestorHidden(video)) {
        img.style.setProperty('visibility', 'hidden', 'important');
        continue;
      }
      const computed = getComputedStyle(video);
      const opacity = parseFloat(computed.opacity);
      const rect = video.getBoundingClientRect();
      const box = {
        left: Number.isFinite(video.offsetLeft) ? video.offsetLeft : 0,
        top: Number.isFinite(video.offsetTop) ? video.offsetTop : 0,
        width: video.offsetWidth > 0 ? video.offsetWidth : rect.width,
        height: video.offsetHeight > 0 ? video.offsetHeight : rect.height,
      };
      for (const property of MEDIA_VISUAL_STYLE_PROPERTIES) {
        if (property === 'opacity' || LAYOUT_PROPERTIES.has(property)) continue;
        const value = computed.getPropertyValue(property);
        if (value) img.style.setProperty(property, value);
      }
      Object.assign(img.style, {
        position: 'absolute',
        inset: 'auto',
        left: `${box.left}px`,
        top: `${box.top}px`,
        right: 'auto',
        bottom: 'auto',
        width: `${box.width}px`,
        height: `${box.height}px`,
        boxSizing: 'border-box',
        objectFit: computed.objectFit,
        objectPosition: computed.objectPosition,
        zIndex: computed.zIndex,
      });
      img.style.setProperty('visibility', 'hidden', 'important');
      img.decoding = 'sync';
      if (entry.slot !== undefined) {
        img.style.opacity = String(Number.isNaN(opacity) ? 1 : opacity);
        natives.push({ img, surface: showNativeFrame(entry.id, entry.slot) });
        continue;
      }
      if (entry.src !== undefined && img.getAttribute('src') !== entry.src) {
        if (entry.crossOrigin) img.crossOrigin = entry.crossOrigin;
        else img.removeAttribute('crossorigin');
        img.src = entry.src;
        decodes.push(
          img.decode().catch((error) => {
            throw new Error(`Video frame for "${entry.id}" failed to load (${entry.src}): ${error instanceof Error ? error.message : String(error)}`);
          }),
        );
      }
      img.style.opacity = String(Number.isNaN(opacity) ? 1 : opacity);
      published.push(img);
    }
    for (const { img, surface: pending } of natives) {
      const surface = await pending;
      surface.image.className = img.className;
      surface.image.style.cssText = img.style.cssText;
      surface.image.style.setProperty('visibility', 'hidden', 'important');
      if (surface.image.previousElementSibling !== img) img.after(surface.image);
      published.push(surface.image);
    }
    await Promise.all(decodes);
    for (const img of published) img.style.visibility = 'visible';
    if (published.length > 0) window.__hf?.colorGrading?.redraw?.();
  };

  const seekTime = async (seconds, { subFrameDivisions = 1, videos, seekOptions } = {}) => {
    const time = quantizeTime(seconds, fpsValue(), subFrameDivisions);
    hf.seek(time, seekOptions ?? (subFrameDivisions > 1 ? { subFrameDivisions } : undefined));
    const compositePending = Boolean(window.__hf_page_composite_pending);
    await decodeCssBackgroundImages();
    if (videos !== undefined) {
      await injectVideoFrames(videos);
      window.__hfReseekGpu?.(time);
    }
    await window.__hfWaitForSeekCompletion?.();
    await window.__hf?.colorGrading?.waitForActiveLuts?.();
    if (document.fonts && document.fonts.status === 'loading') await document.fonts.ready;
    return { time, compositePending };
  };

  const seek = async (frame, options = {}) => {
    const result = await seekTime(frame / fpsValue(), options);
    return { frame, ...result };
  };

  const videos = () =>
    Array.from(document.querySelectorAll('video[data-start]')).map((video) => ({
      id: video.id,
      src: video.getAttribute('src'),
      start: video.getAttribute('data-start'),
      duration: video.getAttribute('data-duration'),
      end: video.getAttribute('data-end'),
      mediaStart: Number(video.getAttribute('data-media-start') ?? video.getAttribute('data-playback-start')) || 0,
      playbackRate: Number(video.getAttribute('data-playback-rate')) || 1,
      loop: video.loop,
      hasAudio: video.getAttribute('data-has-audio') !== 'false',
    }));

  const finiteNumber = (value) => {
    if (value === null || value === undefined || String(value).trim() === '') return null;
    const number = Number(value);
    return Number.isFinite(number) ? number : null;
  };

  const authoredTimingWindow = (el) => {
    const parsedStart = finiteNumber(el.getAttribute('data-start'));
    if (parsedStart === null) return null;
    const start = Math.max(0, parsedStart);
    const positive = (value) => (value !== null && value > 0 ? value : null);
    const duration = positive(finiteNumber(el.getAttribute('data-duration'))) ?? positive(finiteNumber(el.getAttribute('data-hf-authored-duration')));
    if (duration !== null) return { start, end: start + duration };
    const later = (value) => (value !== null && value > start ? value : null);
    return { start, end: later(finiteNumber(el.getAttribute('data-end'))) ?? later(finiteNumber(el.getAttribute('data-hf-authored-end'))) };
  };

  const staticFrames = () => {
    const fps = fpsValue();
    const duration = hf.duration;
    const intervals = [];
    let tweenCount = 0;
    let hasTimelineCall = false;
    const walk = (timeline, offset) => {
      if (typeof timeline.getChildren !== 'function') return;
      if (typeof timeline.vars?.onUpdate === 'function') {
        const total = typeof timeline.totalDuration === 'function' ? timeline.totalDuration() : 0;
        if (total > 0) intervals.push({ start: offset, end: offset + total });
      }
      for (const child of timeline.getChildren(false, true, true)) {
        const start = offset + (typeof child.startTime === 'function' ? child.startTime() : 0);
        const single = typeof child.duration === 'function' ? child.duration() : 0;
        const total = typeof child.totalDuration === 'function' ? child.totalDuration() : single;
        if (typeof child.getChildren === 'function') {
          if (total > single + 1e-6) intervals.push({ start, end: start + total });
          walk(child, start);
        } else {
          tweenCount++;
          intervals.push({ start, end: start + total });
          if (total <= 1e-6 && (typeof child.vars?.onComplete === 'function' || typeof child.vars?.onReverseComplete === 'function')) hasTimelineCall = true;
        }
      }
    };
    for (const timeline of Object.values(window.__timelines || {})) if (timeline && typeof timeline.getChildren === 'function') walk(timeline, 0);
    const unresolvableClipStart = Array.from(document.querySelectorAll('[data-start]')).some((el) => {
      const value = el.dataset.start;
      return value != null && value.trim() !== '' && !Number.isFinite(parseFloat(value));
    });
    const nonGsapAnimation = document.getAnimations().some((a) => a.playState === 'running' || a.playState === 'paused');
    const reasons = [];
    if (!(duration > 0)) reasons.push('unknown/zero duration');
    if (document.querySelector('video')) reasons.push('video');
    if (document.querySelector('canvas')) reasons.push('canvas/webgl');
    if (document.querySelector('iframe')) reasons.push('iframe');
    if (tweenCount === 0) reasons.push('no GSAP tweens (non-GSAP animation)');
    if (nonGsapAnimation) reasons.push('running CSS/WAAPI animation');
    if (hasTimelineCall) reasons.push('tl.call() side effect (not seek-safe)');
    if (unresolvableClipStart) reasons.push('unresolvable clip start (reference expression)');
    const eligible = reasons.length === 0;
    const totalFrames = Math.max(1, Math.ceil(duration * fps));
    const animated = new Set();
    for (const { start, end } of intervals) {
      const lo = Math.max(0, Math.floor(start * fps));
      const hi = Math.min(totalFrames - 1, Math.ceil(end * fps));
      for (let f = lo; f <= hi; f++) animated.add(f);
    }
    for (const el of document.querySelectorAll('[data-start]')) {
      const window_ = authoredTimingWindow(el);
      if (!window_) continue;
      const edges = [Math.round(window_.start * fps)];
      if (window_.end !== null) edges.push(Math.round(window_.end * fps));
      for (const edge of edges) for (const f of [edge - 1, edge, edge + 1]) if (f >= 0) animated.add(f);
    }
    const frames = [];
    if (eligible) for (let f = 1; f < totalFrames; f++) if (!animated.has(f) && !animated.has(f - 1)) frames.push(f);
    return { eligible, reason: eligible ? 'eligible' : reasons.join('+'), frames };
  };

  const compositeFinish = async () => {
    if (typeof window.__hf_page_composite_resolve === 'function') window.__hf_page_composite_resolve();
  };
  const compositePrepare = async () => {
    if (typeof window.__hf_page_composite_prepare === 'function') await window.__hf_page_composite_prepare();
  };

  window.__vrf = window.__vrf || {};
  window.__vrf.hf = { init, seek, extractionSlots, sourceFrameAtSlot, seekTime, quantizeTime, videos, staticFrames, compositePrepare, compositeFinish, real };
})();
