import {existsSync, readFileSync} from 'node:fs';
import {spawnSync} from 'node:child_process';
import {resolve} from 'node:path';

const input = process.argv[2] ?? 'out/launch-video.mp4';
// Expected length in seconds: 118 for the lessons cut, 93 for EveLaunch.
const expected = Number(process.argv[3] ?? 118);
const file = resolve(process.cwd(), input);
if (!existsSync(file)) {
  console.error(`rendered launch video verification failed: missing ${file}`);
  process.exit(1);
}

const probe = spawnSync('ffprobe', ['-v', 'error', '-show_entries', 'format=duration:stream=codec_type,codec_name,width,height', '-of', 'json', file], {encoding: 'utf8'});
if (probe.status !== 0) {
  console.error(`rendered launch video verification failed: ffprobe exited ${probe.status}`);
  process.exit(1);
}

const data = JSON.parse(probe.stdout);
const duration = Number(data.format?.duration ?? 0);
const streams = data.streams ?? [];
const video = streams.find((s) => s.codec_type === 'video');
const audio = streams.find((s) => s.codec_type === 'audio');
const failures = [];
if (!(duration > expected - 3 && duration <= expected + 2.1)) failures.push(`duration ${duration.toFixed(3)}s is outside ${expected - 3}–${expected + 2.1}s`);
if (!video || video.codec_name !== 'h264' || video.width !== 1920 || video.height !== 1080) failures.push('video stream is not 1920x1080 H.264');
if (!audio || audio.codec_type !== 'audio') failures.push('audio stream is missing');

if (failures.length) {
  console.error(`rendered launch video verification failed: ${failures.join('; ')}`);
  process.exit(1);
}

console.log(`rendered launch video verification passed (${duration.toFixed(2)}s, 1920x1080, H.264 + audio)`);
