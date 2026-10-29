import {AbsoluteFill, Composition, Easing, registerRoot, useCurrentFrame, useVideoConfig} from 'remotion';
import {tween} from '../lib/tween';

function Static() {
  const t = useCurrentFrame() / useVideoConfig().fps;
  const opacity = tween(t, 0, 0.4, 0, 1, Easing.out(Easing.quad)) * tween(t, 9.6, 0.4, 1, 0, Easing.in(Easing.quad));
  return (
    <AbsoluteFill style={{background: '#f6f1e7', fontFamily: 'Helvetica, Arial, sans-serif'}}>
      <AbsoluteFill style={{opacity}}>
        {Array.from({length: 70}, (_, i) => (
          <div
            key={i}
            style={{
              position: 'absolute',
              left: 40 + (i % 7) * 270,
              top: 30 + Math.floor(i / 7) * 105,
              width: 280,
              height: 150,
              borderRadius: 18,
              background: 'linear-gradient(160deg, #fff, #e8dfcf)',
              boxShadow: '0 14px 30px rgba(60, 40, 10, 0.25)',
            }}
          />
        ))}
        <h1
          style={{
            position: 'absolute',
            left: 0,
            right: 0,
            top: 440,
            margin: 0,
            textAlign: 'center',
            fontSize: 150,
            fontWeight: 700,
            color: '#2b2118',
            textShadow: '0 8px 24px rgba(60, 40, 10, 0.3)',
            transform: `translateY(${tween(t, 0, 0.4, 40, 0, Easing.out(Easing.cubic))}px)`,
          }}
        >
          Holding still
        </h1>
      </AbsoluteFill>
    </AbsoluteFill>
  );
}

registerRoot(() => <Composition id="static" component={Static} durationInFrames={300} fps={30} width={1920} height={1080} />);
