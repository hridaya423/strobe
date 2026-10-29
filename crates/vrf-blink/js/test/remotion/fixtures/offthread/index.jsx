import React from 'react';
import { Composition, registerRoot, Sequence, OffthreadVideo, Loop } from 'remotion';

const SRC = 'http://assets.invalid/clip.mp4';

const Rates = () => (
  <>
    <Sequence from={0} durationInFrames={30}>
      <OffthreadVideo src={SRC} playbackRate={0.5} />
    </Sequence>
    <Sequence from={30} durationInFrames={30}>
      <OffthreadVideo src={SRC} playbackRate={2} startFrom={10} />
    </Sequence>
    <Sequence from={60} durationInFrames={30}>
      <OffthreadVideo src={SRC} />
    </Sequence>
  </>
);

const Negative = () => (
  <Sequence from={-20}>
    <OffthreadVideo src={SRC} />
  </Sequence>
);

const Looped = () => (
  <Loop durationInFrames={10}>
    <OffthreadVideo src={SRC} />
  </Loop>
);

const Root = () => (
  <>
    <Composition id="Rates" component={Rates} width={64} height={64} fps={30} durationInFrames={90} />
    <Composition id="Negative" component={Negative} width={64} height={64} fps={30} durationInFrames={40} />
    <Composition id="Looped" component={Looped} width={64} height={64} fps={30} durationInFrames={40} />
  </>
);

registerRoot(Root);
