import {existsSync, readFileSync, statSync} from 'node:fs';
import {resolve} from 'node:path';

const root = resolve(new URL('..', import.meta.url).pathname);
const source = readFileSync(resolve(root, 'src/Root.tsx'), 'utf8');
const pkg = JSON.parse(readFileSync(resolve(root, 'package.json'), 'utf8'));

const required = [
  ['Remotion 4 dependency', pkg.dependencies?.remotion === '4.0.524'],
  ['main composition', source.includes('id="LaunchVideo"')],
  ['118-second duration', source.includes('const DURATION_SECONDS = 118')],
  ['instrumental audio', source.includes("audio/eve-launch.mp3")],
  ['local fonts', source.includes("fonts/grandstander-700-900-latin.woff2") && source.includes("fonts/andika-400-latin.woff2")],
  ['problem-first scene', source.includes('an answer<br/>isn\'t enough.')],
  ['show-and-tell system diagram', source.includes('show & tell · under the hood')],
  ['gpt-live-1', source.includes('gpt-live-1')],
  ['gpt-6-astra', source.includes('gpt-6-astra')],
  ['gpt-5.4-nano', source.includes('gpt-5.4-nano')],
  ['Exa', source.includes('Exa')],
  ['teacher guardrail', source.includes('never on the menu')],
  ['robot drawing', source.includes('function Robot(')],
  ['no old readme photo', !source.includes('hero.png')],
];

const failures = required.filter(([, ok]) => !ok).map(([name]) => name);
const audio = resolve(root, 'public/audio/eve-launch.mp3');
if (!existsSync(audio) || statSync(audio).size < 10000) failures.push('trimmed MP3 asset');

if (failures.length) {
  console.error(`launch video source verification failed: ${failures.join(', ')}`);
  process.exit(1);
}

console.log('launch video source verification passed');
