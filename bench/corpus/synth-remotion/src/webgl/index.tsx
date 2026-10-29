import {useLayoutEffect, useRef} from 'react';
import {AbsoluteFill, Composition, registerRoot, useCurrentFrame, useVideoConfig} from 'remotion';

const vertexSource = 'attribute vec2 p; void main() { gl_Position = vec4(p, 0.0, 1.0); }';
const fragmentSource = `
  precision highp float;
  uniform vec2 res;
  uniform float time;
  float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
  float noise(vec2 p) {
    vec2 i = floor(p), f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), f.x), mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), f.x), f.y);
  }
  float fbm(vec2 p) {
    float v = 0.0, a = 0.5;
    for (int i = 0; i < 5; i++) { v += a * noise(p); p = p * 2.03 + 17.0; a *= 0.5; }
    return v;
  }
  void main() {
    vec2 uv = gl_FragCoord.xy / res.y;
    vec2 q = vec2(fbm(uv * 3.0 + time * 0.2), fbm(uv * 3.0 + vec2(5.2, 1.3) - time * 0.15));
    vec2 r = vec2(fbm(uv * 3.0 + 4.0 * q + vec2(1.7, 9.2) + time * 0.1), fbm(uv * 3.0 + 4.0 * q + vec2(8.3, 2.8)));
    float f = fbm(uv * 3.0 + 4.0 * r);
    vec3 col = mix(vec3(0.05, 0.1, 0.35), vec3(0.95, 0.45, 0.2), clamp(f * f * 2.2, 0.0, 1.0));
    col = mix(col, vec3(0.9, 0.95, 1.0), clamp(length(q) * 0.6, 0.0, 1.0) * f);
    gl_FragColor = vec4(col, 1.0);
  }`;

type Renderer = {gl: WebGLRenderingContext; time: WebGLUniformLocation | null};

function createRenderer(canvas: HTMLCanvasElement): Renderer {
  const gl = canvas.getContext('webgl', {preserveDrawingBuffer: true, antialias: false})!;
  const compile = (type: number, source: string) => {
    const shader = gl.createShader(type)!;
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    return shader;
  };
  const program = gl.createProgram()!;
  gl.attachShader(program, compile(gl.VERTEX_SHADER, vertexSource));
  gl.attachShader(program, compile(gl.FRAGMENT_SHADER, fragmentSource));
  gl.linkProgram(program);
  gl.useProgram(program);
  gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
  const position = gl.getAttribLocation(program, 'p');
  gl.enableVertexAttribArray(position);
  gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);
  gl.uniform2f(gl.getUniformLocation(program, 'res'), 1920, 1080);
  return {gl, time: gl.getUniformLocation(program, 'time')};
}

function WebGL() {
  const ref = useRef<HTMLCanvasElement>(null);
  const renderer = useRef<Renderer | null>(null);
  const t = useCurrentFrame() / useVideoConfig().fps;
  useLayoutEffect(() => {
    renderer.current ??= createRenderer(ref.current!);
    const {gl, time} = renderer.current;
    gl.uniform1f(time, t);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }, [t]);
  return (
    <AbsoluteFill style={{background: '#000'}}>
      <canvas ref={ref} width={1920} height={1080} />
    </AbsoluteFill>
  );
}

registerRoot(() => <Composition id="webgl" component={WebGL} durationInFrames={120} fps={30} width={1920} height={1080} />);
