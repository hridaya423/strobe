import {Composition, registerRoot} from 'remotion';
import {ResScene} from '../lib/ResScene';

registerRoot(() => <Composition id="res4k" component={ResScene} durationInFrames={90} fps={30} width={3840} height={2160} />);
