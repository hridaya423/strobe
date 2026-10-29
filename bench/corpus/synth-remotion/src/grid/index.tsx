import {AbsoluteFill, Composition, Easing, registerRoot, useCurrentFrame, useVideoConfig} from 'remotion';
import mono from '../assets/jetbrains-mono-latin-400-normal.woff2';
import {loadFonts} from '../lib/fonts';
import {tween} from '../lib/tween';

loadFonts([{family: 'Synth Mono', url: mono, weight: '400'}]);

const labels = Array.from({length: 2400}, (_, i) => String((i * 7919) % 100000).padStart(5, '0') + ' ab');

function Grid() {
  const t = useCurrentFrame() / useVideoConfig().fps;
  const ease = Easing.inOut(Easing.sin);
  const cw = t < 2 ? tween(t, 0, 2, 64, 180, ease) : tween(t, 2, 2, 180, 96, ease);
  const ch = t < 2 ? tween(t, 0, 2, 36, 30, ease) : tween(t, 2, 2, 30, 44, ease);
  return (
    <AbsoluteFill style={{background: '#0e1117'}}>
      <div style={{display: 'flex', flexWrap: 'wrap', width: 1920, fontFamily: '"Synth Mono", monospace', fontSize: 14, color: '#c9d1d9'}}>
        {labels.map((label, i) => {
          const n = i + 1;
          return (
            <div
              key={i}
              style={{
                boxSizing: 'border-box',
                width: cw,
                height: ch,
                padding: '4px 6px',
                border: '1px solid #21262d',
                overflow: 'hidden',
                whiteSpace: 'nowrap',
                background: n % 7 === 0 ? '#161b22' : undefined,
                color: n % 11 === 0 ? '#58a6ff' : undefined,
              }}
            >
              {label}
            </div>
          );
        })}
      </div>
    </AbsoluteFill>
  );
}

registerRoot(() => <Composition id="grid" component={Grid} durationInFrames={120} fps={30} width={1920} height={1080} />);
