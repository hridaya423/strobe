(() => {
  const W = 1920, H = 1080;
  const HEX = { night: "#04060D", deep: "#0B2A6B", electric: "#2F7BFF", ice: "#8FE3FF", core: "#F4FBFF" };
  const rgb = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
  const toLin = (c) => {
    const s = c / 255;
    return s <= 0.04045 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
  };
  const RAMP = ["night", "deep", "electric", "ice", "core"].map((k) => {
    const c = rgb(HEX[k]);
    return { c, L: 0.2126 * toLin(c[0]) + 0.7152 * toLin(c[1]) + 0.0722 * toLin(c[2]) };
  });

  const BN = Float32Array.from(window.STROBE_BLUE_NOISE, (r) => (r + 0.5) / 4096);

  const LUT = new Uint8Array(256 * 64 * 3);
  for (let v = 0; v < 256; v++) {
    const I = RAMP[0].L + Math.pow(v / 255, 2.2) * (RAMP[4].L - RAMP[0].L);
    let a = 0;
    while (a < 3 && I > RAMP[a + 1].L) a++;
    const frac = Math.min(1, Math.max(0, (I - RAMP[a].L) / (RAMP[a + 1].L - RAMP[a].L)));
    for (let n = 0; n < 64; n++) {
      const c = RAMP[frac > (n + 0.5) / 64 ? a + 1 : a].c;
      LUT.set(c, (v * 64 + n) * 3);
    }
  }

  const clamp = (x, a = 0, b = 1) => Math.min(b, Math.max(a, x));
  const lerp = (a, b, t) => a + (b - a) * t;
  const seg = (t, a, b) => clamp((t - a) / (b - a));
  const E = {
    lin: (x) => x,
    qo: (x) => 1 - (1 - x) * (1 - x),
    co: (x) => 1 - Math.pow(1 - x, 3),
    ci: (x) => x * x * x,
    cio: (x) => (x < 0.5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2),
    eo: (x) => (x >= 1 ? 1 : 1 - Math.pow(2, -10 * x)),
    ei: (x) => (x <= 0 ? 0 : Math.pow(2, 10 * x - 10)),
    eio: (x) => (x <= 0 ? 0 : x >= 1 ? 1 : x < 0.5 ? Math.pow(2, 20 * x - 10) / 2 : (2 - Math.pow(2, -20 * x + 10)) / 2),
    bo: (x) => 1 + 2.70158 * Math.pow(x - 1, 3) + 1.70158 * Math.pow(x - 1, 2),
  };
  const tw = (t, a, b, from, to, ease = E.eio) => lerp(from, to, ease(seg(t, a, b)));
  function hash(a, b = 0) {
    let h = Math.imul(a | 0, 374761393) + Math.imul(b | 0, 668265263);
    h = Math.imul(h ^ (h >>> 13), 1274126177);
    h ^= h >>> 16;
    return (h >>> 0) / 4294967296;
  }

  const field = document.createElement("canvas");
  const fctx = field.getContext("2d", { willReadFrequently: true });

  function dither(ctx, cell, paint) {
    const w = Math.ceil(W / cell), h = Math.ceil(H / cell);
    if (field.width !== w || field.height !== h) {
      field.width = w;
      field.height = h;
    }
    fctx.setTransform(1, 0, 0, 1, 0, 0);
    fctx.globalCompositeOperation = "source-over";
    fctx.filter = "none";
    fctx.globalAlpha = 1;
    fctx.fillStyle = "#000";
    fctx.fillRect(0, 0, w, h);
    paint(fctx, 1 / cell);
    fctx.setTransform(1, 0, 0, 1, 0, 0);
    fctx.filter = "none";
    const img = fctx.getImageData(0, 0, w, h);
    const d = img.data;
    for (let y = 0; y < h; y++) {
      const by = (y & 63) << 6;
      for (let x = 0; x < w; x++) {
        const i = (y * w + x) << 2;
        const n = Math.min(63, (BN[by | (x & 63)] * 64) | 0);
        const o = (d[i] * 64 + n) * 3;
        d[i] = LUT[o];
        d[i + 1] = LUT[o + 1];
        d[i + 2] = LUT[o + 2];
        d[i + 3] = 255;
      }
    }
    fctx.putImageData(img, 0, 0);
    ctx.save();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.imageSmoothingEnabled = false;
    ctx.drawImage(field, 0, 0, w * cell, h * cell);
    ctx.restore();
  }

  function light(c, v) {
    const g = Math.round(Math.pow(clamp(v), 1 / 2.2) * 255);
    c.fillStyle = `rgb(${g},${g},${g})`;
    c.strokeStyle = c.fillStyle;
  }

  const BOLT = [
    [0.58, 0.0], [0.16, 0.57], [0.46, 0.57], [0.33, 1.0], [0.86, 0.38], [0.55, 0.38], [0.75, 0.0],
  ];
  function boltPath(c, cx, cy, h) {
    c.beginPath();
    BOLT.forEach(([x, y], i) => {
      const px = cx + (x - 0.5) * h * 0.62, py = cy + (y - 0.5) * h;
      i ? c.lineTo(px, py) : c.moveTo(px, py);
    });
    c.closePath();
  }

  const probe = document.createElement("canvas");
  const pctx = probe.getContext("2d", { willReadFrequently: true });
  const boltCache = new Map();
  function boltCells(cx, cy, h, p) {
    const key = [cx, cy, h, p].join(",");
    if (boltCache.has(key)) return boltCache.get(key);
    probe.width = W;
    probe.height = H;
    pctx.clearRect(0, 0, W, H);
    pctx.fillStyle = "#fff";
    boltPath(pctx, cx, cy, h);
    pctx.fill();
    const d = pctx.getImageData(0, 0, W, H).data;
    const out = [];
    const gx = cx - Math.round(cx / p) * p;
    const gy = cy - Math.round(cy / p) * p;
    for (let y = gy + p / 2; y < H; y += p)
      for (let x = gx + p / 2; x < W; x += p) {
        const xi = Math.round(x), yi = Math.round(y);
        if (xi >= 0 && yi >= 0 && xi < W && yi < H && d[(yi * W + xi) * 4 + 3] > 127) out.push({ x, y });
      }
    boltCache.set(key, out);
    return out;
  }

  const textCache = new Map();
  function textCells(text, font, cx, cy, p, spacing = "0px") {
    const key = [text, font, cx, cy, p, spacing].join("|");
    if (textCache.has(key)) return textCache.get(key);
    probe.width = W;
    probe.height = H;
    pctx.clearRect(0, 0, W, H);
    pctx.font = font;
    pctx.letterSpacing = spacing;
    pctx.textAlign = "center";
    pctx.textBaseline = "middle";
    pctx.fillStyle = "#fff";
    pctx.fillText(text, cx, cy);
    pctx.letterSpacing = "0px";
    const d = pctx.getImageData(0, 0, W, H).data;
    const out = [];
    for (let y = p / 2; y < H; y += p)
      for (let x = p / 2; x < W; x += p) {
        const xi = Math.round(x), yi = Math.round(y);
        if (d[(yi * W + xi) * 4 + 3] > 127) out.push({ x, y });
      }
    textCache.set(key, out);
    return out;
  }

  function cellsOf(key, draw, p) {
    if (textCache.has(key)) return textCache.get(key);
    probe.width = W;
    probe.height = H;
    pctx.setTransform(1, 0, 0, 1, 0, 0);
    pctx.clearRect(0, 0, W, H);
    pctx.fillStyle = "#fff";
    draw(pctx);
    pctx.setTransform(1, 0, 0, 1, 0, 0);
    pctx.letterSpacing = "0px";
    const d = pctx.getImageData(0, 0, W, H).data;
    const out = [];
    for (let y = p / 2; y < H; y += p)
      for (let x = p / 2; x < W; x += p) {
        const xi = Math.round(x), yi = Math.round(y);
        if (d[(yi * W + xi) * 4 + 3] > 127) out.push({ x, y });
      }
    textCache.set(key, out);
    return out;
  }

  const U = 10, ADV = 7;
  const glyphCache = new Map();
  function glyphs(text) {
    if (glyphCache.has(text)) return glyphCache.get(text);
    probe.width = (text.length * ADV + 2) * U;
    probe.height = 15 * U;
    pctx.clearRect(0, 0, probe.width, probe.height);
    pctx.font = `${11 * U}px "Departure Mono"`;
    pctx.textAlign = "left";
    pctx.textBaseline = "alphabetic";
    pctx.fillStyle = "#000";
    pctx.fillText(text, 0, 11 * U);
    const d = pctx.getImageData(0, 0, probe.width, probe.height).data;
    const out = [];
    for (let r = 0; r < 15; r++)
      for (let c = 0; c < text.length * ADV; c++)
        if (d[((r * U + 5) * probe.width + c * U + 5) * 4 + 3] > 127) out.push({ c, r, i: Math.floor(c / ADV) });
    glyphCache.set(text, out);
    return out;
  }

  function pixelText(text, { p, cx, cy, x }) {
    const width = (text.length * ADV - 1) * p;
    const x0 = x !== undefined ? x : cx - width / 2;
    const y0 = cy - 7 * p;
    return { width, x0, cells: glyphs(text).map((k) => ({ x: x0 + k.c * p + p / 2, y: y0 + k.r * p + p / 2, c: k.c, r: k.r, i: k.i })) };
  }

  function squares(ctx, cells, size, color) {
    ctx.fillStyle = color;
    for (const q of cells) {
      const s = q.s === undefined ? size : size * q.s;
      if (s > 0.2) ctx.fillRect(q.x - s / 2, q.y - s / 2, s, s);
    }
  }

  window.SX = { W, H, HEX, clamp, lerp, seg, E, tw, hash, dither, light, boltPath, boltCells, textCells, cellsOf, pixelText, glyphs, squares, ADV };
})();
