(() => {
  const OriginalDate = Date;
  let nowMs = 0;
  let nextId = 1;
  let queue = [];
  const frames = [];

  function VirtualDate(...args) {
    if (!new.target) return OriginalDate.apply(null, args.length ? args : [nowMs]);
    const instance = args.length ? new OriginalDate(...args) : new OriginalDate(nowMs);
    Object.setPrototypeOf(instance, VirtualDate.prototype);
    return instance;
  }
  VirtualDate.prototype = OriginalDate.prototype;
  Object.setPrototypeOf(VirtualDate, OriginalDate);
  VirtualDate.now = () => nowMs;
  window.Date = VirtualDate;
  Object.defineProperty(performance, 'now', { configurable: true, value: () => nowMs });

  window.requestAnimationFrame = (callback) => {
    const entry = { id: nextId++, callback, cancelled: false };
    queue.push(entry);
    return entry.id;
  };
  window.cancelAnimationFrame = (id) => {
    for (const entry of queue) if (entry.id === id) entry.cancelled = true;
  };

  window.__vrf = window.__vrf ?? {};
  window.__vrf.clock = {
    calls: frames,
    set(frame, num, den) {
      nowMs = (frame * 1000 * den) / num;
      frames.push([frame, num, den, nowMs]);
      const current = queue;
      queue = [];
      for (const entry of current) if (!entry.cancelled) entry.callback(nowMs);
    },
  };
})();
