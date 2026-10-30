import React from 'react';
import { Composition, registerRoot, Sequence, Audio } from 'remotion';

const LONG = `http://assets.invalid/${'a'.repeat(500)}.mp3`;

const Offset = () => (
  <>
    <Sequence from={30}>
      <Audio src="http://assets.invalid/offset.mp3" volume={(f) => 0.25 + f / 100} playbackRate={2} />
    </Sequence>
    <Sequence from={10} durationInFrames={20}>
      <Audio src={LONG} />
    </Sequence>
  </>
);

const Root = () => <Composition id="Offset" component={Offset} width={160} height={160} fps={30} durationInFrames={90} />;

registerRoot(Root);
