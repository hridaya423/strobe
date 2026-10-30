(() => {
  const vrf = (globalThis.__vrf ??= {});

  const COEFFICIENTS = {
    601: [104597, 132201, -25675, -53279],
    709: [117489, 138438, -13975, -34925],
  };
  const QUANT = [
    8, 4, 4, 5, 6, 6, 7, 8, 4, 4, 5, 6, 6, 7, 8, 9, 4, 5, 6, 6, 7, 8, 8, 9, 5, 5, 6, 6, 7, 8, 9, 10, 5, 6, 6, 7, 8, 8, 10, 12, 6, 6, 7, 8, 8,
    10, 12, 14, 6, 6, 7, 8, 9, 11, 14, 17, 6, 7, 8, 9, 11, 14, 17, 20,
  ];
  const rounded = (value) => (value + 32768) >> 16;

  const conversion = (matrix, full) => {
    let [crv, cbu, cgu, cgv] = COEFFICIENTS[matrix];
    let luma = 65536;
    let offset = 0;
    if (full) {
      const scale = (c) => Math.trunc((c * 224) / 255);
      [crv, cbu, cgu, cgv] = [crv, cbu, cgu, cgv].map(scale);
    } else {
      luma = Math.floor((65536 * 255) / 219);
      offset = 16 << 16;
    }
    return {
      luma: rounded(luma * 8192),
      offset: rounded(offset * 512),
      red: rounded(crv * 8192),
      greenV: rounded(cgv * 8192),
      greenU: rounded(cgu * 8192),
      blue: rounded(cbu * 8192),
    };
  };

  const VERTEX = `#version 300 es
    void main() { vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2)); gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0); }`;

  const HEAD = `#version 300 es
    precision highp float;
    precision highp int;
    precision highp usampler2D;
    precision highp isampler2D;
    precision highp sampler2D;
  `;

  const RGB = `
    uniform usampler2D uY;
    uniform usampler2D uUV;
    uniform int uLuma;
    uniform int uOffset;
    uniform int uRed;
    uniform int uGreenV;
    uniform int uGreenU;
    uniform int uBlue;
    uniform ivec2 uSize;
    int saturate(int base, int part) {
      int sum = base + part;
      if (sum < 0 && base > 0 && part > 0) return 255;
      return clamp(sum >> 22, 0, 255);
    }
    ivec3 rgb(ivec2 p) {
      p = clamp(p, ivec2(0), uSize - 1);
      int y = int(texelFetch(uY, p, 0).r);
      uvec2 c = texelFetch(uUV, p >> 1, 0).rg;
      int base = (((y << 9) - uOffset) * uLuma) + 2097152;
      int u = (int(c.r) - 128) << 9;
      int v = (int(c.g) - 128) << 9;
      return ivec3(saturate(base, v * uRed), clamp((base + v * uGreenV + u * uGreenU) >> 22, 0, 255), saturate(base, u * uBlue));
    }
  `;

  const LUMA = `${HEAD}${RGB}
    out uint outColor;
    void main() {
      ivec3 c = rgb(ivec2(gl_FragCoord.xy));
      outColor = uint(clamp((9798 * c.r + 19235 * c.g + 3735 * c.b + 16383) >> 15, 0, 255));
    }`;

  const CHROMA = `${HEAD}${RGB}
    layout(location = 0) out uint outU;
    layout(location = 1) out uint outV;
    void main() {
      ivec2 p = ivec2(gl_FragCoord.xy) * 2 + ivec2(0, 1);
      ivec3 s = rgb(p) + rgb(p + ivec2(1, 0));
      outU = uint(clamp((-5537 * s.r - 10848 * s.g + 16385 * s.b + 8388608) >> 16, 0, 255));
      outV = uint(clamp((16386 * s.r - 13730 * s.g - 2656 * s.b + 8388608) >> 16, 0, 255));
    }`;

  const FDCT = `
    int descale(int x, int n) { return (x + (1 << (n - 1))) >> n; }
    void fdct(int d[8], bool second, out int o[8]) {
      int t0 = d[0] + d[7]; int t7 = d[0] - d[7];
      int t1 = d[1] + d[6]; int t6 = d[1] - d[6];
      int t2 = d[2] + d[5]; int t5 = d[2] - d[5];
      int t3 = d[3] + d[4]; int t4 = d[3] - d[4];
      int t10 = t0 + t3; int t13 = t0 - t3;
      int t11 = t1 + t2; int t12 = t1 - t2;
      int shift = second ? 14 : 12;
      o[0] = second ? descale(t10 + t11, 1) : (t10 + t11) << 1;
      o[4] = second ? descale(t10 - t11, 1) : (t10 - t11) << 1;
      int z1 = (t12 + t13) * 4433;
      o[2] = descale(z1 + t13 * 6270, shift);
      o[6] = descale(z1 + t12 * -15137, shift);
      t10 = t4 + t7; t11 = t5 + t6; t12 = t4 + t6; t13 = t5 + t7;
      int z5 = (t12 + t13) * 9633;
      t4 *= 2446; t5 *= 16819; t6 *= 25172; t7 *= 12299;
      z1 = t10 * -7373;
      int z2 = t11 * -20995;
      int z3 = t12 * -16069 + z5;
      int z4 = t13 * -3196 + z5;
      o[7] = descale(t4 + z1 + z3, shift);
      o[5] = descale(t5 + z2 + z4, shift);
      o[3] = descale(t6 + z2 + z3, shift);
      o[1] = descale(t7 + z1 + z4, shift);
    }
  `;

  const ROWS = `${HEAD}${FDCT}
    uniform usampler2D uPlane;
    uniform ivec2 uSize;
    out int outValue;
    void main() {
      ivec2 p = ivec2(gl_FragCoord.xy);
      int y = min(p.y, uSize.y - 1);
      int d[8];
      for (int j = 0; j < 8; j++) d[j] = int(texelFetch(uPlane, ivec2(min((p.x & ~7) + j, uSize.x - 1), y), 0).r) - 128;
      int o[8];
      fdct(d, false, o);
      outValue = o[p.x & 7];
    }`;

  const COLUMNS = `${HEAD}${FDCT}
    uniform isampler2D uRows;
    out int outValue;
    void main() {
      ivec2 p = ivec2(gl_FragCoord.xy);
      int d[8];
      for (int i = 0; i < 8; i++) d[i] = texelFetch(uRows, ivec2(p.x, (p.y & ~7) + i), 0).r;
      int o[8];
      fdct(d, true, o);
      outValue = o[p.y & 7];
    }`;

  const IDCT = `
    const int Q[64] = int[64](${QUANT.join(',')});
    void idct(int d[8], int shift, out int o[8]) {
      int z1 = (d[2] + d[6]) * 4433;
      int tmp2 = z1 + d[6] * -15137;
      int tmp3 = z1 + d[2] * 6270;
      int tmp0 = (d[0] + d[4]) << 13;
      int tmp1 = (d[0] - d[4]) << 13;
      int t10 = tmp0 + tmp3;
      int t13 = tmp0 - tmp3;
      int t11 = tmp1 + tmp2;
      int t12 = tmp1 - tmp2;
      tmp0 = d[7]; tmp1 = d[5]; tmp2 = d[3]; tmp3 = d[1];
      z1 = tmp0 + tmp3;
      int z2 = tmp1 + tmp2;
      int z3 = tmp0 + tmp2;
      int z4 = tmp1 + tmp3;
      int z5 = (z3 + z4) * 9633;
      tmp0 *= 2446; tmp1 *= 16819; tmp2 *= 25172; tmp3 *= 12299;
      z1 *= -7373; z2 *= -20995; z3 *= -16069; z4 *= -3196;
      z3 += z5; z4 += z5;
      tmp0 += z1 + z3; tmp1 += z2 + z4; tmp2 += z2 + z3; tmp3 += z1 + z4;
      o[0] = descale(t10 + tmp3, shift); o[7] = descale(t10 - tmp3, shift);
      o[1] = descale(t11 + tmp2, shift); o[6] = descale(t11 - tmp2, shift);
      o[2] = descale(t12 + tmp1, shift); o[5] = descale(t12 - tmp1, shift);
      o[3] = descale(t13 + tmp0, shift); o[4] = descale(t13 - tmp0, shift);
    }
  `;

  const VERTICAL = `${HEAD}${FDCT}${IDCT}
    uniform isampler2D uCoefficients;
    out int outValue;
    int level(int c, int q, bool dc) {
      if (dc) return (c + 32) >> 6;
      int n = abs(c);
      int step = 8 * q;
      int k = n / step;
      int rem = 8 * (n - k * step);
      bool up = rem > 5 * step || (rem == 5 * step && (q == 4 || q == 8));
      return sign(c) * (k + (up ? 1 : 0));
    }
    void main() {
      ivec2 p = ivec2(gl_FragCoord.xy);
      int column = p.x & 7;
      int d[8];
      for (int r = 0; r < 8; r++) {
        int q = Q[r * 8 + column];
        d[r] = level(texelFetch(uCoefficients, ivec2(p.x, (p.y & ~7) + r), 0).r, q, r == 0 && column == 0) * q;
      }
      int o[8];
      idct(d, 11, o);
      outValue = o[p.y & 7];
    }`;

  const HORIZONTAL = `${HEAD}${FDCT}${IDCT}
    uniform isampler2D uColumns;
    out uint outValue;
    void main() {
      ivec2 p = ivec2(gl_FragCoord.xy);
      int d[8];
      for (int u = 0; u < 8; u++) d[u] = texelFetch(uColumns, ivec2((p.x & ~7) + u, p.y), 0).r;
      int o[8];
      idct(d, 18, o);
      outValue = uint(clamp(o[p.x & 7] + 128, 0, 255));
    }`;

  const OUTPUT = `${HEAD}
    uniform usampler2D uY;
    uniform usampler2D uU;
    uniform usampler2D uV;
    uniform ivec2 uSize;
    out vec4 outColor;
    int fancy(usampler2D plane, ivec2 p) {
      ivec2 last = (uSize >> 1) - 1;
      ivec2 c = p >> 1;
      int far = ((p.y & 1) == 0) ? max(c.y - 1, 0) : min(c.y + 1, last.y);
      int side = ((p.x & 1) == 0) ? max(c.x - 1, 0) : min(c.x + 1, last.x);
      int near = int(texelFetch(plane, c, 0).r) * 3 + int(texelFetch(plane, ivec2(c.x, far), 0).r);
      int other = int(texelFetch(plane, ivec2(side, c.y), 0).r) * 3 + int(texelFetch(plane, ivec2(side, far), 0).r);
      return (near * 3 + other + (((p.x & 1) == 0) ? 8 : 7)) >> 4;
    }
    void main() {
      ivec2 p = ivec2(int(gl_FragCoord.x), uSize.y - 1 - int(gl_FragCoord.y));
      int y = int(texelFetch(uY, p, 0).r);
      int cb = fancy(uU, p) - 128;
      int cr = fancy(uV, p) - 128;
      ivec3 c = ivec3(y + ((91881 * cr + 32768) >> 16), y + ((-22554 * cb - 46802 * cr + 32768) >> 16), y + ((116130 * cb + 32768) >> 16));
      outColor = vec4(vec3(clamp(c, 0, 255)) / 255.0, 1.0);
    }`;

  const createSurface = () => {
    const canvas = document.createElement('canvas');
    const gl = canvas.getContext('webgl2', { alpha: false, antialias: false, preserveDrawingBuffer: false });
    const compile = (type, source) => {
      const object = gl.createShader(type);
      gl.shaderSource(object, source);
      gl.compileShader(object);
      if (!gl.getShaderParameter(object, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(object));
      return object;
    };
    const program = (fragment) => {
      const object = gl.createProgram();
      gl.attachShader(object, compile(gl.VERTEX_SHADER, VERTEX));
      gl.attachShader(object, compile(gl.FRAGMENT_SHADER, fragment));
      gl.linkProgram(object);
      if (!gl.getProgramParameter(object, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(object));
      return object;
    };
    const programs = {
      luma: program(LUMA),
      chroma: program(CHROMA),
      rows: program(ROWS),
      columns: program(COLUMNS),
      vertical: program(VERTICAL),
      horizontal: program(HORIZONTAL),
      output: program(OUTPUT),
    };
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    const framebuffer = gl.createFramebuffer();
    const FORMATS = {
      u8: [gl.R8UI, gl.RED_INTEGER, gl.UNSIGNED_BYTE],
      u8x2: [gl.RG8UI, gl.RG_INTEGER, gl.UNSIGNED_BYTE],
      i32: [gl.R32I, gl.RED_INTEGER, gl.INT],
    };
    const texture = (kind, width, height) => {
      const object = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, object);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      const [internal, format, type] = FORMATS[kind];
      gl.texImage2D(gl.TEXTURE_2D, 0, internal, width, height, 0, format, type, null);
      return { object, width, height, kind };
    };
    const padded = (n) => (n + 7) & ~7;

    let resources = null;
    const allocate = (w, h) => {
      const cw = w >> 1;
      const ch = h >> 1;
      const plane = (pw, ph) => ({
        width: pw,
        height: ph,
        source: texture('u8', pw, ph),
        rows: texture('i32', padded(pw), padded(ph)),
        coefficients: texture('i32', padded(pw), padded(ph)),
        columns: texture('i32', padded(pw), padded(ph)),
        result: texture('u8', padded(pw), padded(ph)),
      });
      resources = {
        w,
        h,
        y: texture('u8', w, h),
        uv: texture('u8x2', cw, ch),
        planes: { y: plane(w, h), u: plane(cw, ch), v: plane(cw, ch) },
      };
      canvas.width = w;
      canvas.height = h;
    };

    const bind = (unit, name, program, source) => {
      gl.activeTexture(gl.TEXTURE0 + unit);
      gl.bindTexture(gl.TEXTURE_2D, source.object);
      gl.uniform1i(gl.getUniformLocation(program, name), unit);
    };

    const render = (program, targets, width, height, inputs, setup) => {
      gl.useProgram(program);
      inputs.forEach(([name, source], unit) => bind(unit, name, program, source));
      if (setup) setup(program);
      if (targets) {
        gl.bindFramebuffer(gl.FRAMEBUFFER, framebuffer);
        targets.forEach((target, i) => gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0 + i, gl.TEXTURE_2D, target.object, 0));
        gl.drawBuffers(targets.map((_, i) => gl.COLOR_ATTACHMENT0 + i));
      } else {
        gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      }
      gl.viewport(0, 0, width, height);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
      if (targets) targets.forEach((_, i) => gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0 + i, gl.TEXTURE_2D, null, 0));
    };

    const roundTrip = (plane, source) => {
      const { width, height } = plane;
      const w8 = padded(width);
      const h8 = padded(height);
      render(programs.rows, [plane.rows], w8, h8, [['uPlane', source]], (p) => {
        gl.uniform2i(gl.getUniformLocation(p, 'uSize'), width, height);
      });
      render(programs.columns, [plane.coefficients], w8, h8, [['uRows', plane.rows]]);
      render(programs.vertical, [plane.columns], w8, h8, [['uCoefficients', plane.coefficients]]);
      render(programs.horizontal, [plane.result], w8, h8, [['uColumns', plane.columns]]);
    };

    return {
      canvas,
      draw(picture, { matrix, full }) {
        const { width: w, height: h } = picture;
        if (!resources || resources.w !== w || resources.h !== h) allocate(w, h);
        const cw = w >> 1;
        const ch = h >> 1;
        const { y, uv, planes } = resources;
        gl.activeTexture(gl.TEXTURE0);
        gl.bindTexture(gl.TEXTURE_2D, y.object);
        gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, w, h, gl.RED_INTEGER, gl.UNSIGNED_BYTE, picture.y);
        gl.bindTexture(gl.TEXTURE_2D, uv.object);
        gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, cw, ch, gl.RG_INTEGER, gl.UNSIGNED_BYTE, picture.uv);
        const k = conversion(matrix, full);
        const setup = (p) => {
          gl.uniform1i(gl.getUniformLocation(p, 'uLuma'), k.luma);
          gl.uniform1i(gl.getUniformLocation(p, 'uOffset'), k.offset);
          gl.uniform1i(gl.getUniformLocation(p, 'uRed'), k.red);
          gl.uniform1i(gl.getUniformLocation(p, 'uGreenV'), k.greenV);
          gl.uniform1i(gl.getUniformLocation(p, 'uGreenU'), k.greenU);
          gl.uniform1i(gl.getUniformLocation(p, 'uBlue'), k.blue);
          gl.uniform2i(gl.getUniformLocation(p, 'uSize'), w, h);
        };
        const inputs = [['uY', y], ['uUV', uv]];
        render(programs.luma, [planes.y.source], w, h, inputs, setup);
        render(programs.chroma, [planes.u.source, planes.v.source], cw, ch, inputs, setup);
        roundTrip(planes.y, planes.y.source);
        roundTrip(planes.u, planes.u.source);
        roundTrip(planes.v, planes.v.source);
        render(
          programs.output,
          null,
          w,
          h,
          [['uY', planes.y.result], ['uU', planes.u.result], ['uV', planes.v.result]],
          (p) => gl.uniform2i(gl.getUniformLocation(p, 'uSize'), w, h),
        );
      },
    };
  };

  vrf.video.createJpegSurface = createSurface;
})();
