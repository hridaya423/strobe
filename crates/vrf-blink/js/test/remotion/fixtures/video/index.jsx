import React from 'react';
import { AbsoluteFill, Composition, OffthreadVideo, registerRoot, staticFile } from 'remotion';

const Clip = ({ src }) => (
  <AbsoluteFill>
    <OffthreadVideo src={staticFile(src)} />
  </AbsoluteFill>
);

const Rates = () => (
  <AbsoluteFill style={{ background: '#101820' }}>
    <OffthreadVideo src={staticFile('cfr1080.mp4')} playbackRate={0.5} style={{ position: 'absolute', left: 0, top: 0, width: 960, height: 540 }} />
    <OffthreadVideo src={staticFile('vfr1080.mp4')} playbackRate={2} startFrom={10} style={{ position: 'absolute', left: 960, top: 0, width: 960, height: 540 }} />
  </AbsoluteFill>
);

const Two = () => (
  <AbsoluteFill style={{ background: '#123456' }}>
    <OffthreadVideo src={staticFile('cfr1080.mp4')} style={{ position: 'absolute', left: 40, top: 40, width: 960, height: 540 }} />
    <OffthreadVideo
      src={staticFile('vfr1080.mp4')}
      startFrom={5}
      style={{ position: 'absolute', left: 1000, top: 300, width: 640, height: 360, transform: 'rotate(8deg) scale(1.1)', borderRadius: 24 }}
    />
  </AbsoluteFill>
);

const clips = [
  ['Cfr', 'cfr1080.mp4', 120],
  ['Vfr', 'vfr1080.mp4', 166],
  ['Offset', 'off1080.mp4', 165],
  ['Hevc', 'cfr1080-hevc.mp4', 60],
  ['Bt601', 'cfr1080-601.mp4', 30],
  ['Full', 'cfr1080-full.mp4', 30],
];

const Root = () => (
  <>
    {clips.map(([id, src, durationInFrames]) => (
      <Composition key={id} id={id} component={Clip} defaultProps={{ src }} durationInFrames={durationInFrames} fps={30} width={1920} height={1080} />
    ))}
    <Composition id="Rates" component={Rates} durationInFrames={90} fps={30} width={1920} height={1080} />
    <Composition id="Two" component={Two} durationInFrames={90} fps={30} width={1920} height={1080} />
  </>
);

registerRoot(Root);
