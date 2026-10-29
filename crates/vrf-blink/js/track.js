(() => {
  if (window !== window.top) return;
  const ANIMATED_IMAGE = /(\.(gif|apng|webp|avif|svg)([?#"')\s]|$))|(data:image\/(gif|apng|webp|avif|svg))/i;
  const BLOCKERS = [
    'canvas',
    'video',
    'iframe',
    'embed',
    'object',
    'marquee',
    'animate',
    'animateTransform',
    'animateMotion',
    'set',
    'template[shadowrootmode]',
    'template[shadowroot]',
  ].join(',');

  let hidden = 0;
  let structural = false;
  let sheetsKey = null;
  let sheetsAnimated = false;
  const changes = new Map();

  const note = (records) => {
    const tick = globalThis.__vrf?.tick?.();
    for (const record of records) {
      if (record.type === 'childList') {
        const touched = [...record.addedNodes, ...record.removedNodes];
        if (touched.length === 0 || touched.some((node) => node !== tick)) structural = true;
      } else if (record.target !== tick) {
        const key = record.type === 'attributes' ? record.attributeName : '#data';
        const old = changes.get(record.target) ?? new Map();
        if (!old.has(key)) old.set(key, record.oldValue);
        changes.set(record.target, old);
      }
    }
  };

  const observer = new MutationObserver(note);
  const options = { subtree: true, childList: true, attributes: true, attributeOldValue: true, characterData: true, characterDataOldValue: true };
  observer.observe(document, options);

  const hide = () => {
    hidden++;
  };

  const wrap = (target, name) => {
    const descriptor = Object.getOwnPropertyDescriptor(target, name);
    if (!descriptor) return;
    if (typeof descriptor.value === 'function') {
      const original = descriptor.value;
      Object.defineProperty(target, name, {
        ...descriptor,
        value: function (...args) {
          hide();
          return original.apply(this, args);
        },
      });
    } else if (descriptor.set) {
      const original = descriptor.set;
      Object.defineProperty(target, name, {
        ...descriptor,
        set(value) {
          hide();
          original.call(this, value);
        },
      });
    }
  };

  for (const name of ['insertRule', 'deleteRule', 'replace', 'replaceSync', 'addRule', 'removeRule']) wrap(CSSStyleSheet.prototype, name);
  for (const name of ['insertRule', 'deleteRule']) wrap(CSSGroupingRule.prototype, name);
  wrap(Document.prototype, 'adoptedStyleSheets');
  wrap(ShadowRoot.prototype, 'adoptedStyleSheets');
  for (const name of ['scrollTop', 'scrollLeft', 'scroll', 'scrollTo', 'scrollBy', 'scrollIntoView']) wrap(Element.prototype, name);
  for (const name of ['scroll', 'scrollTo', 'scrollBy']) wrap(window, name);
  for (const [type, names] of [
    [HTMLInputElement, ['value', 'checked', 'indeterminate', 'valueAsNumber']],
    [HTMLTextAreaElement, ['value']],
    [HTMLSelectElement, ['value', 'selectedIndex']],
    [HTMLMediaElement, ['currentTime']],
  ]) for (const name of names) wrap(type.prototype, name);
  for (const name of ['animate']) wrap(Element.prototype, name);
  for (const name of ['play', 'finish', 'cancel', 'reverse', 'persist']) wrap(Animation.prototype, name);

  const prototype = CSSStyleDeclaration.prototype;
  for (const [name, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(prototype))) {
    if (typeof descriptor.value === 'function') {
      if (name !== 'setProperty' && name !== 'removeProperty') continue;
      const original = descriptor.value;
      Object.defineProperty(prototype, name, {
        ...descriptor,
        value: function (...args) {
          if (this.parentRule) hide();
          return original.apply(this, args);
        },
      });
    } else if (descriptor.set) {
      const original = descriptor.set;
      Object.defineProperty(prototype, name, {
        ...descriptor,
        set(value) {
          if (this.parentRule) hide();
          original.call(this, value);
        },
      });
    }
  }

  const watched = new WeakMap();
  const watchRuleStyle = (declaration) => {
    if (!watched.has(declaration)) {
      watched.set(
        declaration,
        new Proxy(declaration, {
          get(target, name) {
            const value = Reflect.get(target, name, target);
            return typeof value === 'function' ? value.bind(target) : value;
          },
          set(target, name, value) {
            hide();
            return Reflect.set(target, name, value, target);
          },
          deleteProperty(target, name) {
            hide();
            return Reflect.deleteProperty(target, name);
          },
        }),
      );
    }
    return watched.get(declaration);
  };
  for (const type of [CSSStyleRule, CSSKeyframeRule, CSSPageRule, CSSFontFaceRule]) {
    const descriptor = Object.getOwnPropertyDescriptor(type.prototype, 'style');
    if (!descriptor?.get) continue;
    Object.defineProperty(type.prototype, 'style', {
      ...descriptor,
      get() {
        return watchRuleStyle(descriptor.get.call(this));
      },
    });
  }

  const attachShadow = Element.prototype.attachShadow;
  Object.defineProperty(Element.prototype, 'attachShadow', {
    configurable: true,
    writable: true,
    value: function (init) {
      const root = attachShadow.call(this, init);
      observer.observe(root, options);
      return root;
    },
  });

  document.addEventListener('load', hide, true);
  document.addEventListener('error', hide, true);
  try {
    new PerformanceObserver(hide).observe({ type: 'resource' });
  } catch {}

  const animatedSheets = () => {
    const key = `${document.styleSheets.length}:${hidden}`;
    if (key === sheetsKey) return sheetsAnimated;
    sheetsKey = key;
    sheetsAnimated = false;
    const scan = (rules) => {
      for (const rule of rules) {
        if (ANIMATED_IMAGE.test(rule.cssText) && /url\(/.test(rule.cssText)) return true;
      }
      return false;
    };
    for (const sheet of document.styleSheets) {
      try {
        if (scan(sheet.cssRules)) sheetsAnimated = true;
      } catch {
        sheetsAnimated = true;
      }
    }
    return sheetsAnimated;
  };

  const blocked = () => {
    if (document.readyState !== 'complete') return 'loading';
    if (document.fonts && document.fonts.status !== 'loaded') return 'fonts';
    if (document.querySelector(BLOCKERS)) return 'element';
    for (const image of document.images) {
      if (!image.complete || ANIMATED_IMAGE.test(image.currentSrc || image.src)) return 'image';
    }
    for (const image of document.querySelectorAll('image')) {
      if (ANIMATED_IMAGE.test(image.getAttribute('href') ?? image.getAttribute('xlink:href') ?? '')) return 'image';
    }
    for (const element of document.querySelectorAll('[style*="url("]')) {
      if (ANIMATED_IMAGE.test(element.getAttribute('style'))) return 'image';
    }
    if (animatedSheets()) return 'stylesheet';
    return null;
  };

  const settled = () => {
    for (const animation of document.getAnimations()) {
      if (animation.playState !== 'finished' && animation.playState !== 'idle') return false;
    }
    return true;
  };

  const take = () => {
    note(observer.takeRecords());
    let reason = null;
    if (structural) reason = 'structure';
    else if (hidden !== seenHidden) reason = 'unobserved';
    else {
      for (const [target, old] of changes) {
        for (const [key, was] of old) {
          const now = key === '#data' ? target.data : target.getAttribute(key);
          if (now !== was) reason = key;
        }
      }
    }
    structural = false;
    seenHidden = hidden;
    changes.clear();
    if (reason) return reason;
    if (!settled()) return 'animation';
    return blocked();
  };

  let seenHidden = 0;
  (globalThis.__vrf ??= {}).track = { take };
})();
