import {AbsoluteFill, Composition, Easing, registerRoot, useCurrentFrame, useVideoConfig} from 'remotion';
import inter from '../assets/inter-latin-800-normal.woff2';
import grotesk from '../assets/space-grotesk-latin-700-normal.woff2';
import playfair from '../assets/playfair-display-latin-800-normal.woff2';
import mono from '../assets/jetbrains-mono-latin-400-normal.woff2';
import {loadFonts} from '../lib/fonts';
import {tween} from '../lib/tween';

loadFonts([
  {family: 'Synth Inter', url: inter, weight: '800'},
  {family: 'Synth Grotesk', url: grotesk, weight: '700'},
  {family: 'Synth Playfair', url: playfair, weight: '800'},
  {family: 'Synth Mono', url: mono, weight: '400'},
]);

const phrases = [
  'Every frame is a function of time',
  'Seek the page and read the pixels',
  'Fonts decide where every glyph lands',
  'Kinetic type moves one letter at a time',
  'Layout is measured once then animated',
  'Compositing is cheap when layers are few',
  'Text raster is the cost nobody counts',
  'Ligatures kerning and hinting all matter',
  'Subpixel positions blur the stems',
  'Stagger the letters and watch them settle',
  'Deterministic output needs a fixed clock',
  'Render the sentence then render the next',
];

const lineStyles = [
  {fontFamily: '"Synth Inter", sans-serif', fontWeight: 800, color: '#14110f'},
  {fontFamily: '"Synth Grotesk", sans-serif', fontWeight: 700, color: '#c2410c'},
  {fontFamily: '"Synth Playfair", serif', fontWeight: 800, color: '#1d4ed8'},
  {fontFamily: '"Synth Mono", monospace', fontWeight: 400, color: '#047857'},
];

function Text() {
  const t = useCurrentFrame() / useVideoConfig().fps;
  let k = 0;
  return (
    <AbsoluteFill style={{background: '#f3efe6', padding: '36px 90px', boxSizing: 'border-box'}}>
      {phrases.map((phrase, line) => (
        <div key={line} style={{whiteSpace: 'nowrap', height: 84, lineHeight: '84px', fontSize: 68, ...lineStyles[line % 4]}}>
          {Array.from(phrase).map((c) => {
            const at = line * 0.09 + (k % 40) * 0.012;
            const out = 3.8 + line * 0.04 + (k % 40) * 0.006;
            k++;
            const enter = Easing.out(Easing.back(2));
            const y = tween(t, at, 0.55, 70, 0, enter) + tween(t, out, 0.4, 0, -40, Easing.in(Easing.quad));
            const opacity = tween(t, at, 0.55, 0, 1, enter) * tween(t, out, 0.4, 1, 0, Easing.in(Easing.quad));
            return (
              <span
                key={k}
                style={{
                  display: 'inline-block',
                  opacity: Math.min(Math.max(opacity, 0), 1),
                  transform: `translateY(${y}px) rotate(${tween(t, at, 0.55, -18, 0, enter)}deg) scale(${tween(t, at, 0.55, 0.6, 1, enter)})`,
                }}
              >
                {c === ' ' ? ' ' : c}
              </span>
            );
          })}
        </div>
      ))}
    </AbsoluteFill>
  );
}

registerRoot(() => <Composition id="text" component={Text} durationInFrames={150} fps={30} width={1920} height={1080} />);
