import React from 'react';
import {Composition, registerRoot} from 'remotion';
import {EveEngineering, ENG_FPS, ENG_FRAMES} from './Engineering';

// Entry point for the engineering breakdown only, so the launch films' entry (index.tsx) stays untouched.
//   npx remotion studio src/engineering-index.tsx
const EngineeringRoot: React.FC = () => (
  <Composition id="EveEngineering" component={EveEngineering} durationInFrames={ENG_FRAMES} fps={ENG_FPS} width={1920} height={1080} />
);

registerRoot(EngineeringRoot);
