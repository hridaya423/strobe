import React, { useLayoutEffect, useRef } from 'react';
import { Composition, registerRoot, useCurrentFrame } from 'remotion';

const fragment = `
  precision highp float;
  uniform float time;
  float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
  void main() { gl_FragColor = vec4(vec3(hash(floor(gl_FragCoord.xy / 4.0) + time)), 1.0); }`;

const Shader = () => {
  const canvas = useRef(null);
  const frame = useCurrentFrame();
  useLayoutEffect(() => {
    const gl = canvas.current.getContext('webgl', { preserveDrawingBuffer: true, antialias: false });
    const compile = (type, source) => {
      const shader = gl.createShader(type);
      gl.shaderSource(shader, source);
      gl.compileShader(shader);
      return shader;
    };
    const program = gl.createProgram();
    gl.attachShader(program, compile(gl.VERTEX_SHADER, 'attribute vec2 p; void main() { gl_Position = vec4(p, 0.0, 1.0); }'));
    gl.attachShader(program, compile(gl.FRAGMENT_SHADER, fragment));
    gl.linkProgram(program);
    gl.useProgram(program);
    gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
    const position = gl.getAttribLocation(program, 'p');
    gl.enableVertexAttribArray(position);
    gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);
    gl.uniform1f(gl.getUniformLocation(program, 'time'), frame);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }, [frame]);
  return <canvas ref={canvas} width={320} height={180} />;
};

registerRoot(() => <Composition id="Shader" component={Shader} durationInFrames={10} fps={30} width={320} height={180} />);
