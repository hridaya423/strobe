import {Composition, registerRoot} from 'remotion';
import {ResScene} from '../lib/ResScene';

registerRoot(() => <Composition id="res720" component={ResScene} durationInFrames={90} fps={30} width={1280} height={720} />);
