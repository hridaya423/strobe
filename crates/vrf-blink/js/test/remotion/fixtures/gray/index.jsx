import React from 'react';
import { Composition, registerRoot, useCurrentFrame } from 'remotion';

const Gray = () => {
  const frame = useCurrentFrame();
  return (
    <div style={{ width: '100%', height: '100%', background: 'linear-gradient(90deg, #000, #fff)', position: 'relative', overflow: 'hidden' }}>
      <div
        style={{
          position: 'absolute',
          left: frame * 5,
          top: 20,
          width: 60,
          height: 60,
          background: frame % 2 ? '#fff' : '#000',
          transform: `rotate(${frame * 3}deg)`,
        }}
      />
      <div style={{ position: 'absolute', left: 20, top: 110, fontFamily: 'sans-serif', fontSize: 44, color: '#888' }}>frame {frame}</div>
    </div>
  );
};

registerRoot(() => <Composition id="Gray" component={Gray} width={320} height={180} fps={30} durationInFrames={60} />);
