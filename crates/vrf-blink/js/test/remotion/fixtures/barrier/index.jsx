import React, { useLayoutEffect, useState } from 'react';
import { Composition, registerRoot, useCurrentFrame, delayRender, continueRender, cancelRender } from 'remotion';

window.__log = [];

const DELAYS = [40, 90, 160];

const Barrier = () => {
  const frame = useCurrentFrame();
  const [state, setState] = useState({ frame, done: 0 });
  const done = state.frame === frame ? state.done : 0;
  useLayoutEffect(() => {
    const handles = DELAYS.map((ms, i) => {
      const h = delayRender(`barrier-${frame}-${i}`);
      setTimeout(() => {
        setState((s) => ({ frame, done: (s.frame === frame ? s.done : 0) + 1 }));
        continueRender(h);
      }, ms);
      return h;
    });
    return () => handles.length;
  }, [frame]);
  window.__log.push({ frame, done });
  return (
    <div data-frame={frame} data-done={done} style={{ width: '100%', height: '100%', background: done === DELAYS.length ? 'rgb(0, 200, 0)' : 'rgb(200, 0, 0)' }} />
  );
};

const Fonts = () => {
  const frame = useCurrentFrame();
  useLayoutEffect(() => {
    const face = new FontFace('SlowFace', `url(/slow-font.woff2?frame=${frame})`);
    document.fonts.add(face);
    face.load();
  }, [frame]);
  return <div style={{ fontFamily: 'SlowFace, serif', fontSize: 30 }}>fonts {frame}</div>;
};

const Images = () => {
  const frame = useCurrentFrame();
  return (
    <div style={{ width: '100%', height: '100%' }}>
      <img data-img src={`/slow-image.png?frame=${frame}`} style={{ width: 40, height: 40 }} />
    </div>
  );
};

const Never = () => {
  useLayoutEffect(() => {
    delayRender('never cleared');
  }, []);
  return <div />;
};

const Retry = () => {
  useLayoutEffect(() => {
    delayRender('flaky', { retries: 2 });
  }, []);
  return <div />;
};

const Cancel = () => {
  const frame = useCurrentFrame();
  useLayoutEffect(() => {
    if (frame === 2) {
      delayRender('cancel-pending');
      setTimeout(() => cancelRender(new Error('cancelled on purpose')), 5);
    }
  }, [frame]);
  return <div />;
};

const Root = () => (
  <>
    <Composition id="Barrier" component={Barrier} width={64} height={64} fps={30} durationInFrames={20} />
    <Composition id="Fonts" component={Fonts} width={200} height={60} fps={30} durationInFrames={20} />
    <Composition id="Images" component={Images} width={64} height={64} fps={30} durationInFrames={20} />
    <Composition id="Never" component={Never} width={64} height={64} fps={30} durationInFrames={20} />
    <Composition id="Retry" component={Retry} width={64} height={64} fps={30} durationInFrames={20} />
    <Composition id="Cancel" component={Cancel} width={64} height={64} fps={30} durationInFrames={20} />
  </>
);

registerRoot(Root);
