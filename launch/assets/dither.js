window.StrobeDither = (() => {
  const RAMP = ["#04060D", "#0B2A6B", "#2F7BFF", "#8FE3FF", "#F4FBFF"].map((h) => [
    parseInt(h.slice(1, 3), 16),
    parseInt(h.slice(3, 5), 16),
    parseInt(h.slice(5, 7), 16),
  ]);
  const B4 = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
  const BOLT = [
    [0.58, 0.0],
    [0.18, 0.56],
    [0.46, 0.56],
    [0.34, 1.0],
    [0.84, 0.38],
    [0.55, 0.38],
    [0.74, 0.0],
  ];
  const work = document.createElement("canvas");
  const wctx = work.getContext("2d", { willReadFrequently: true });

  function boltPath(c, cx, cy, h) {
    c.beginPath();
    BOLT.forEach(([x, y], i) => {
      const px = cx + (x - 0.5) * h * 0.62;
      const py = cy + (y - 0.5) * h;
      i ? c.lineTo(px, py) : c.moveTo(px, py);
    });
    c.closePath();
  }

  function light(c, v) {
    const g = Math.round(Math.max(0, Math.min(1, v)) * 255);
    c.fillStyle = `rgb(${g},${g},${g})`;
  }

  function bolt(c, k, { cx, cy, h, energy = 1, reveal = 1 }) {
    c.save();
    c.globalCompositeOperation = "lighter";
    const top = cy - h / 2;
    c.beginPath();
    c.rect(0, 0, 1e5, (top + h * reveal + h * 0.25 * (reveal > 0 ? 1 : 0)) * k);
    c.clip();
    c.filter = `blur(${h * 0.32 * k}px)`;
    light(c, 0.42 * energy);
    boltPath(c, cx * k, cy * k, h * k);
    c.fill();
    c.filter = `blur(${h * 0.07 * k}px)`;
    light(c, 0.5 * energy);
    boltPath(c, cx * k, cy * k, h * k);
    c.fill();
    c.filter = "none";
    c.restore();
    c.save();
    c.beginPath();
    c.rect(0, 0, 1e5, (top + h * reveal) * k);
    c.clip();
    light(c, 0.78 + 0.22 * energy);
    boltPath(c, cx * k, cy * k, h * k * 0.94);
    c.fill();
    c.restore();
  }

  function glowText(c, k, { text, x, y, size, font, energy = 1 }) {
    c.save();
    c.globalCompositeOperation = "lighter";
    c.font = `${font} ${size * k}px Inter`;
    c.textAlign = "center";
    c.textBaseline = "middle";
    c.filter = `blur(${size * 0.16 * k}px)`;
    light(c, 0.38 * energy);
    c.fillText(text, x * k, y * k);
    c.filter = "none";
    light(c, 0.62 + 0.3 * energy);
    c.fillText(text, x * k, y * k);
    c.restore();
  }

  function render(ctx, cell, paint, ambient = 0) {
    const W = ctx.canvas.width;
    const H = ctx.canvas.height;
    const w = Math.ceil(W / cell);
    const h = Math.ceil(H / cell);
    work.width = w;
    work.height = h;
    wctx.filter = "none";
    wctx.globalCompositeOperation = "source-over";
    light(wctx, ambient);
    wctx.fillRect(0, 0, w, h);
    paint(wctx, 1 / cell);
    const img = wctx.getImageData(0, 0, w, h);
    const d = img.data;
    const L = RAMP.length - 1;
    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        const i = (y * w + x) * 4;
        const v = (d[i] / 255) * L;
        const base = Math.floor(v);
        const t = (B4[(y & 3) * 4 + (x & 3)] + 0.5) / 16;
        const c = RAMP[Math.min(L, base + (v - base > t ? 1 : 0))];
        d[i] = c[0];
        d[i + 1] = c[1];
        d[i + 2] = c[2];
      }
    }
    wctx.putImageData(img, 0, 0);
    ctx.imageSmoothingEnabled = false;
    ctx.drawImage(work, 0, 0, w * cell, h * cell);
  }

  return { render, bolt, glowText };
})();
