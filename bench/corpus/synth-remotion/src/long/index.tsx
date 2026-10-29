import {AbsoluteFill, Composition, Easing, registerRoot, useCurrentFrame, useVideoConfig} from 'remotion';
import {rng} from '../lib/rng';
import {tween} from '../lib/tween';

const colors = ['#ff6b6b', '#ffd166', '#06d6a0', '#6ea8ff'];
const random = rng(7);
const paths = colors.map(() => {
  const points = [{x: 200 + random() * 1400, y: 300 + random() * 500, scale: 1}];
  for (let k = 0; k < 30; k++) {
    points.push({x: 100 + random() * 1600, y: 300 + random() * 540, scale: 0.5 + random() * 1.2});
  }
  return points;
});

function Long() {
  const t = useCurrentFrame() / useVideoConfig().fps;
  const segment = Math.min(Math.floor(t / 2), 29);
  return (
    <AbsoluteFill style={{background: 'linear-gradient(135deg, #0b1020, #1b2a55)', fontFamily: 'Helvetica, Arial, sans-serif'}}>
      <h1
        style={{
          position: 'absolute',
          left: 160,
          top: 120,
          margin: 0,
          fontSize: 120,
          fontWeight: 700,
          color: '#f4f6ff',
          opacity: tween(t, 0, 0.8, 0, 1, Easing.out(Easing.cubic)),
          transform: `translateY(${tween(t, 0, 0.8, 60, 0, Easing.out(Easing.cubic))}px)`,
        }}
      >
        Sixty seconds
      </h1>
      {colors.map((color, i) => {
        const a = paths[i][segment];
        const b = paths[i][segment + 1];
        const start = segment * 2;
        return (
          <div
            key={color}
            style={{
              position: 'absolute',
              left: 0,
              top: 0,
              width: 96,
              height: 96,
              borderRadius: '50%',
              background: color,
              transform: `translate(${tween(t, start, 2, a.x, b.x)}px, ${tween(t, start, 2, a.y, b.y)}px) scale(${tween(t, start, 2, a.scale, b.scale)})`,
            }}
          />
        );
      })}
      <div style={{position: 'absolute', left: 160, top: 960, width: 1600, height: 12, background: '#26335f', borderRadius: 6, overflow: 'hidden'}}>
        <div style={{width: '100%', height: '100%', background: '#6ea8ff', transformOrigin: 'left center', transform: `scaleX(${t / 60})`}} />
      </div>
    </AbsoluteFill>
  );
}

registerRoot(() => <Composition id="long" component={Long} durationInFrames={1800} fps={30} width={1920} height={1080} />);
