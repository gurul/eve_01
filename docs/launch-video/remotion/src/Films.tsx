import React from 'react';
import {Composition} from 'remotion';
import {RemotionRoot} from './Root';
import {POCKET_FPS, POCKET_FRAMES, PocketLaunch} from './Pocket';

// Every film in this project: the Sep 13 cut (LaunchVideo) and the holistic cut (EveLaunch).
export const Films: React.FC = () => (
  <>
    <RemotionRoot />
    <Composition id="EveLaunch" component={PocketLaunch} durationInFrames={POCKET_FRAMES} fps={POCKET_FPS} width={1920} height={1080} />
  </>
);
