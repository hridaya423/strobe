window.SX_BUILD = new Promise((resolve) => {
  const start = () => resolve(build());
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
  else start();
});

function build() {
  return Promise.all([
    document.fonts.load('110px "Departure Mono"'),
    document.fonts.load('800 120px "Geist"'),
    document.fonts.load('600 56px "Geist"'),
    document.fonts.load('italic 400 120px "Instrument Serif"'),
    ...[...document.querySelectorAll("#stills img")].map((i) => i.decode().catch(() => {})),
  ]).then(setup);
}

function setup() {
  const { W, H, HEX, clamp, lerp, seg, E, tw, hash, dither, light, boltPath, boltCells, cellsOf, pixelText, squares } = SX;
  const ctx = document.getElementById("film").getContext("2d");
  const BEAT = 0.4;
  const b = (n) => n * BEAT;
  const MONO = '"Departure Mono", monospace';
  const GEIST = (w, s) => `${w} ${s}px "Geist"`;
  const SERIF = (s) => `italic 400 ${s}px "Instrument Serif"`;
  const core = (a) => `rgba(244,251,255,${a})`;
  const decay = (t, t0, k) => (t >= t0 ? Math.exp(-k * (t - t0)) : 0);

  const stills = [...document.querySelectorAll("#stills img")].map((img) => {
    const c = document.createElement("canvas");
    c.width = img.naturalWidth;
    c.height = img.naturalHeight;
    const x = c.getContext("2d", { willReadFrequently: true });
    x.drawImage(img, 0, 0);
    const d = x.getImageData(0, 0, c.width, c.height);
    for (let i = 0; i < d.data.length; i += 4) d.data[i] = d.data[i + 1] = d.data[i + 2] = Math.round(Math.pow(d.data[i] / 255, 1.1) * 255);
    x.putImageData(d, 0, 0);
    return c;
  });

  function camera(c, k, cam) {
    c.setTransform(k, 0, 0, k, 0, 0);
    c.translate(W / 2 + (cam.ox || 0), H / 2 + (cam.oy || 0));
    if (cam.r) c.rotate(cam.r);
    c.scale(cam.s, cam.s);
    c.translate(-cam.fx, -cam.fy);
  }

  function grid(cam, color, step = 120) {
    ctx.save();
    camera(ctx, 1, cam);
    ctx.strokeStyle = color;
    ctx.lineWidth = 1 / cam.s;
    const R = Math.hypot(W, H) / cam.s;
    const x0 = cam.fx - R, x1 = cam.fx + R, y0 = cam.fy - R, y1 = cam.fy + R;
    ctx.beginPath();
    for (let x = Math.floor(x0 / step) * step; x <= x1; x += step) {
      ctx.moveTo(x + 0.5, y0);
      ctx.lineTo(x + 0.5, y1);
    }
    for (let y = Math.floor(y0 / step) * step; y <= y1; y += step) {
      ctx.moveTo(x0, y + 0.5);
      ctx.lineTo(x1, y + 0.5);
    }
    ctx.stroke();
    ctx.restore();
  }

  function label(text, x, y, color, { size = 28, align = "left", a = 1 } = {}) {
    ctx.globalAlpha = a;
    ctx.font = `${size}px ${MONO}`;
    ctx.textAlign = align;
    ctx.textBaseline = "middle";
    ctx.fillStyle = color;
    ctx.fillText(text, x, y);
    ctx.globalAlpha = 1;
    ctx.textAlign = "left";
  }

  const SCRAMBLE = "#%&*+=/<>?@[]{}~ABCDEFGHJKLMNPRSTUVXYZ0123456789";
  const scramble = (text, t, t0, dur, seed) =>
    [...text].map((ch, i) => (ch === " " || t >= t0 + dur * ((i + 1) / text.length) ? ch : SCRAMBLE[Math.floor(hash(i + Math.floor(t * 60), seed) * SCRAMBLE.length)])).join("");

  function layout(text, font, spacing, x0) {
    ctx.font = font;
    ctx.letterSpacing = spacing;
    const out = [...text].map((ch, i) => ({ ch, i, x: x0 + ctx.measureText(text.slice(0, i)).width, w: ctx.measureText(ch).width }));
    const width = ctx.measureText(text).width;
    ctx.letterSpacing = "0px";
    return { letters: out, width };
  }

  function drawLetters(L, font, spacing, y, color, each) {
    ctx.font = font;
    ctx.letterSpacing = "0px";
    ctx.textBaseline = "alphabetic";
    for (const q of L.letters) {
      const st = each(q);
      if (!st || st.a <= 0) continue;
      ctx.save();
      ctx.globalAlpha = st.a ?? 1;
      ctx.translate(q.x + q.w / 2 + (st.dx || 0), y + (st.dy || 0));
      if (st.r) ctx.rotate(st.r);
      if (st.s !== undefined) ctx.scale(st.sx ?? st.s, st.s);
      if (st.blur) ctx.filter = `blur(${st.blur}px)`;
      ctx.fillStyle = st.color || color;
      ctx.fillText(q.ch, -q.w / 2, 0);
      ctx.restore();
    }
  }

  const WM = "strobe";
  function wordmarkLayout(size, cx) {
    const font = SERIF(size);
    const L = layout(WM, font, `${-0.01 * size}px`, 0);
    const shift = cx - L.width / 2;
    for (const q of L.letters) q.x += shift;
    ctx.font = font;
    const o = L.letters[3];
    const oh = ctx.measureText("o").actualBoundingBoxAscent;
    return { L, font, size, o: { x: o.x + o.w * 0.5, h: oh }, x0: shift, width: L.width };
  }

  function boltInO(c, M, y, { bg, fill, scale = 1, a = 1 }) {
    if (a <= 0) return;
    const cx = M.o.x, cy = y - M.o.h * 0.5;
    c.save();
    c.globalAlpha = a;
    c.fillStyle = bg;
    boltPath(c, cx, cy, M.o.h * 1.55 * scale);
    c.fill();
    c.fillStyle = fill;
    boltPath(c, cx, cy, M.o.h * 1.05 * scale);
    c.fill();
    c.restore();
  }

  const LINE_Y = 520;
  const W1 = layout("Rendering", GEIST(800, 132), "-6px", 0);
  const W2 = layout("takes", GEIST(800, 132), "-6px", 0);
  const W3 = layout("forever.", SERIF(168), "0px", 0);
  const GAP = 44;
  const LINE_W = W1.width + W2.width + W3.width + GAP * 2;
  const X1 = W / 2 - LINE_W / 2, X2 = X1 + W1.width + GAP, X3 = X2 + W2.width + GAP;
  for (const q of W1.letters) q.x += X1;
  for (const q of W2.letters) q.x += X2;
  for (const q of W3.letters) q.x += X3;
  const T_STRIKE = b(4);
  const stretch = (i, t) => i * 30 * E.qo(seg(t, b(3.1), T_STRIKE));
  const FOREVER_CX = X3 + W3.width / 2 + 3.5 * 30;
  const SHARDS = cellsOf(
    "shards",
    (c) => {
      c.font = SERIF(168);
      c.textBaseline = "alphabetic";
      for (const q of W3.letters) c.fillText(q.ch, q.x + stretch(q.i, T_STRIKE), LINE_Y);
    },
    9,
  );
  const STRIKE = [[1580, -40], [1430, 230], [1510, 300], [FOREVER_CX + 30, LINE_Y - 70], [FOREVER_CX + 95, LINE_Y - 10], [1230, 820], [1300, 860], [1150, 1140]];

  function shardPos(q, n, t) {
    const dt = Math.max(0, t - T_STRIKE);
    const dx = q.x - (FOREVER_CX + 60), dy = q.y - (LINE_Y - 40);
    const sp = 900 + 1600 * hash(n, 3);
    const ang = Math.atan2(dy, dx) + (hash(n, 5) - 0.5) * 1.2;
    const k = 4.2;
    const travel = (sp * (1 - Math.exp(-k * dt))) / k;
    return { x: q.x + Math.cos(ang) * travel, y: q.y + Math.sin(ang) * travel + 40 * dt * dt * (hash(n, 9) - 0.3) };
  }

  const BOLT_C = { x: 960, y: 540, h: 640 };
  const BOLT12 = boltCells(BOLT_C.x, BOLT_C.y, BOLT_C.h, 12);
  const T_GATHER = b(4.75);
  const PAIRS = (() => {
    const src = SHARDS.map((q, n) => ({ q, n }));
    const n = Math.max(src.length, BOLT12.length);
    const s = [...src].sort((a, c) => a.q.x + a.q.y * 0.2 - (c.q.x + c.q.y * 0.2));
    const tt = [...BOLT12].sort((a, c) => a.x + a.y * 0.2 - (c.x + c.y * 0.2));
    return Array.from({ length: n }, (_, k) => ({ from: s[Math.floor((k * s.length) / n)], to: tt[Math.floor((k * tt.length) / n)], k }));
  })();

  function boltGlow(c, k, cx, cy, h, energy) {
    c.save();
    c.globalCompositeOperation = "lighter";
    c.filter = `blur(${h * 0.3 * k}px)`;
    light(c, 0.55 * energy);
    boltPath(c, cx * k, cy * k, h * k);
    c.fill();
    c.filter = `blur(${h * 0.06 * k}px)`;
    light(c, 0.7 * energy);
    boltPath(c, cx * k, cy * k, h * k);
    c.fill();
    c.filter = "none";
    light(c, 0.55 + 0.45 * Math.min(1, energy));
    boltPath(c, cx * k, cy * k, h * k * 0.97);
    c.fill();
    c.restore();
  }

  const LABELS = ["HTML", "CSS", "SVG", "<canvas>", "WebGL", "GSAP", "<video>", "Three.js", "Lottie", "Remotion", "HyperFrames", "Web Audio"];
  const LABEL_POS = [[-600, -260], [560, -280], [-740, -40], [720, -110], [-660, 190], [640, 140], [-400, 330], [440, 320], [-220, -360], [220, -370], [0, 390], [790, 280]];
  const BRAND = wordmarkLayout(330, W / 2);
  const BRAND_Y = H / 2 + 100;

  const NUM_P = 60;
  const NUM_Y = 400;

  const CUTS = [];
  {
    let t = b(18), n = 0;
    while (t < b(22) - 1e-6) {
      const dur = t < b(21) ? 0.1 : 0.05;
      CUTS.push({ t0: t, t1: t + dur, n, layout: [0, 1, 0, 2, 0, 3, 1, 0][n % 8], dir: hash(n, 77) < 0.5 ? -1 : 1 });
      t += dur;
      n++;
    }
  }

  function drawStill(c, k, idx, x, y, w, h, zoom, dx = 0) {
    const img = stills[idx % stills.length];
    c.save();
    c.beginPath();
    c.rect(x * k, y * k, w * k, h * k);
    c.clip();
    const zw = w * zoom, zh = h * zoom;
    c.drawImage(img, (x - (zw - w) / 2 + dx) * k, (y - (zh - h) / 2) * k, zw * k, zh * k);
    c.restore();
  }

  const LOGO = wordmarkLayout(300, W / 2);
  const glint = document.createElement("canvas");
  glint.width = W;
  glint.height = H;
  const LOGO_Y = H / 2 + 40;

  function render(t) {
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalAlpha = 1;
    ctx.filter = "none";
    if (t < b(8)) sceneOpen(t);
    else if (t < b(12)) sceneBrand(t);
    else if (t < b(18)) sceneNumbers(t);
    else if (t < b(22)) sceneMontage(t);
    else sceneLogo(t);
  }

  function sceneOpen(t) {
    const pull = E.eo(seg(t, 0, 0.7));
    const shake = t >= T_STRIKE && t < T_STRIKE + 0.2 ? Math.sin(t * 190) * 16 * (1 - seg(t, T_STRIKE, T_STRIKE + 0.2)) : 0;
    const assemble = t >= T_GATHER;
    const dive = E.ei(seg(t, b(7.35), b(8)));
    const cam = assemble
      ? { s: tw(t, T_GATHER, b(7.35), 0.9, 1.08, E.lin) * (1 + dive * 15), fx: BOLT_C.x, fy: BOLT_C.y + 30 * dive, r: tw(t, T_GATHER, b(6.6), 0.16, 0, E.eo) - 0.02 * dive }
      : {
          s: lerp(3.6, 1, pull) * tw(t, 0.7, T_STRIKE, 1, 1.13, E.lin),
          fx: lerp(X1 + 260, W / 2, pull) + tw(t, 0.7, T_STRIKE, 0, 40, E.lin),
          fy: LINE_Y - 40,
          r: lerp(-0.07, 0, pull) + tw(t, 0.7, T_STRIKE, 0, 0.012, E.lin),
          ox: shake,
          oy: shake * 0.4,
        };

    const glowOn = E.eo(seg(t, b(6), b(6.8)));
    const res = t < b(7) ? 12 : t < b(7.5) ? 6 : 3;
    dither(ctx, 3, (c, k) => {
      if (t >= T_STRIKE && t < T_STRIKE + 0.16) {
        light(c, 0.035 * Math.pow(1 - seg(t, T_STRIKE, T_STRIKE + 0.16), 2));
        c.fillRect(0, 0, W * k, H * k);
      }
      if (!assemble) return;
      camera(c, k, cam);
      if (glowOn > 0) boltGlow(c, 1, BOLT_C.x, BOLT_C.y, BOLT_C.h, glowOn * (1 + 0.6 * dive));
      if (dive > 0) {
        c.setTransform(1, 0, 0, 1, 0, 0);
        light(c, dive * dive);
        c.globalCompositeOperation = "lighter";
        c.fillRect(0, 0, W * k, H * k);
      }
    });
    grid(cam, "rgba(47,123,255,0.10)");

    ctx.save();
    camera(ctx, 1, cam);
    if (!assemble) {
      const knock = E.eo(seg(t, T_STRIKE, T_STRIKE + 0.35));
      drawLetters(W1, GEIST(800, 132), "0px", LINE_Y, HEX.core, (q) => {
        const d = 0.012 * q.i;
        const u = E.eo(seg(t, d, d + 0.22));
        return { a: u * (1 - knock), dx: (1 - u) * 90 - knock * 320, blur: (1 - u) * 10 };
      });
      drawLetters(W2, GEIST(800, 132), "0px", LINE_Y, HEX.ice, (q) => {
        const d = b(2) + 0.017 * q.i;
        const u = E.eo(seg(t, d, d + 0.3));
        return { a: Math.min(1, u * 3) * (1 - knock), dy: -(1 - u) * 120 - knock * 40, dx: -knock * 320, r: (1 - u) * -0.25 };
      });
      if (t < T_STRIKE) {
        drawLetters(W3, SERIF(168), "0px", LINE_Y, HEX.core, (q) => {
          const d = b(3) + 0.025 * q.i;
          const u = E.eo(seg(t, d, d + 0.25));
          return { a: u, dx: stretch(q.i, t), blur: (1 - u) * 14, s: 1 + 0.25 * (1 - u) };
        });
      }

      const barA = E.eo(seg(t, 0.35, 0.6)) * (1 - knock);
      if (barA > 0) {
        const bx = W / 2 - 440, by = LINE_Y + 120, bw = 880, sx = -knock * 320;
        ctx.globalAlpha = barA;
        ctx.fillStyle = "rgba(47,123,255,0.28)";
        ctx.fillRect(bx + sx, by, bw, 4);
        ctx.fillStyle = HEX.ice;
        ctx.fillRect(bx + sx, by, bw * (0.02 + 0.05 * seg(t, 0.35, T_STRIKE)), 4);
        const frame = 112 + Math.floor(seg(t, 0.35, T_STRIKE) * 19);
        label(`frame ${String(frame).padStart(4, "0")} / 1692`, bx + sx, by + 40, HEX.electric, { a: barA });
        label(`eta 00:${String(44 - Math.floor(seg(t, 0.35, T_STRIKE) * 2)).padStart(2, "0")}`, bx + bw + sx, by + 40, HEX.electric, { align: "right", a: barA });
        ctx.globalAlpha = 1;
      }

      if (t >= T_STRIKE) {
        ctx.globalAlpha = Math.max(0.35, 1 - seg(t, T_STRIKE + 0.25, T_GATHER));
        squares(ctx, SHARDS.map((q, n) => shardPos(q, n, t)), 8, HEX.core);
        ctx.globalAlpha = 1;
      }

      if (t >= T_STRIKE && t < T_STRIKE + 0.2) {
        const p = seg(t, T_STRIKE, T_STRIKE + 0.05);
        const fade = 1 - seg(t, T_STRIKE + 0.08, T_STRIKE + 0.2);
        const lens = [];
        let total = 0;
        for (let i = 1; i < STRIKE.length; i++) {
          lens.push(Math.hypot(STRIKE[i][0] - STRIKE[i - 1][0], STRIKE[i][1] - STRIKE[i - 1][1]));
          total += lens[i - 1];
        }
        let left = total * p;
        ctx.beginPath();
        ctx.moveTo(...STRIKE[0]);
        for (let i = 1; i < STRIKE.length && left > 0; i++) {
          const u = Math.min(1, left / lens[i - 1]);
          ctx.lineTo(lerp(STRIKE[i - 1][0], STRIKE[i][0], u), lerp(STRIKE[i - 1][1], STRIKE[i][1], u));
          left -= lens[i - 1];
        }
        ctx.lineJoin = "miter";
        ctx.lineCap = "square";
        ctx.globalAlpha = fade;
        ctx.strokeStyle = HEX.ice;
        ctx.lineWidth = 34;
        ctx.stroke();
        ctx.strokeStyle = HEX.core;
        ctx.lineWidth = 12;
        ctx.stroke();
        ctx.globalAlpha = 1;
      }
    } else {
      const landed = [], flying = [];
      for (const pr of PAIRS) {
        const d = T_GATHER + 0.28 * (pr.k / PAIRS.length) + 0.06 * hash(pr.k, 2);
        const u = E.eio(seg(t, d, d + 0.42));
        const from = shardPos(pr.from.q, pr.from.n, T_GATHER);
        const bend = Math.sin(Math.PI * u) * 160 * (hash(pr.k, 7) - 0.5);
        (u >= 1 ? landed : flying).push({ x: lerp(from.x, pr.to.x, u) + bend, y: lerp(from.y, pr.to.y, u) - Math.abs(bend) * 0.4 });
      }
      squares(ctx, flying, 8, HEX.ice);
      if (res === 12) squares(ctx, landed, 10.5, HEX.core);
      else squares(ctx, boltCells(BOLT_C.x, BOLT_C.y, BOLT_C.h, res), res <= 3 ? res : res * 0.88, HEX.core);
    }
    ctx.restore();

    if (t >= T_STRIKE && t < T_STRIKE + 0.05) {
      ctx.fillStyle = core(t < T_STRIKE + 0.017 ? 0.9 : 0.45);
      ctx.fillRect(0, 0, W, H);
    }
  }

  function brandMark(t, bloom, boltT) {
    const ECHO = [[4, "#D6F2FF"], [3, HEX.ice], [2, HEX.electric], [1, HEX.deep]];
    if (bloom > 0.001) {
      ctx.save();
      ctx.beginPath();
      ctx.rect(-W, -H, W * 3, BRAND_Y - BRAND.size * 0.72 + H);
      ctx.rect(-W, BRAND_Y + BRAND.size * 0.2, W * 3, H * 2);
      ctx.clip();
      for (const [k, col] of ECHO)
        for (const sgn of [-1, 1]) {
          const y = BRAND_Y + sgn * k * 104 * bloom;
          drawLetters(BRAND.L, BRAND.font, "0px", y, col, () => ({ a: 1 }));
          boltInO(ctx, BRAND, y, { bg: HEX.core, fill: col, a: t >= boltT ? 1 : 0 });
        }
      ctx.restore();
    }
    drawLetters(BRAND.L, BRAND.font, "0px", BRAND_Y, HEX.night, (q) => {
      const d = b(8) + 0.035 * q.i;
      const u = E.eo(seg(t, d, d + 0.35));
      return { a: Math.min(1, u * 2.5), dy: (1 - u) * 160, blur: (1 - u) * 18, s: 1 + 0.35 * (1 - u), r: (1 - u) * 0.12 };
    });
    if (t >= boltT) {
      const u = E.eo(seg(t, boltT, boltT + 0.22));
      boltInO(ctx, BRAND, BRAND_Y, { bg: HEX.core, fill: t < boltT + 0.05 ? HEX.ice : HEX.electric, scale: lerp(2.6, 1, u) });
    }
  }

  function sceneBrand(t) {
    const t0 = b(8);
    const arrive = E.eo(seg(t, t0, t0 + 0.6));
    const whip = E.ei(seg(t, b(11), b(11.85)));
    const boltT = b(8.75);
    const kick = decay(t, boltT, 12);
    const cam = {
      s: lerp(1.7, 1, arrive) * tw(t, t0 + 0.6, b(11), 1, 1.05, E.lin) * (1 + 0.04 * kick),
      fx: W / 2 + whip * 2600,
      fy: H / 2,
      r: lerp(0.06, 0, arrive) - 0.01 * seg(t, t0 + 0.6, b(11)),
      ox: Math.sin(t * 160) * 10 * kick,
    };

    ctx.fillStyle = HEX.core;
    ctx.fillRect(0, 0, W, H);
    grid(cam, "rgba(11,42,107,0.10)");

    ctx.save();
    camera(ctx, 1, cam);
    const bloom = E.eo(seg(t, b(8.9), b(9.6))) * (1 - E.ei(seg(t, b(10.2), b(10.8))));
    brandMark(t, bloom, boltT);

    LABELS.forEach((txt, i) => {
      const on = b(9.2) + i * 0.034;
      const off = b(10.1) + (LABELS.length - i) * 0.017;
      if (t < on || t >= off) return;
      const [dx, dy] = LABEL_POS[i];
      const x = W / 2 + dx, y = BRAND_Y - 100 + dy * Math.max(0.6, bloom);
      const fresh = t < on + 0.05;
      ctx.fillStyle = fresh ? HEX.electric : HEX.night;
      ctx.fillRect(x - 6, y - 6, 12, 12);
      label(fresh ? scramble(txt, t, on, 0.08, i) : txt, x + 18, y, fresh ? HEX.electric : HEX.deep);
    });
    ctx.restore();

    if (t >= boltT && t < boltT + 0.034) {
      ctx.fillStyle = "rgba(47,123,255,0.18)";
      ctx.fillRect(0, 0, W, H);
    }

    const panel = E.ei(seg(t, b(11.1), b(12)));
    if (panel > 0) {
      const x = W - (W + 120) * panel;
      ctx.fillStyle = HEX.night;
      ctx.fillRect(x, 0, W - x + 10, H);
      ctx.save();
      ctx.beginPath();
      ctx.rect(x, 0, W - x, H);
      ctx.clip();
      numbersLayer(t);
      ctx.restore();
    }
  }

  function numbersLayer(t) {
    const enter = E.eo(seg(t, b(11.3), b(12.6)));
    const drift = Math.max(0, t - b(12.6)) * 70;
    const flipT = b(15);
    const push = tw(t, b(12.6), b(18), 1, 1.06, E.lin) * (1 + 0.04 * decay(t, flipT, 14));
    const cam = { s: push, fx: W / 2, fy: H / 2, ox: (1 - enter) * 2300 - drift, r: tw(t, b(12.6), b(18), -0.01, 0.012, E.lin) };
    const exitK = E.ei(seg(t, b(17.5), b(18)));

    const A = pixelText("3.9×", { p: NUM_P, cx: W / 2, cy: NUM_Y });
    const B = pixelText("3.1×", { p: NUM_P, cx: W / 2, cy: NUM_Y });
    const key = (q) => q.c + "," + q.r;
    const bKeys = new Set(B.cells.map(key));
    const aKeys = new Set(A.cells.map(key));
    let cells;
    if (t < flipT) cells = A.cells.map((q) => ({ ...q }));
    else {
      cells = [];
      for (const q of A.cells) {
        if (bKeys.has(key(q))) cells.push({ ...q });
        else {
          const d = flipT + 0.12 * ((q.x - A.x0) / A.width) + 0.03 * hash(q.c, q.r);
          const s = 1 - E.qo(seg(t, d, d + 0.06));
          if (s > 0) cells.push({ ...q, s });
        }
      }
      for (const q of B.cells) {
        if (aKeys.has(key(q))) continue;
        const d = flipT + 0.02 + 0.12 * ((q.x - A.x0) / A.width) + 0.03 * hash(q.c, q.r);
        const u = seg(t, d, d + 0.08);
        if (u > 0) cells.push({ ...q, s: E.bo(u), flash: t < d + 0.06 });
      }
    }
    for (const q of cells) {
      const wave = Math.sin((q.c * 0.6 + q.r * 0.3) - t * 9) * 6 * decay(t, b(12.55), 3);
      q.y += wave;
      if (exitK > 0) {
        q.x = lerp(q.x, W / 2, exitK * 0.85);
        q.y = lerp(q.y, H / 2, exitK * 0.85);
        q.s = (q.s ?? 1) * (1 - exitK * 0.5);
      }
    }

    const landed = E.eo(seg(t, b(12.4), b(12.9)));
    dither(ctx, 3, (c, k) => {
      camera(c, k, cam);
      c.globalCompositeOperation = "lighter";
      c.filter = `blur(${70 * k * cam.s}px)`;
      light(c, 0.5 + 0.35 * (1 - landed) + 0.3 * decay(t, flipT, 10));
      for (const q of cells) c.fillRect(q.x - NUM_P / 2, q.y - NUM_P / 2, NUM_P, NUM_P);
      c.filter = "none";
    });
    grid(cam, "rgba(47,123,255,0.10)");

    ctx.save();
    camera(ctx, 1, cam);
    const flashOn = t >= b(12.55) && t < b(12.55) + 0.06;
    for (const q of cells) {
      const s = NUM_P * 0.9 * (q.s ?? 1);
      ctx.fillStyle = q.flash || flashOn ? HEX.ice : HEX.core;
      ctx.fillRect(q.x - s / 2, q.y - s / 2, s, s);
    }
    ctx.restore();

    ctx.save();
    ctx.translate((1 - enter) * 2300, 0);
    const left = 130;
    const isHF = t >= flipT;
    const kick = isHF ? "MEAN OF 4 HYPERFRAMES LAUNCH VIDEOS" : "MEAN OF 3 REMOTION LAUNCH VIDEOS";
    if (t >= b(12.5)) label(scramble(kick, t, isHF ? flipT : b(12.5), 0.18, isHF ? 2 : 1), left + 20, 96, HEX.electric);

    const capY = 880;
    ctx.font = GEIST(600, 64);
    ctx.letterSpacing = "-2px";
    ctx.textBaseline = "alphabetic";
    const a1 = E.eo(seg(t, b(12.7), b(13)));
    ctx.globalAlpha = a1 * (1 - exitK);
    ctx.fillStyle = HEX.ice;
    ctx.fillText("faster than", left + 20, capY);
    const fw = ctx.measureText("faster than ").width;
    ctx.letterSpacing = "0px";
    ctx.save();
    ctx.beginPath();
    ctx.rect(left + 20 + fw - 10, capY - 90, 900, 120);
    ctx.clip();
    const roll = E.eo(seg(t, flipT, flipT + 0.24));
    ctx.font = SERIF(92);
    ctx.fillStyle = HEX.core;
    ctx.globalAlpha = E.eo(seg(t, b(12.85), b(13.15))) * (1 - exitK);
    if (!isHF || roll < 1) ctx.fillText("Remotion", left + 20 + fw, capY - roll * 120);
    if (isHF) ctx.fillText("HyperFrames", left + 20 + fw, capY + 120 - roll * 120);
    ctx.restore();
    ctx.globalAlpha = 1;

    const raceT = isHF ? b(15.1) : b(12.9);
    const mult = isHF ? 3.1 : 3.9;
    const rx = left + 1200, rw = 320;
    const p = seg(t, raceT, raceT + 0.3);
    const ra = E.eo(seg(t, b(12.8), b(13))) * (1 - exitK);
    if (ra > 0) {
      ctx.globalAlpha = ra;
      [["strobe", 1, HEX.core], [isHF ? "hyperframes" : "remotion", 1 / mult, HEX.electric]].forEach(([name, frac, col], i) => {
        const y = capY - 44 + i * 44;
        label(name, rx - 20, y, col, { align: "right" });
        ctx.fillStyle = "rgba(47,123,255,0.25)";
        ctx.fillRect(rx, y - 3, rw, 6);
        ctx.fillStyle = col;
        ctx.fillRect(rx, y - 3, rw * frac * p, 6);
      });
      ctx.globalAlpha = 1;
    }
    ctx.restore();
  }

  function sceneNumbers(t) {
    ctx.fillStyle = HEX.night;
    ctx.fillRect(0, 0, W, H);
    numbersLayer(t);
  }

  function sceneMontage(t) {
    const cut = CUTS.find((c) => t >= c.t0 && t < c.t1) || CUTS[CUTS.length - 1];
    const u = seg(t, cut.t0, cut.t1);
    const zoom = 1.1 - 0.1 * E.co(u);
    const slide = cut.dir * 60 * (1 - E.co(u));
    const n = cut.n;
    dither(ctx, cut.layout === 0 ? 3 : 2, (c, k) => {
      if (cut.layout === 0) drawStill(c, k, n * 3, 0, 0, W, H, zoom, slide);
      else {
        const cols = cut.layout + 1;
        const gap = 12;
        const cw = (W - gap * (cols + 1)) / cols, ch = (H - gap * (cols + 1)) / cols;
        for (let r = 0; r < cols; r++)
          for (let q = 0; q < cols; q++) drawStill(c, k, n * 5 + r * cols + q, gap + q * (cw + gap), gap + r * (ch + gap), cw, ch, zoom, slide * 0.5);
      }
    });

    const frame = Math.floor(lerp(0, 1692, seg(t, b(18), b(22))));
    const ctr = pixelText(String(frame).padStart(4, "0"), { p: 8, x: 96, cy: 120 });
    ctx.fillStyle = HEX.night;
    ctx.fillRect(80, 52, ctr.width + 32, 14 * 8 + 8);
    squares(ctx, ctr.cells, 7.2, HEX.core);
    label("FRAME", 96 + ctr.width + 40, 92, HEX.ice);
    ctx.fillStyle = HEX.night;
    ctx.fillRect(W - 360, H - 120, 280, 64);
    label("286 FPS", W - 104, H - 88, HEX.core, { align: "right", size: 40 });

    const first = t < b(20);
    const head = first ? "every frame," : "rendered.";
    const hT = first ? b(18) : b(20);
    ctx.font = SERIF(200);
    ctx.textBaseline = "alphabetic";
    const hw = ctx.measureText(head).width;
    const pop = E.eo(seg(t, hT, hT + 0.15));
    const hx = W / 2 - hw / 2, hy = H / 2 + 70;
    ctx.fillStyle = HEX.night;
    ctx.fillRect(hx - 40, hy - 170, (hw + 80) * pop, 230);
    ctx.save();
    ctx.beginPath();
    ctx.rect(hx - 40, hy - 170, (hw + 80) * pop, 230);
    ctx.clip();
    ctx.fillStyle = HEX.core;
    ctx.fillText(head, hx, hy);
    ctx.restore();
    if (t >= b(21.5) && Math.floor(t * 60) % 3 === 0) {
      ctx.fillStyle = core(0.18);
      ctx.fillRect(0, 0, W, H);
    }
  }

  function sceneLogo(t) {
    const t0 = b(22);
    ctx.fillStyle = HEX.night;
    ctx.fillRect(0, 0, W, H);

    if (t < t0 + 0.28) {
      const s = 1 - E.ei(seg(t, t0, t0 + 0.28));
      dither(ctx, 3, (c, k) => {
        c.setTransform(k, 0, 0, k, 0, 0);
        const w = Math.max(6, W * s), h = Math.max(6, H * s);
        drawStill(c, 1, CUTS[CUTS.length - 1].n * 3, W / 2 - w / 2, H / 2 - h / 2, w, h, 1);
      });
      return;
    }
    const tp = t0 + 0.28;
    const tb = b(23.5);
    if (t < tb) {
      const blink = [[0, 0.1], [0.16, 0.2], [0.3, 0.42], [0.5, 0.53]].some(([a, z]) => t - tp >= a && t - tp < z);
      if (blink || t - tp > 0.6) {
        ctx.fillStyle = HEX.core;
        ctx.fillRect(W / 2 - 5, H / 2 - 5, 10, 10);
      }
      return;
    }

    const STEPS = [128, 96, 64, 48, 32, 24, 16, 12, 8, 6, 4];
    const si = Math.min(STEPS.length - 1, Math.floor((t - tb) / (1.5 / 60)));
    const smooth = t >= tb + STEPS.length * (1.5 / 60) + 0.05;
    const flyT = b(25), landT = b(26);
    const fly = E.eio(seg(t, flyT, landT));
    const oy = LOGO_Y - LOGO.o.h * 0.5;
    const cx = lerp(W / 2, LOGO.o.x, fly), cy = lerp(H / 2, oy, fly), h = lerp(560, LOGO.o.h * 1.05, fly);
    const cam = { s: tw(t, landT, 12, 1, 1.05, E.lin) * (1 + 0.05 * decay(t, landT, 9)), fx: W / 2, fy: H / 2, r: tw(t, landT, 12, 0.012, 0, E.lin) };
    const glow = E.eo(seg(t, b(24), b(25))) * lerp(1, 0.55, fly);
    const pulse = 1 + 0.5 * decay(t, b(24), 8) + 0.7 * decay(t, landT, 6);

    dither(ctx, 3, (c, k) => {
      camera(c, k, cam);
      if (glow > 0) boltGlow(c, 1, cx, cy, h, glow * pulse);
    });

    ctx.save();
    camera(ctx, 1, cam);
    if (t >= landT) {
      drawLetters(LOGO.L, LOGO.font, "0px", LOGO_Y, HEX.core, (q) => {
        const dist = Math.abs(q.i - 3);
        if (q.i === 3) return { a: 1 };
        const d = landT + 0.04 * dist;
        const u = E.eo(seg(t, d, d + 0.4));
        const side = q.i < 3 ? 1 : -1;
        return { a: Math.min(1, u * 2), dx: side * (1 - u) * 90 * dist, blur: (1 - u) * 12, s: lerp(0.6, 1, u) };
      });
    } else if (fly > 0) {
      ctx.font = LOGO.font;
      ctx.textBaseline = "alphabetic";
      ctx.globalAlpha = E.eo(seg(t, flyT + 0.2, landT));
      const o = LOGO.L.letters[3];
      ctx.fillStyle = HEX.core;
      ctx.fillText("o", o.x, LOGO_Y);
      ctx.globalAlpha = 1;
    }
    if (!smooth) {
      const p = STEPS[si];
      squares(ctx, boltCells(Math.round(cx), Math.round(cy), Math.round(h), p), p * 0.94, HEX.core);
    } else if (t < landT) {
      ctx.fillStyle = HEX.core;
      boltPath(ctx, cx, cy, h * 0.97);
      ctx.fill();
    } else {
      boltInO(ctx, LOGO, LOGO_Y, { bg: HEX.night, fill: t < landT + 0.05 ? HEX.core : HEX.ice });
    }

    const tagT = b(26.6);
    const tag = "HTML to video, at the speed of light.";
    const nch = Math.floor(clamp((t - tagT) * 75, 0, tag.length));
    if (nch > 0) label(tag.slice(0, nch), W / 2, LOGO_Y + 120, HEX.ice, { size: 34, align: "center" });
    const cmd = "$ strobe render ./site";
    const cmdT = b(27.4);
    const ncmd = Math.floor(clamp((t - cmdT) * 45, 0, cmd.length));
    if (ncmd > 0) {
      ctx.font = `30px ${MONO}`;
      const full = ctx.measureText(cmd).width;
      const x0 = W / 2 - full / 2;
      label(cmd.slice(0, ncmd), x0, LOGO_Y + 180, HEX.electric, { size: 30 });
      ctx.font = `30px ${MONO}`;
      const cw = ctx.measureText(cmd.slice(0, ncmd)).width;
      if (ncmd < cmd.length || Math.floor(t * 2.5) % 2 === 0) {
        ctx.fillStyle = HEX.ice;
        ctx.fillRect(x0 + cw + 6, LOGO_Y + 164, 16, 32);
      }
    }
    const glintT = b(28);
    if (t >= glintT && t < glintT + 0.4) {
      const gc = glint.getContext("2d");
      gc.setTransform(1, 0, 0, 1, 0, 0);
      gc.globalCompositeOperation = "source-over";
      gc.clearRect(0, 0, W, H);
      gc.font = LOGO.font;
      gc.textBaseline = "alphabetic";
      gc.fillStyle = "#fff";
      for (const q of LOGO.L.letters) gc.fillText(q.ch, q.x, LOGO_Y);
      const gx = lerp(LOGO.x0 - 200, LOGO.x0 + LOGO.width + 200, E.cio(seg(t, glintT, glintT + 0.4)));
      const g = gc.createLinearGradient(gx - 90, 0, gx + 90, 0);
      g.addColorStop(0, "rgba(143,227,255,0)");
      g.addColorStop(0.5, "rgba(143,227,255,0.9)");
      g.addColorStop(1, "rgba(143,227,255,0)");
      gc.globalCompositeOperation = "source-in";
      gc.fillStyle = g;
      gc.fillRect(0, 0, W, H);
      ctx.drawImage(glint, 0, 0);
    }
    ctx.restore();

    if (t >= 11.8) {
      const f = Math.floor((t - 11.8) * 60);
      const v = [1, 1, 0, 0, 1, 0, 2, 2, 2, 2, 2, 2][Math.min(11, f)];
      if (v) {
        ctx.fillStyle = v === 1 ? HEX.core : HEX.night;
        ctx.fillRect(0, 0, W, H);
      }
    }
  }

  const tl = gsap.timeline({ paused: true });
  const clock = { t: 0 };
  tl.to(clock, { t: 12, duration: 12, ease: "none", onUpdate: () => render(clock.t) }, 0);
  render(0);
  return { tl, render };
}
