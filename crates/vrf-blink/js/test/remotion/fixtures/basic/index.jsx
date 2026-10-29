import React from 'react';
import { Composition, registerRoot, useCurrentFrame, getInputProps } from 'remotion';

const Basic = () => {
  const frame = useCurrentFrame();
  return (
    <div
      data-frame={frame}
      style={{ width: '100%', height: '100%', background: `rgb(${frame * 2}, 40, ${255 - frame * 2})`, color: 'white', fontSize: 40 }}
    >
      {frame}
    </div>
  );
};

const Props = (props) => {
  return <div data-props={JSON.stringify(props)} data-input={JSON.stringify(getInputProps())}>{props.label}</div>;
};

const Transparent = () => {
  const frame = useCurrentFrame();
  return (
    <div style={{ width: '100%', height: '100%' }}>
      <div style={{ position: 'absolute', left: 0, top: 0, width: 80, height: 90, background: `rgba(255, 0, 0, 0.5)` }} data-frame={frame} />
    </div>
  );
};

const Root = () => (
  <>
    <Composition id="Basic" component={Basic} width={320} height={180} fps={30} durationInFrames={90} />
    <Composition id="Props" component={Props} width={320} height={180} fps={30} durationInFrames={10} defaultProps={{ label: 'default' }} />
    <Composition id="Transparent" component={Transparent} width={160} height={90} fps={30} durationInFrames={10} />
    <Composition
      id="Meta"
      component={Basic}
      width={100}
      height={100}
      fps={25}
      durationInFrames={5}
      defaultProps={{ w: 200, h: 120, d: 12, fps: 24 }}
      calculateMetadata={({ props }) => ({ width: props.w, height: props.h, durationInFrames: props.d, fps: props.fps })}
    />
  </>
);

registerRoot(Root);
