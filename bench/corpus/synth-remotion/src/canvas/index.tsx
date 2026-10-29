import {useLayoutEffect, useRef} from 'react';
import {AbsoluteFill, Composition, registerRoot, useCurrentFrame, useVideoConfig} from 'remotion';
import {rng} from '../lib/rng';

const random = rng(23);
const particles = Array.from({length: 5000}, () => ({
  radius: 40 + random() * 520,
  angle: random() * Math.PI * 2,
  speed: (0.3 + random() * 0.9) * (random() < 0.5 ? -1 : 1),
  wobble: random() * Math.PI * 2,
  size: 4 + random() * 14,
  hue: 190 + random() * 140,
}));

const sprites: Record<number, HTMLCanvasElement> = {};
function sprite(hue: number) {
  const key = Math.round(hue / 10);
  if (!sprites[key]) {
    const c = document.createElement('canvas');
    c.width = c.height = 32;
    const g = c.getContext('2d')!;
    const grad = g.createRadialGradient(16, 16, 0, 16, 16, 16);
    grad.addColorStop(0, `hsla(${key * 10}, 90%, 70%, 1)`);
    grad.addColorStop(1, `hsla(${key * 10}, 90%, 50%, 0)`);
    g.fillStyle = grad;
    g.fillRect(0, 0, 32, 32);
    sprites[key] = c;
  }
  return sprites[key];
}

function draw(ctx: CanvasRenderingContext2D, t: number) {
  ctx.globalCompositeOperation = 'source-over';
  ctx.fillStyle = '#05060d';
  ctx.fillRect(0, 0, 1920, 1080);
  ctx.globalCompositeOperation = 'lighter';
  for (const p of particles) {
    const a = p.angle + p.speed * t;
    const r = p.radius * (1 + 0.15 * Math.sin(t * 2 + p.wobble));
    const x = 960 + Math.cos(a) * r * 1.5;
    const y = 540 + Math.sin(a) * r * 0.9;
    const s = p.size * (1 + 0.4 * Math.sin(t * 3 + p.wobble));
    ctx.drawImage(sprite(p.hue), x - s, y - s, s * 2, s * 2);
  }
}

function Canvas() {
  const ref = useRef<HTMLCanvasElement>(null);
  const t = useCurrentFrame() / useVideoConfig().fps;
  useLayoutEffect(() => draw(ref.current!.getContext('2d')!, t), [t]);
  return (
    <AbsoluteFill style={{background: '#05060d'}}>
      <canvas ref={ref} width={1920} height={1080} />
    </AbsoluteFill>
  );
}

registerRoot(() => <Composition id="canvas" component={Canvas} durationInFrames={150} fps={30} width={1920} height={1080} />);
