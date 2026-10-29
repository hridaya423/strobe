import {AbsoluteFill, Composition, Easing, registerRoot, useCurrentFrame, useVideoConfig} from 'remotion';
import {tween} from '../lib/tween';

const dots = [
  {top: 560, color: '#ff6b6b'},
  {top: 640, color: '#ffd166'},
  {top: 720, color: '#06d6a0'},
];

function Startup() {
  const t = useCurrentFrame() / useVideoConfig().fps;
  return (
    <AbsoluteFill style={{background: 'linear-gradient(135deg, #0b1020, #1b2a55)', fontFamily: 'Helvetica, Arial, sans-serif'}}>
      <h1
        style={{
          position: 'absolute',
          left: 160,
          top: 380,
          margin: 0,
          fontSize: 140,
          fontWeight: 700,
          color: '#f4f6ff',
          opacity: tween(t, 0, 0.6, 0, 1, Easing.out(Easing.cubic)),
          transform: `translateY(${tween(t, 0, 0.6, 60, 0, Easing.out(Easing.cubic))}px)`,
        }}
      >
        Startup
      </h1>
      {dots.map((dot, i) => (
        <div
          key={dot.color}
          style={{
            position: 'absolute',
            left: 160,
            top: dot.top,
            width: 96,
            height: 96,
            borderRadius: '50%',
            background: dot.color,
            transform: `translateX(${tween(t, i * 0.1, 1.6, 0, 1500)}px) scale(${tween(t, i * 0.1, 1.6, 0.4, 1)})`,
          }}
        />
      ))}
      <div style={{position: 'absolute', left: 160, top: 900, width: 1600, height: 12, background: '#26335f', borderRadius: 6, overflow: 'hidden'}}>
        <div style={{width: '100%', height: '100%', background: '#6ea8ff', transformOrigin: 'left center', transform: `scaleX(${tween(t, 0, 2, 0, 1, (x) => x)})`}} />
      </div>
    </AbsoluteFill>
  );
}

registerRoot(() => <Composition id="startup" component={Startup} durationInFrames={60} fps={30} width={1920} height={1080} />);
