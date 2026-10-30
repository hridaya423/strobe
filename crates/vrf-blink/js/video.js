(() => {
  const vrf = (globalThis.__vrf ??= {});
  if (vrf.video) return;

  const AHEAD_KEEP = 12;
  const BEHIND_KEEP = 2;
  const HEADER = 32;
  let pixel = null;
  const placeholder = () => {
    if (!pixel) {
      const canvas = new OffscreenCanvas(1, 1);
      canvas.getContext('2d');
      pixel = canvas.convertToBlob().then((blob) => blob.arrayBuffer());
    }
    return pixel;
  };

  const SWS = {
    '709': { limited: true, r: [100900, 256, -239], b: [118893, 128, -204], gu: [-12004, 256, -460], gv: [-29996, 256, -495] },
    '601': { limited: true, r: [89829, 512, -261], b: [113534, 512, -215], gu: [-22049, 64, -480], gv: [-45756, 192, -526] },
    '709f': { limited: false, r: [103204, 64, -293], b: [121606, 128, -257], gu: [-12288, 640, -518], gv: [-30681, 64, -554] },
    '601f': { limited: false, r: [91878, 640, -315], b: [116125, 576, -268], gu: [-22552, 0, -539], gv: [-46812, 8512, -586] },
  };
  const swsConfig = (matrix, full) => SWS[`${matrix}${full ? 'f' : ''}`];

  const lumaStep = (limited) => (limited ? [Math.floor((65536 * 255) / 219), 16 << 16] : [65536, 0]);

  const swsRgb = (config, y, u, v) => {
    const [cy, oy] = lumaStep(config.limited);
    const off = ([inc, c, a], x) => ((x * inc + c) >> 16) - a;
    const q = (j) => Math.min(255, Math.max(0, ((j - 110) * cy - (384 << 16) - oy + 0x8000) >> 16));
    const g0 = off(config.gu, 128) + off(config.gv, 128) - off(config.r, 128);
    return [q(y + off(config.r, v)), q(y + off(config.gu, u) + off(config.gv, v) - g0), q(y + off(config.b, u))];
  };

  const chooseFrame = (pts, ticks) => {
    const target = Math.trunc(ticks);
    let low = 0;
    let high = pts.length;
    while (low < high) {
      const mid = (low + high) >> 1;
      if (pts[mid] <= target) low = mid + 1;
      else high = mid;
    }
    const before = low - 1;
    if (before < 0) return 0;
    if (before === pts.length - 1) return before;
    return target - pts[before] < pts[before + 1] - target ? before : before + 1;
  };

  const send = (message) => globalThis.__vrf_video_send(JSON.stringify(message));

  let fallbackInfo = null;
  let lastRefusal = '';
  const clips = new Map();
  const opening = new Map();
  const pendingOpen = new Map();
  let nextRequest = 1;

  const trim = (clip, k) => {
    for (const key of clip.frames.keys()) if (key < k - BEHIND_KEEP || key > k + AHEAD_KEEP) clip.frames.delete(key);
  };

  const open = (src, jpeg) => {
    if (!opening.has(src)) {
      const req = nextRequest++;
      opening.set(src, new Promise((resolve) => pendingOpen.set(req, resolve)));
      send({ t: 'open', req, src, jpeg });
    }
    return opening.get(src);
  };

  const frame = (clip, k) => {
    const cached = clip.frames.get(k);
    if (cached) {
      if (clip.last !== k) send({ t: 'want', clip: clip.id, k, hit: true });
      clip.last = k;
      trim(clip, k);
      return Promise.resolve(cached);
    }
    clip.last = k;
    return new Promise((resolve, reject) => {
      if (!clip.waiters.has(k)) clip.waiters.set(k, []);
      clip.waiters.get(k).push({ resolve, reject });
      send({ t: 'want', clip: clip.id, k, hit: false });
    });
  };

  const onMeta = (buffer) => {
    const head = new DataView(buffer);
    const id = head.getUint32(4, true);
    const count = head.getUint32(8, true);
    const jsonLength = head.getUint32(12, true);
    const meta = JSON.parse(new TextDecoder().decode(new Uint8Array(buffer, HEADER, jsonLength)));
    let clip = null;
    if (meta.eligible) {
      const at = HEADER + ((jsonLength + 7) & ~7);
      clip = {
        id,
        width: meta.width,
        height: meta.height,
        timescale: meta.timescale,
        config: swsConfig(meta.matrix, meta.full),
        pts: new Float64Array(buffer.slice(at, at + count * 8)),
        frames: new Map(),
        waiters: new Map(),
        last: -1,
      };
      clips.set(id, clip);
    }
    if (meta.fallback) fallbackInfo = meta.fallback;
    if (!meta.eligible) lastRefusal = meta.reason ?? meta.fallbackError ?? '';
    pendingOpen.get(meta.req)(clip);
    pendingOpen.delete(meta.req);
  };

  const onFrame = (buffer) => {
    const head = new DataView(buffer);
    const clip = clips.get(head.getUint32(4, true));
    const k = head.getUint32(8, true);
    const w = head.getUint32(12, true);
    const h = head.getUint32(16, true);
    const cw = (w + 1) >> 1;
    const ch = (h + 1) >> 1;
    const picture = {
      width: w,
      height: h,
      y: new Uint8Array(buffer, HEADER, w * h),
      uv: new Uint8Array(buffer, HEADER + w * h, cw * ch * 2),
    };
    clip.frames.set(k, picture);
    const waiting = clip.waiters.get(k);
    if (waiting) {
      clip.waiters.delete(k);
      for (const { resolve } of waiting) resolve(picture);
    }
  };

  const onJpeg = (buffer) => {
    const head = new DataView(buffer);
    const clip = clips.get(head.getUint32(4, true));
    const k = head.getUint32(8, true);
    const picture = { jpeg: new Uint8Array(buffer, HEADER, head.getUint32(12, true)) };
    clip.frames.set(k, picture);
    const waiting = clip.waiters.get(k);
    if (waiting) {
      clip.waiters.delete(k);
      for (const { resolve } of waiting) resolve(picture);
    }
  };

  const onFailure = (buffer) => {
    const head = new DataView(buffer);
    const clip = clips.get(head.getUint32(4, true));
    const k = head.getUint32(16, true);
    const { error } = JSON.parse(new TextDecoder().decode(new Uint8Array(buffer, HEADER, head.getUint32(12, true))));
    const waiting = clip.waiters.get(k) ?? [];
    clip.waiters.delete(k);
    for (const { reject } of waiting) reject(new Error(`video decode failed at frame ${k}: ${error}`));
  };

  globalThis.__vrf_video_push = (buffer) => {
    const kind = new DataView(buffer).getUint32(0, true);
    if (kind === 1) onFrame(buffer);
    else if (kind === 3) onFailure(buffer);
    else if (kind === 4) onJpeg(buffer);
    else onMeta(buffer);
  };

  const VERTEX = `#version 300 es
    void main() { vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2)); gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0); }`;
  const FRAGMENT = `#version 300 es
    precision highp float;
    precision highp int;
    uniform highp usampler2D uY;
    uniform highp usampler2D uUV;
    uniform ivec3 uR;
    uniform ivec3 uB;
    uniform ivec3 uGu;
    uniform ivec3 uGv;
    uniform ivec2 uLuma;
    uniform int uHeight;
    out vec4 outColor;
    int off(ivec3 p, int x) { return ((x * p.x + p.y) >> 16) - p.z; }
    int q(int j) { return clamp(((j - 110) * uLuma.x - 25165824 - uLuma.y + 32768) >> 16, 0, 255); }
    void main() {
      ivec2 p = ivec2(int(gl_FragCoord.x), uHeight - 1 - int(gl_FragCoord.y));
      int y = int(texelFetch(uY, p, 0).r);
      uvec2 c = texelFetch(uUV, p >> 1, 0).rg;
      int u = int(c.r);
      int v = int(c.g);
      int g0 = off(uGu, 128) + off(uGv, 128) - off(uR, 128);
      outColor = vec4(float(q(y + off(uR, v))), float(q(y + off(uGu, u) + off(uGv, v) - g0)), float(q(y + off(uB, u))), 255.0) / 255.0;
    }`;

  const createSurface = () => {
    const canvas = document.createElement('canvas');
    canvas.width = 1;
    canvas.height = 1;
    const gl = canvas.getContext('webgl2', { alpha: false, antialias: false, preserveDrawingBuffer: false });
    const shader = (type, source) => {
      const object = gl.createShader(type);
      gl.shaderSource(object, source);
      gl.compileShader(object);
      if (!gl.getShaderParameter(object, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(object));
      return object;
    };
    const program = gl.createProgram();
    gl.attachShader(program, shader(gl.VERTEX_SHADER, VERTEX));
    gl.attachShader(program, shader(gl.FRAGMENT_SHADER, FRAGMENT));
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(program));
    gl.useProgram(program);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    const texture = (unit) => {
      const t = gl.createTexture();
      gl.activeTexture(gl.TEXTURE0 + unit);
      gl.bindTexture(gl.TEXTURE_2D, t);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      return t;
    };
    texture(0);
    texture(1);
    gl.uniform1i(gl.getUniformLocation(program, 'uY'), 0);
    gl.uniform1i(gl.getUniformLocation(program, 'uUV'), 1);
    const at = (name) => gl.getUniformLocation(program, name);
    const uniforms = { r: at('uR'), b: at('uB'), gu: at('uGu'), gv: at('uGv'), luma: at('uLuma'), height: at('uHeight') };
    let size = '';
    return {
      canvas,
      draw(picture, config) {
        const { width: w, height: h } = picture;
        const cw = (w + 1) >> 1;
        const ch = (h + 1) >> 1;
        if (size !== `${w}x${h}`) {
          size = `${w}x${h}`;
          canvas.width = w;
          canvas.height = h;
          gl.viewport(0, 0, w, h);
          gl.activeTexture(gl.TEXTURE0);
          gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8UI, w, h, 0, gl.RED_INTEGER, gl.UNSIGNED_BYTE, null);
          gl.activeTexture(gl.TEXTURE1);
          gl.texImage2D(gl.TEXTURE_2D, 0, gl.RG8UI, cw, ch, 0, gl.RG_INTEGER, gl.UNSIGNED_BYTE, null);
          gl.uniform1i(uniforms.height, h);
        }
        gl.uniform3i(uniforms.r, ...config.r);
        gl.uniform3i(uniforms.b, ...config.b);
        gl.uniform3i(uniforms.gu, ...config.gu);
        gl.uniform3i(uniforms.gv, ...config.gv);
        gl.uniform2i(uniforms.luma, ...lumaStep(config.limited));
        gl.activeTexture(gl.TEXTURE0);
        gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, w, h, gl.RED_INTEGER, gl.UNSIGNED_BYTE, picture.y);
        gl.activeTexture(gl.TEXTURE1);
        gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, cw, ch, gl.RG_INTEGER, gl.UNSIGNED_BYTE, picture.uv);
        gl.drawArrays(gl.TRIANGLES, 0, 3);
      },
    };
  };

  const nativeCreateObjectURL = URL.createObjectURL.bind(URL);
  const tagged = new WeakMap();
  const sources = new Map();
  URL.createObjectURL = (object) => {
    const url = nativeCreateObjectURL(object);
    if (tagged.has(object)) sources.set(url, tagged.get(object));
    return url;
  };

  const pool = [];
  let shown = [];

  const request = async (src, time) => {
    const clip = await open(src);
    if (!clip) return null;
    const k = chooseFrame(clip.pts, time * clip.timescale);
    const [picture, bytes] = await Promise.all([frame(clip, k), placeholder()]);
    const response = new Response(null, { status: 200 });
    response.blob = async () => {
      const blob = new Blob([bytes], { type: 'image/png' });
      tagged.set(blob, { clip, k, picture });
      return blob;
    };
    return response;
  };

  const present = () => {
    for (const surface of shown) {
      surface.canvas.remove();
      pool.push(surface);
    }
    const live = [];
    for (const img of document.images) {
      const source = sources.get(img.src);
      if (!source) continue;
      const surface = pool.pop() ?? createSurface();
      const { canvas } = surface;
      canvas.className = img.className;
      canvas.style.cssText = img.style.cssText;
      img.style.display = 'none';
      img.parentNode.insertBefore(canvas, img);
      surface.draw(source.picture, source.clip.config);
      live.push(surface);
    }
    shown = live;
    return live.length;
  };

  vrf.video = {
    open,
    request,
    present,
    fallback: () => fallbackInfo,
    refusal: () => lastRefusal,
    chooseFrame,
    swsRgb,
    swsConfig,
    frame,
    clips,
    createSurface,
  };
})();
