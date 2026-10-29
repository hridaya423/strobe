import {Composition, registerRoot} from 'remotion';
import {ResScene} from '../lib/ResScene';

registerRoot(() => <Composition id="res1080" component={ResScene} durationInFrames={90} fps={30} width={1920} height={1080} />);
