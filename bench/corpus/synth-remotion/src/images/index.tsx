import {AbsoluteFill, Composition, Easing, Img, registerRoot, useCurrentFrame, useVideoConfig} from 'remotion';
import photo1 from '../assets/photo-1.jpg';
import photo2 from '../assets/photo-2.jpg';
import photo3 from '../assets/photo-3.jpg';
import photo4 from '../assets/photo-4.jpg';
import photo5 from '../assets/photo-5.jpg';
import photo6 from '../assets/photo-6.jpg';
import {tween} from '../lib/tween';

const photos = [photo1, photo2, photo3, photo4, photo5, photo6];

function Images() {
  const t = useCurrentFrame() / useVideoConfig().fps;
  const ease = Easing.inOut(Easing.sin);
  return (
    <AbsoluteFill style={{background: '#111'}}>
      {Array.from({length: 24}, (_, i) => {
        const base = 0.2 + (i % 3) * 0.03;
        const scale = tween(t, 0, 5, base, base * 2.2, ease);
        const x = tween(t, 0, 5, -((i * 37) % 150), -((i * 91) % 150), ease);
        const y = tween(t, 0, 5, -((i * 23) % 45), -((i * 53) % 45), ease);
        return (
          <div key={i} style={{position: 'absolute', left: (i % 6) * 320, top: Math.floor(i / 6) * 270, width: 320, height: 270, overflow: 'hidden'}}>
            <Img
              src={photos[i % 6]}
              style={{position: 'absolute', left: 0, top: 0, width: 2400, height: 1600, transformOrigin: '0 0', transform: `translate(${x}px, ${y}px) scale(${scale})`}}
            />
          </div>
        );
      })}
    </AbsoluteFill>
  );
}

registerRoot(() => <Composition id="images" component={Images} durationInFrames={150} fps={30} width={1920} height={1080} />);
