import React from 'react';
import { Composition, registerRoot, useCurrentFrame } from 'remotion';

const Boom = () => {
  const frame = useCurrentFrame();
  if (frame === 5) throw new Error('boom at frame 5');
  return <div style={{ width: '100%', height: '100%', background: `rgb(${frame * 10}, 40, ${255 - frame * 10})` }} />;
};

registerRoot(() => <Composition id="Boom" component={Boom} width={320} height={180} fps={30} durationInFrames={10} />);
