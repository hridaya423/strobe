(() => {
  const NativeDate = Date;
  const nativePause = Animation.prototype.pause;
  const clock = { epochMs: 0 };
  let t = 0;

  const dateNow = () => Math.floor(clock.epochMs + t);

  function define(target, name, descriptor) {
    Object.defineProperty(target, name, { configurable: true, ...Object.getOwnPropertyDescriptor(target, name), ...descriptor });
  }

  const replace = (target, name, value) => define(target, name, { value });

  const VirtualDate = new Proxy(NativeDate, {
    construct: (target, args, newTarget) => Reflect.construct(target, args.length ? args : [dateNow()], newTarget),
    apply: () => new NativeDate(dateNow()).toString(),
  });
  replace(NativeDate, 'now', function now() {
    return dateNow();
  });
  replace(NativeDate.prototype, 'constructor', VirtualDate);
  replace(globalThis, 'Date', VirtualDate);

  replace(Performance.prototype, 'now', function now() {
    return t;
  });
  define(Performance.prototype, 'timeOrigin', {
    get() {
      return clock.epochMs;
    },
  });
  define(DocumentTimeline.prototype, 'currentTime', {
    get() {
      return t;
    },
    enumerable: true,
  });

  let timerId = 0;
  let nesting = 0;
  const timers = [];
  const timersById = new Map();

  const timerDelay = (timer) => (timer.level > 5 ? Math.max(timer.delay, 4) : timer.delay);

  function enqueue(timer) {
    let i = timers.length;
    while (i > 0 && timers[i - 1].due > timer.due) i--;
    timers.splice(i, 0, timer);
  }

  function addTimer(repeat, callback, delay, args) {
    const timer = { id: ++timerId, callback, args, repeat, delay: Math.max(0, Number(delay) || 0), level: nesting + 1, cancelled: false };
    timer.due = t + timerDelay(timer);
    timersById.set(timer.id, timer);
    enqueue(timer);
    return timer.id;
  }

  function clearTimer(id) {
    const timer = timersById.get(id);
    if (!timer) return;
    timer.cancelled = true;
    timersById.delete(id);
  }

  function runTimers(limit) {
    while (timers.length && timers[0].due <= limit) {
      const timer = timers.shift();
      if (timer.cancelled) continue;
      t = timer.due;
      nesting = timer.level;
      try {
        Reflect.apply(timer.callback, globalThis, timer.args);
      } catch (error) {
        reportError(error);
      }
      nesting = 0;
      if (timer.repeat && !timer.cancelled) {
        timer.level++;
        timer.due += timerDelay(timer);
        enqueue(timer);
      } else {
        timersById.delete(timer.id);
      }
    }
  }

  replace(globalThis, 'setTimeout', function setTimeout(callback, delay, ...args) {
    return addTimer(false, callback, delay, args);
  });
  replace(globalThis, 'setInterval', function setInterval(callback, delay, ...args) {
    return addTimer(true, callback, delay, args);
  });
  replace(globalThis, 'clearTimeout', function clearTimeout(id) {
    clearTimer(id);
  });
  replace(globalThis, 'clearInterval', function clearInterval(id) {
    clearTimer(id);
  });

  let frameCallbackId = 0;
  const frameCallbacks = new Map();

  replace(globalThis, 'requestAnimationFrame', function requestAnimationFrame(callback) {
    frameCallbacks.set(++frameCallbackId, callback);
    return frameCallbackId;
  });
  replace(globalThis, 'cancelAnimationFrame', function cancelAnimationFrame(id) {
    frameCallbacks.delete(id);
  });

  function runFrameCallbacks() {
    for (const id of [...frameCallbacks.keys()]) {
      const callback = frameCallbacks.get(id);
      if (!callback) continue;
      frameCallbacks.delete(id);
      try {
        callback(t);
      } catch (error) {
        reportError(error);
      }
    }
  }

  let videoCallbackId = 0;
  const videoCallbacks = new Map();
  const shownMediaTime = new WeakMap();
  const shownFrames = new WeakMap();

  replace(HTMLVideoElement.prototype, 'requestVideoFrameCallback', function requestVideoFrameCallback(callback) {
    videoCallbacks.set(++videoCallbackId, { video: this, callback });
    return videoCallbackId;
  });
  replace(HTMLVideoElement.prototype, 'cancelVideoFrameCallback', function cancelVideoFrameCallback(id) {
    videoCallbacks.delete(id);
  });

  function runVideoFrameCallbacks() {
    const ready = [...videoCallbacks].filter(([, { video }]) => video.readyState >= 2 && shownMediaTime.get(video) !== video.currentTime);
    for (const [, { video }] of ready) {
      shownMediaTime.set(video, video.currentTime);
      shownFrames.set(video, (shownFrames.get(video) ?? 0) + 1);
    }
    for (const [id, { video, callback }] of ready) {
      if (!videoCallbacks.delete(id)) continue;
      try {
        callback(t, {
          presentationTime: t,
          expectedDisplayTime: t,
          width: video.videoWidth,
          height: video.videoHeight,
          mediaTime: video.currentTime,
          presentedFrames: shownFrames.get(video),
          processingDuration: 0,
        });
      } catch (error) {
        reportError(error);
      }
    }
  }

  const animationStates = new WeakMap();

  replace(Animation.prototype, 'pause', function pause() {
    const state = animationStates.get(this);
    if (state) state.userPaused = true;
    return nativePause.call(this);
  });

  function holdRunningAnimations() {
    for (const animation of document.getAnimations()) {
      if (animation.playState !== 'running') continue;
      const ct = animation.pending ? (animation.currentTime ?? 0) : 0;
      nativePause.call(animation);
      animation.currentTime = ct;
      animationStates.set(animation, { ct, t, userPaused: false });
    }
  }

  function seekAnimations(to) {
    for (const animation of document.getAnimations()) {
      const state = animationStates.get(animation);
      if (!state || state.userPaused || animation.playState !== 'paused') continue;
      const rate = animation.playbackRate;
      state.ct += (to - state.t) * rate;
      state.t = to;
      const done = rate > 0 ? state.ct >= animation.effect.getComputedTiming().endTime : rate < 0 && state.ct <= 0;
      if (done) animation.finish();
      else animation.currentTime = state.ct;
    }
  }

  function setMs(ms) {
    if (ms < t) throw new RangeError(`virtual time cannot go backwards: ${ms} < ${t}`);
    holdRunningAnimations();
    runTimers(ms);
    t = ms;
    seekAnimations(ms);
    runFrameCallbacks();
    runVideoFrameCallbacks();
    holdRunningAnimations();
    for (let i = 0; i < window.length; i++) {
      try {
        window[i].__vrf.clock.setMs(ms);
      } catch {}
    }
  }

  clock.setMs = setMs;
  clock.set = (frame, fpsNum, fpsDen = 1) => setMs((frame * 1000 * fpsDen) / fpsNum);
  clock.timeMs = () => t;

  if (parent !== window) {
    try {
      t = parent.__vrf.clock.timeMs();
      clock.epochMs = parent.__vrf.clock.epochMs;
    } catch {}
  }

  (globalThis.__vrf ??= {}).clock = clock;
})();
