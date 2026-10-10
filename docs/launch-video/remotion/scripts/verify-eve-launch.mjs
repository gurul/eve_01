import {existsSync, readFileSync, statSync} from 'node:fs';
import {resolve} from 'node:path';

// Source checks for the holistic cut (EveLaunch, src/Pocket.tsx).
const root = resolve(new URL('..', import.meta.url).pathname);
const pocket = readFileSync(resolve(root, 'src/Pocket.tsx'), 'utf8');
const films = readFileSync(resolve(root, 'src/Films.tsx'), 'utf8');
const index = readFileSync(resolve(root, 'src/index.tsx'), 'utf8');

const scenes = ['hello', 'senses', 'life', 'pocket', 'chrome', 'routes', 'coding', 'brain', 'orbit', 'outro'];
const required = [
  ['EveLaunch registered', films.includes('id="EveLaunch"') && index.includes('registerRoot(Films)')],
  ['lessons cut still registered', films.includes('<RemotionRoot />')],
  ['all ten chapters, in order', scenes.every((n, i) => pocket.indexOf(`name: '${n}'`) > (i ? pocket.indexOf(`name: '${scenes[i - 1]}'`) : -1))],
  ['music bed', pocket.includes("audio/eve-launch.mp3")],
  ['opt-in tags on the off-by-default features', (pocket.match(/>opt-in</g) ?? []).length >= 3],
  ['lessons are one chip, not the story', (pocket.match(/lesson/gi) ?? []).length <= 3],
  ['repo link', pocket.includes('github.com/gurul/eve')],
];

const failures = required.filter(([, ok]) => !ok).map(([name]) => name);
for (const f of ['public/audio/sfx-wake.wav', 'public/audio/sfx-pop.wav', 'public/audio/sfx-happy.wav', 'public/audio/sfx-ok.wav', 'public/assets/widget-notes-medium.png']) {
  const p = resolve(root, f);
  if (!existsSync(p) || statSync(p).size < 1000) failures.push(f);
}

if (failures.length) {
  console.error(`EveLaunch source verification failed: ${failures.join(', ')}`);
  process.exit(1);
}
console.log('EveLaunch source verification passed');
