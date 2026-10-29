import {AbsoluteFill, Composition, Easing, OffthreadVideo, registerRoot, useCurrentFrame, useVideoConfig} from 'remotion';
import clip from '../assets/clip.mp4';
import {tween} from '../lib/tween';

const chips = [
  {left: 1500, color: '#ff6b6b'},
  {left: 1640, color: '#ffd166'},
  {left: 1780, color: '#06d6a0'},
];

function Video() {
  const t = useCurrentFrame() / useVideoConfig().fps;
  const lower = tween(t, 0.3, 0.7, -900, 0, Easing.out(Easing.cubic)) + tween(t, 4.2, 0.6, 0, -900, Easing.in(Easing.cubic));
  const lowerOpacity = tween(t, 0.3, 0.7, 0, 1, Easing.out(Easing.cubic)) * tween(t, 4.2, 0.6, 1, 0, Easing.in(Easing.cubic));
  return (
    <AbsoluteFill style={{background: '#000', fontFamily: 'Helvetica, Arial, sans-serif'}}>
      <OffthreadVideo src={clip} muted style={{position: 'absolute', left: 0, top: 0, width: 1920, height: 1080, objectFit: 'cover'}} />
      <div
        style={{
          position: 'absolute',
          left: 80,
          bottom: 80,
          padding: '24px 40px',
          background: 'rgba(10, 14, 30, 0.8)',
          borderLeft: '10px solid #6ea8ff',
          color: '#f4f6ff',
          fontSize: 56,
          fontWeight: 700,
          opacity: lowerOpacity,
          transform: `translateX(${lower}px)`,
        }}
      >
        Video under overlays
      </div>
      {chips.map((chip, i) => (
        <div
          key={chip.color}
          style={{
            position: 'absolute',
            top: 60,
            left: chip.left,
            width: 120,
            height: 120,
            borderRadius: '50%',
            background: chip.color,
            opacity: 0.8,
            transform: `translateY(${tween(t, 0.5 + i * 0.15, 0.6, -200, 0, Easing.out(Easing.back(2))) + tween(t, 1.5 + i * 0.2, 2.5, 0, 700, Easing.inOut(Easing.sin))}px) rotate(${tween(t, 1.5 + i * 0.2, 2.5, 0, 180, Easing.inOut(Easing.sin))}deg) scale(${tween(t, 0.5 + i * 0.15, 0.6, 0.5, 1, Easing.out(Easing.back(2)))})`,
          }}
        />
      ))}
    </AbsoluteFill>
  );
}

registerRoot(() => <Composition id="video" component={Video} durationInFrames={150} fps={30} width={1920} height={1080} />);
