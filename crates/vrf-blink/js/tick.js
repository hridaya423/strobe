(() => {
  if (window !== window.top) return;
  const raf = window.requestAnimationFrame.bind(window);
  let element = null;
  let phase = 0;
  (globalThis.__vrf ??= {}).tick = () => element;
  const tick = () => {
    if (!element || !element.isConnected) {
      if (!document.documentElement) return raf(tick);
      element = document.createElement('div');
      element.style.cssText =
        'position:fixed;left:0;top:0;width:1px;height:1px;background:#000;opacity:0.0005;pointer-events:none;z-index:2147483647;will-change:opacity';
      document.documentElement.appendChild(element);
    }
    element.style.opacity = phase++ & 1 ? '0.0005' : '0.001';
    raf(tick);
  };
  raf(tick);
})();
