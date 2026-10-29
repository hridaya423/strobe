import {AbsoluteFill, Composition, Easing, registerRoot, useCurrentFrame, useVideoConfig} from 'remotion';
import {rng} from '../lib/rng';
import {tween} from '../lib/tween';

const random = rng(11);
const hues = [350, 40, 160, 215, 275, 30];
const layers = Array.from({length: 400}, (_, i) => {
  const size = 60 + random() * 160;
  const angle = random() * 360;
  const from = {x: random() * 1800, y: random() * 960, rotation: random() * 360, scale: 0.6 + random() * 0.6, opacity: 0.35 + random() * 0.6};
  const to = {x: random() * 1800, y: random() * 960, rotation: from.rotation + 90 + random() * 270, scale: 0.4 + random() * 1.0, opacity: 0.3 + random() * 0.7};
  const filter = i % 10 === 1 ? 'blur(6px)' : i % 10 === 2 ? 'drop-shadow(0 0 14px rgba(255, 255, 255, 0.55))' : undefined;
  const boxShadow = i % 4 === 0 ? '0 12px 36px rgba(0, 0, 0, 0.6), 0 0 24px rgba(255, 255, 255, 0.15)' : undefined;
  return {size, from, to, filter, boxShadow, background: `linear-gradient(${angle}deg, hsl(${hues[i % 6]} 85% 60%), hsl(${hues[(i + 2) % 6]} 80% 40%))`};
});

function Layers() {
  const t = useCurrentFrame() / useVideoConfig().fps;
  const ease = Easing.inOut(Easing.sin);
  return (
    <AbsoluteFill style={{background: 'radial-gradient(circle at 50% 50%, #1a1f3a, #090b14 75%)'}}>
      {layers.map((l, i) => (
        <div
          key={i}
          style={{
            position: 'absolute',
            left: 0,
            top: 0,
            width: l.size,
            height: l.size,
            borderRadius: '28%',
            background: l.background,
            filter: l.filter,
            boxShadow: l.boxShadow,
            opacity: tween(t, 0, 5, l.from.opacity, l.to.opacity, ease),
            transform: `translate(${tween(t, 0, 5, l.from.x, l.to.x, ease)}px, ${tween(t, 0, 5, l.from.y, l.to.y, ease)}px) rotate(${tween(t, 0, 5, l.from.rotation, l.to.rotation, ease)}deg) scale(${tween(t, 0, 5, l.from.scale, l.to.scale, ease)})`,
          }}
        />
      ))}
    </AbsoluteFill>
  );
}

registerRoot(() => <Composition id="layers" component={Layers} durationInFrames={150} fps={30} width={1920} height={1080} />);
