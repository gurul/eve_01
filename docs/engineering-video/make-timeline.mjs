// Builds the engineering film's timeline from its narration.
//
//   node docs/engineering-video/make-timeline.mjs
//
// Reads  docs/launch-video/remotion/src/engineering/script.json   (the narration, one cue per caption)
// Writes docs/launch-video/remotion/src/engineering/timeline.json (frame timings the composition reads)
//        docs/engineering-video/eve-engineering.srt             (the subtitle track)
//
// One source of truth: the composition, the burned-in captions and the .srt all come
// from the same cue list, so they cannot drift apart.

import {readFileSync, writeFileSync} from 'node:fs';
import {dirname, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const src = resolve(here, '../launch-video/remotion/src/engineering');
const script = JSON.parse(readFileSync(resolve(src, 'script.json'), 'utf8'));

const FPS = 30;
const WORDS_PER_SECOND = 2.9; // caption reading pace (about 175 words a minute)
const CUE_GAP = 0.35;         // breath between captions, seconds
const MIN_CUE = 2.2;          // a short cue still stays up long enough to read
const SECTION_LEAD = 1.4;     // title card before the first caption of a section
const SECTION_TAIL = 1.0;     // hold after the last caption

let frame = 0;
const sections = [];
for (const s of script.sections) {
  const from = frame;
  let t = Math.round(SECTION_LEAD * FPS);
  const cues = [];
  for (const text of s.cues) {
    const words = text.trim().split(/\s+/).length;
    const dur = Math.round(Math.max(MIN_CUE, words / WORDS_PER_SECOND) * FPS);
    cues.push({text, from: t, duration: dur});
    t += dur + Math.round(CUE_GAP * FPS);
  }
  const duration = t + Math.round(SECTION_TAIL * FPS);
  sections.push({id: s.id, from, duration, cues});
  frame += duration;
}

const timeline = {fps: FPS, totalFrames: frame, sections};
writeFileSync(resolve(src, 'timeline.json'), JSON.stringify(timeline, null, 2) + '\n');

const stamp = (f) => {
  const ms = Math.round((f / FPS) * 1000);
  const h = Math.floor(ms / 3600000);
  const m = Math.floor((ms % 3600000) / 60000);
  const sec = Math.floor((ms % 60000) / 1000);
  const r = ms % 1000;
  const p = (n, w = 2) => String(n).padStart(w, '0');
  return `${p(h)}:${p(m)}:${p(sec)},${p(r, 3)}`;
};

let n = 1;
const srt = [];
for (const s of sections) {
  for (const c of s.cues) {
    const a = s.from + c.from;
    srt.push(`${n++}\n${stamp(a)} --> ${stamp(a + c.duration)}\n${c.text}\n`);
  }
}
writeFileSync(resolve(here, 'eve-engineering.srt'), srt.join('\n'));

const mmss = (f) => {
  const s = Math.round(f / FPS);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
};
const words = script.sections.flatMap((s) => s.cues).join(' ').split(/\s+/).length;
console.log(`timeline: ${sections.length} sections, ${n - 1} cues, ${words} words, ${frame} frames = ${(frame / FPS).toFixed(1)} s`);
for (const [i, s] of sections.entries()) {
  console.log(`  ${String(i + 1).padStart(2)}  ${mmss(s.from).padStart(5)}  ${mmss(s.duration).padStart(5)}  ${s.id}`);
}
