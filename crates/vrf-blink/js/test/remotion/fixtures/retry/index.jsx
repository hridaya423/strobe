import React, { useLayoutEffect } from 'react';
import { Composition, delayRender, registerRoot, useCurrentFrame } from 'remotion';

const Flaky = () => {
  const frame = useCurrentFrame();
  useLayoutEffect(() => {
    if (frame === 3 && window.remotion_attempt === 1) delayRender('flaky', { retries: 1 });
  }, [frame]);
  return <div style={{ width: '100%', height: '100%', background: `rgb(${frame * 10}, 40, ${255 - frame * 10})` }} />;
};

registerRoot(() => <Composition id="Flaky" component={Flaky} width={320} height={180} fps={30} durationInFrames={10} />);
