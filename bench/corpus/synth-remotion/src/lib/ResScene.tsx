import {AbsoluteFill, Easing, useCurrentFrame, useVideoConfig} from 'remotion';
import {tween} from './tween';

const colors = ['#ff6b6b', '#ffd166', '#06d6a0', '#6ea8ff', '#c77dff', '#ff9f1c'];

export function ResScene() {
  const {fps, width} = useVideoConfig();
  const t = useCurrentFrame() / fps;
  return (
    <AbsoluteFill style={{background: '#10131c', fontFamily: 'Helvetica, Arial, sans-serif'}}>
      <div
        style={{
          position: 'absolute',
          left: 0,
          top: 0,
          width: 1920,
          height: 1080,
          transformOrigin: '0 0',
          transform: `scale(${width / 1920})`,
          background: 'radial-gradient(circle at 30% 20%, #26335f, #10131c 70%)',
        }}
      >
        <h1
          style={{
            position: 'absolute',
            left: 120,
            top: 100,
            margin: 0,
            fontSize: 110,
            fontWeight: 700,
            color: '#f4f6ff',
            letterSpacing: -2,
            opacity: tween(t, 0, 0.7, 0, 1, Easing.out(Easing.cubic)),
            transform: `translateY(${tween(t, 0, 0.7, 50, 0, Easing.out(Easing.cubic))}px)`,
          }}
        >
          Resolution
        </h1>
        <div style={{position: 'absolute', left: 124, top: 240, fontSize: 40, color: '#9fb4ff', opacity: tween(t, 0.3, 0.7, 0, 1, Easing.out(Easing.quad))}}>
          One scene, three sizes
        </div>
        <div
          style={{
            position: 'absolute',
            left: 1360,
            top: 120,
            width: 400,
            height: 400,
            borderRadius: '50%',
            border: '24px solid #6ea8ff',
            borderTopColor: '#ff6b6b',
            boxSizing: 'border-box',
            transform: `rotate(${tween(t, 0, 3, 0, 360, (x) => x)}deg)`,
          }}
        />
        {Array.from({length: 12}, (_, i) => {
          const start = 0.4 + i * 0.08;
          const bob = Math.sin(Math.min(Math.max((t - (1.4 + (i % 6) * 0.05)) / 2, 0), 1) * Math.PI);
          return (
            <div
              key={i}
              style={{
                position: 'absolute',
                left: 120 + (i % 6) * 290,
                top: 600 + Math.floor(i / 6) * 190,
                width: 260,
                height: 160,
                borderRadius: 24,
                boxShadow: '0 18px 40px rgba(0, 0, 0, 0.45)',
                background: `linear-gradient(135deg, ${colors[i % 6]}, #1b2a55)`,
                opacity: tween(t, start, 0.8, 0, 1, Easing.out(Easing.quad)),
                transform: `translateY(${tween(t, start, 0.8, 200, 0, Easing.out(Easing.back(1.4))) - 24 * bob}px) rotate(${tween(t, start, 0.8, -8, 0, Easing.out(Easing.back(1.4)))}deg)`,
              }}
            />
          );
        })}
      </div>
    </AbsoluteFill>
  );
}
