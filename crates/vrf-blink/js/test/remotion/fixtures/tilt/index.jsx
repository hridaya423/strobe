import React from 'react';
import { Composition, registerRoot, useCurrentFrame } from 'remotion';

const Tilt = () => {
  const settle = Math.min(Math.max(useCurrentFrame() / 20, 0), 1);
  return (
    <div style={{ width: '100%', height: '100%', background: '#f3efe6', padding: 20, boxSizing: 'border-box', fontFamily: 'Arial, sans-serif', fontSize: 40, fontWeight: 800, color: '#14110f', whiteSpace: 'nowrap' }}>
      {Array.from('Subpixel positions blur the stems').map((letter, index) => (
        <span key={index} style={{ display: 'inline-block', transform: `translateY(${30 * (1 - settle)}px) rotate(${-18 * (1 - settle)}deg) scale(${0.6 + 0.4 * settle})` }}>
          {letter === ' ' ? ' ' : letter}
        </span>
      ))}
    </div>
  );
};

registerRoot(() => <Composition id="Tilt" component={Tilt} durationInFrames={45} fps={30} width={640} height={360} />);
