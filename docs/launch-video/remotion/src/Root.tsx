import React, {useId} from 'react';
import {
  AbsoluteFill,
  Audio,
  Composition,
  Easing,
  Img,
  Sequence,
  interpolate,
  spring,
  staticFile,
  useCurrentFrame,
  useVideoConfig,
} from 'remotion';

/*
 * eve launch film — 118 s, 1920×1080, 30 fps.
 *
 * Motion system (every animation is a pure function of the frame, so renders are
 * deterministic):
 *   - Kinetic type: headings rise word by word with a spring; kickers write on
 *     left to right like handwriting.
 *   - Cut-paper background: each shape drifts on its own phase, breathes, and
 *     slides in from its own edge when a scene starts.
 *   - Camera: every scene pushes in about 4% over its length.
 *   - Transitions: a tilted paper sheet in the next scene's accent colour wipes
 *     across the seam.
 *   - Cards: spring in with overshoot, their offset shadow settles, then they
 *     float on a slow sine.
 *   - Arrows draw on; pipeline nodes light up in order.
 *   - The robot bobs, tilts, blinks with a squash, types its screen text, chirps
 *     rings when it wakes, and pulses a ring while it listens.
 */

const FPS = 30;
const DURATION_SECONDS = 118;
const TOTAL_FRAMES = DURATION_SECONDS * FPS;

export const C = {
  paper: '#FBF5EC',
  sheet: '#FFFDF8',
  ink: '#1F3A78',
  inkSoft: '#3E4F82',
  pink: '#E68AAE',
  pinkSoft: '#F4C3D4',
  teal: '#79C6B2',
  sun: '#F1C85B',
  sky: '#7FA2DE',
  desk: '#E9E1D3',
  screen: '#1B2350',
  screenPink: '#FF74D4',
  purple: '#A487D0',
};

export type CSS = React.CSSProperties;

export const fontFace = `
  @font-face { font-family: Grandstander; src: url(${staticFile('fonts/grandstander-700-900-latin.woff2')}) format('woff2'); font-weight: 700 900; }
  @font-face { font-family: Andika; src: url(${staticFile('fonts/andika-400-latin.woff2')}) format('woff2'); font-weight: 400; }
  @font-face { font-family: Andika; src: url(${staticFile('fonts/andika-700-latin.woff2')}) format('woff2'); font-weight: 700; }
  @font-face { font-family: Gochi Hand; src: url(${staticFile('fonts/gochi-hand-400-latin.woff2')}) format('woff2'); font-weight: 400; }
  @font-face { font-family: VT323; src: url(${staticFile('fonts/vt323-400-latin.woff2')}) format('woff2'); font-weight: 400; }
`;

// ---------- motion helpers ----------

export const SPRINGS = {
  pop: {damping: 11, stiffness: 170, mass: 0.6},     // overshoots, for pills and buttons
  rise: {damping: 15, stiffness: 120, mass: 0.7},    // cards and text
  soft: {damping: 22, stiffness: 90, mass: 1},       // big things, no overshoot
};

export function sp(frame: number, fps: number, delay = 0, config = SPRINGS.rise, duration?: number) {
  return spring({frame: Math.max(0, frame - delay), fps, config, durationInFrames: duration});
}

export function fade(frame: number, start: number, end: number, easing = Easing.out(Easing.cubic)) {
  return interpolate(frame, [start, end], [0, 1], {extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing});
}

export function wave(frame: number, amp = 8, speed = 0.055, phase = 0) {
  return Math.sin(frame * speed + phase) * amp;
}

// A one-off bump: 0 → 1 → 0 over `length` frames starting at `at`.
export function bump(frame: number, at: number, length = 14) {
  const t = (frame - at) / length;
  if (t <= 0 || t >= 1) return 0;
  return Math.sin(t * Math.PI);
}

// Deterministic pseudo-random in [0, 1) from an integer seed.
function rnd(seed: number) {
  const x = Math.sin(seed * 12.9898 + 78.233) * 43758.5453;
  return x - Math.floor(x);
}

export function useT() {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();
  return {frame, fps};
}

// ---------- background ----------

type Shape = {d: string; fill: string; from: [number, number]; phase: number; speed: number; amp: number; spin: number; stroke?: string; dash?: string};

const SHAPES: Shape[] = [
  {d: 'circle:40,60,190', fill: C.sky, from: [-260, -220], phase: 0.2, speed: 0.021, amp: 14, spin: 0},
  {d: 'ring:40,60,128', fill: 'none', stroke: '#9BB6E6', from: [-260, -220], phase: 0.2, speed: 0.021, amp: 14, spin: 0},
  {d: 'M-20 200C130 165 175 290 120 410C80 500 10 470-40 420Z', fill: C.teal, from: [-260, 0], phase: 1.1, speed: 0.018, amp: 10, spin: 0.02},
  {d: 'poly:126,118 205,42 230,94 292,30 320,80 372,28 350,74 285,132 260,82 206,145 179,101 120,150', fill: C.sun, from: [-120, -300], phase: 2.3, speed: 0.03, amp: 12, spin: 0.05},
  {d: 'M1670 -50H1950V270C1874 292 1772 260 1726 196C1690 142 1650 40 1670 -50Z', fill: C.pink, from: [320, -320], phase: 0.7, speed: 0.016, amp: 10, spin: 0, stroke: '#D97A9E', dash: '4 22'},
  {d: 'poly:1760,830 1920,786 1920,1080 1802,1080', fill: C.sun, from: [300, 300], phase: 1.9, speed: 0.024, amp: 9, spin: 0, stroke: '#E7B44A', dash: '3 22'},
  {d: 'M820 1030C940 900 1110 878 1220 945L1185 1000C1070 954 964 970 885 1080Z', fill: C.pink, from: [0, 320], phase: 2.8, speed: 0.02, amp: 12, spin: 0.03},
  {d: 'M1460 720l18 42 46 4-35 30 10 45-39-24-39 24 10-45-35-30 46-4z', fill: C.pink, from: [200, 260], phase: 3.4, speed: 0.04, amp: 16, spin: 0.35},
];

function ShapePath({s, i, accent}: {s: Shape; i: number; accent: string}) {
  const {frame, fps} = useT();
  const enter = sp(frame, fps, 2 + i * 2, SPRINGS.soft, 40);
  const dx = s.from[0] * (1 - enter) + wave(frame, s.amp, s.speed, s.phase);
  const dy = s.from[1] * (1 - enter) + Math.cos(frame * s.speed * 0.9 + s.phase) * s.amp * 0.7;
  const rot = wave(frame, s.spin * 20, s.speed * 0.6, s.phase);
  const breathe = 1 + Math.sin(frame * 0.02 + s.phase) * 0.015;
  const common = {fill: s.fill, stroke: s.stroke, strokeWidth: s.stroke ? 12 : undefined, strokeDasharray: s.dash, strokeDashoffset: s.dash ? -frame * 0.8 : undefined};
  let el: React.ReactNode;
  if (s.d.startsWith('circle:')) {
    const [cx, cy, r] = s.d.slice(7).split(',').map(Number);
    el = <circle cx={cx} cy={cy} r={r} {...common} />;
  } else if (s.d.startsWith('ring:')) {
    const [cx, cy, r] = s.d.slice(5).split(',').map(Number);
    el = <circle cx={cx} cy={cy} r={r} fill="none" stroke={s.stroke} strokeWidth={22} />;
  } else if (s.d.startsWith('poly:')) {
    el = <polygon points={s.d.slice(5)} {...common} />;
  } else {
    el = <path d={s.d} {...common} />;
  }
  const cx = i === 7 ? 1460 : 960;
  const cy = i === 7 ? 765 : 540;
  return (
    <g transform={`translate(${dx} ${dy}) rotate(${rot} ${cx} ${cy}) translate(${cx} ${cy}) scale(${breathe}) translate(${-cx} ${-cy})`} opacity={i === 7 ? 0.55 : 1}>
      {el}
      {i === 0 && <circle cx="1500" cy="820" r="88" fill={accent} opacity={0.15 + Math.sin(frame * 0.05) * 0.06} />}
    </g>
  );
}

function PaperBackground({accent}: {accent: string}) {
  return (
    <AbsoluteFill style={{background: C.paper, color: C.ink, overflow: 'hidden'}}>
      <svg width="1920" height="1080" viewBox="0 0 1920 1080" style={{position: 'absolute', inset: 0}}>
        {SHAPES.map((s, i) => <ShapePath key={i} s={s} i={i} accent={accent} />)}
      </svg>
    </AbsoluteFill>
  );
}

// Camera push: the whole scene scales 1 → 1.04 and drifts up 12 px over its life.
export function SceneFrame({children, accent, duration}: {children: React.ReactNode; accent: string; duration: number}) {
  const frame = useCurrentFrame();
  const t = interpolate(frame, [0, duration], [0, 1], {extrapolateRight: 'clamp'});
  const scale = 1 + t * 0.04;
  const y = -t * 12;
  return (
    <AbsoluteFill style={{fontFamily: 'Andika', background: C.paper}}>
      <div style={{position: 'absolute', inset: 0, transform: `scale(${1 + t * 0.06})`, transformOrigin: '50% 50%'}}>
        <PaperBackground accent={accent} />
      </div>
      <div style={{position: 'absolute', inset: 0, padding: '70px 100px', zIndex: 1, transform: `translateY(${y}px) scale(${scale})`, transformOrigin: '50% 55%'}}>{children}</div>
    </AbsoluteFill>
  );
}

// Tilted paper sheet sweeping left → right across a scene seam. Placed in its own
// 32-frame Sequence centred on the cut, so the seam is covered at local frame 16.
export function Wipe({color}: {color: string}) {
  const frame = useCurrentFrame();
  const x = interpolate(frame, [0, 32], [-3400, 3400], {easing: Easing.inOut(Easing.cubic), extrapolateLeft: 'clamp', extrapolateRight: 'clamp'});
  return (
    <AbsoluteFill style={{pointerEvents: 'none', zIndex: 50}}>
      <div style={{position: 'absolute', left: -600, top: -400, width: 3100, height: 1900, background: color, transform: `translateX(${x}px) rotate(-7deg)`, boxShadow: `-18px 0 0 ${C.ink}`}} />
    </AbsoluteFill>
  );
}

// ---------- type ----------

export function Words({text, delay = 0, step = 3, style, config = SPRINGS.rise, shadow = 6}: {text: string; delay?: number; step?: number; style?: CSS; config?: typeof SPRINGS.rise; shadow?: number}) {
  const {frame, fps} = useT();
  const lines = text.split('<br/>');
  let n = 0;
  return (
    <span style={style}>
      {lines.map((line, li) => (
        <span key={li} style={{display: 'block', whiteSpace: 'nowrap'}}>
          {line.split(' ').map((w, wi) => {
            const a = sp(frame, fps, delay + n++ * step, config, 26);
            return (
              <span key={wi} style={{display: 'inline-block', marginRight: '0.26em', opacity: a, transform: `translateY(${(1 - a) * 34}px) rotate(${(1 - a) * -4}deg)`, textShadow: shadow ? `${shadow * a}px ${shadow * a}px 0 ${C.pinkSoft}` : undefined}}>
                {w}
              </span>
            );
          })}
        </span>
      ))}
    </span>
  );
}

export function H1({text, size = 72, delay = 0, style}: {text: string; size?: number; delay?: number; style?: CSS}) {
  return (
    <div style={{font: `900 ${size}px/0.98 Grandstander`, letterSpacing: '-0.035em', color: C.ink, ...style}}>
      <Words text={text} delay={delay} />
    </div>
  );
}

// Handwriting write-on: a clip reveals the text left to right.
export function Kicker({children, color = C.inkSoft, delay = 0, size = 33, style}: {children: React.ReactNode; color?: string; delay?: number; size?: number; style?: CSS}) {
  const frame = useCurrentFrame();
  const p = fade(frame, delay, delay + 22, Easing.inOut(Easing.quad));
  return (
    <div style={{font: `400 ${size}px/1.05 Gochi Hand`, color, marginBottom: 10, clipPath: `inset(-10% ${(1 - p) * 100}% -10% 0)`, ...style}}>{children}</div>
  );
}

export function Body({children, size = 25, delay = 0, style}: {children: React.ReactNode; size?: number; delay?: number; style?: CSS}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, delay, SPRINGS.soft, 30);
  return <div style={{font: `400 ${size}px/1.28 Andika`, color: C.ink, opacity: a, transform: `translateY(${(1 - a) * 18}px)`, ...style}}>{children}</div>;
}

// ---------- objects ----------

export function Card({children, color = C.sky, delay = 0, from = 'below', float = true, style, inner}: {children: React.ReactNode; color?: string; delay?: number; from?: 'below' | 'left' | 'right'; float?: boolean; style?: CSS; inner?: CSS}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, delay, SPRINGS.rise, 30);
  const off = from === 'left' ? -260 : from === 'right' ? 260 : 0;
  const dy = from === 'below' ? 60 : 0;
  const fl = float ? wave(frame, 4, 0.05, delay) : 0;
  const tilt = (1 - a) * (from === 'right' ? 4 : -3);
  return (
    <div style={style}>
      <div
        style={{
          background: C.sheet,
          border: `3px solid ${C.ink}`,
          borderRadius: 8,
          boxShadow: `${10 * a}px ${10 * a}px 0 ${color}`,
          opacity: a,
          transform: `translate(${off * (1 - a)}px, ${dy * (1 - a) + fl}px) rotate(${tilt + (float ? wave(frame, 0.35, 0.04, delay) : 0)}deg)`,
          ...inner,
        }}
      >
        {children}
      </div>
    </div>
  );
}

export function Tag({children, color = C.teal, delay = 0}: {children: React.ReactNode; color?: string; delay?: number}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, delay, SPRINGS.pop, 24);
  return (
    <span style={{display: 'inline-block', padding: '10px 16px', border: `3px solid ${C.ink}`, borderRadius: 999, background: color, font: '700 18px/1 Andika', letterSpacing: '0.06em', textTransform: 'uppercase', color: C.ink, opacity: Math.min(1, a * 1.4), transform: `scale(${0.4 + 0.6 * a}) rotate(${(1 - a) * 8}deg)`}}>
      {children}
    </span>
  );
}

export function Pill({children, color, delay = 0, size = 21, lit = false, style}: {children: React.ReactNode; color: string; delay?: number; size?: number; lit?: boolean; style?: CSS}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, delay, SPRINGS.pop, 22);
  return (
    <span style={{display: 'inline-block', padding: `${size * 0.55}px ${size * 0.85}px`, border: `3px solid ${C.ink}`, borderRadius: 999, background: color, font: `700 ${size}px/1 Andika`, color: C.ink, opacity: Math.min(1, a * 1.4), transform: `scale(${0.5 + 0.5 * a}) translateY(${(1 - a) * 10}px)`, boxShadow: lit ? `3px 3px 0 ${C.ink}` : undefined, ...style}}>
      {children}
    </span>
  );
}

// A pipeline node that pops in, then "lights up" (a scale bump and an ink shadow) at `litAt`.
export function Node({children, color, delay = 0, litAt, style}: {children: React.ReactNode; color: string; delay?: number; litAt?: number; style?: CSS}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, delay, SPRINGS.pop, 22);
  const b = litAt === undefined ? 0 : bump(frame, litAt, 16);
  const held = litAt !== undefined && frame >= litAt ? 1 : 0;
  return (
    <div style={{padding: '15px 20px', border: `3px solid ${C.ink}`, borderRadius: 10, background: color, font: '700 22px Andika', color: C.ink, opacity: Math.min(1, a * 1.4), transform: `scale(${(0.6 + 0.4 * a) * (1 + b * 0.1)})`, boxShadow: `${4 * held}px ${4 * held}px 0 ${C.ink}`, ...style}}>
      {children}
    </div>
  );
}

export function Arrow({color = C.ink, direction = 'right', width = 118, delay = 0}: {color?: string; direction?: 'right' | 'down' | 'left'; width?: number; delay?: number}) {
  const frame = useCurrentFrame();
  const p = fade(frame, delay, delay + 16, Easing.out(Easing.quad));
  const head = fade(frame, delay + 10, delay + 18, Easing.out(Easing.back(2)));
  const vertical = direction === 'down';
  const reverse = direction === 'left';
  const len = vertical ? 42 : width - 21;
  return (
    <div style={{width: vertical ? 48 : width, height: vertical ? 54 : 38, display: 'flex', alignItems: 'center', justifyContent: 'center', transform: reverse ? 'scaleX(-1)' : undefined}}>
      {vertical ? (
        <svg width="30" height="54" viewBox="0 0 30 54">
          <path d="M15 2v42" fill="none" stroke={color} strokeWidth="4" strokeLinecap="round" strokeDasharray={len} strokeDashoffset={len * (1 - p)} />
          <path d="M4 33l11 14 11-14" fill="none" stroke={color} strokeWidth="4" strokeLinecap="round" strokeLinejoin="round" opacity={head} transform={`translate(0 ${(1 - head) * -8})`} />
        </svg>
      ) : (
        <svg width={width} height="28" viewBox={`0 0 ${width} 28`}>
          <path d={`M3 14H${width - 18}`} fill="none" stroke={color} strokeWidth="4" strokeLinecap="round" strokeDasharray={len} strokeDashoffset={len * (1 - p)} />
          <path d={`M${width - 30} 4l12 10-12 10`} fill="none" stroke={color} strokeWidth="4" strokeLinecap="round" strokeLinejoin="round" opacity={head} transform={`translate(${(1 - head) * -8} 0)`} />
        </svg>
      )}
    </div>
  );
}

// Expanding rings, for a chirp.
export function Chirp({at, x = 0, y = 0, color = C.pink}: {at: number; x?: number; y?: number; color?: string}) {
  const frame = useCurrentFrame();
  return (
    <div style={{position: 'absolute', left: x, top: y, width: 0, height: 0, pointerEvents: 'none'}}>
      {[0, 1, 2].map((i) => {
        const p = fade(frame, at + i * 5, at + i * 5 + 22, Easing.out(Easing.quad));
        const r = 20 + p * 90;
        return <div key={i} style={{position: 'absolute', left: -r, top: -r, width: r * 2, height: r * 2, borderRadius: '50%', border: `4px solid ${color}`, opacity: (1 - p) * (p > 0 ? 1 : 0)}} />;
      })}
    </div>
  );
}

export function Robot({message = 'hey!', scale = 1, delay = 0, screenPink = C.screenPink, listening = false, chirp = true, type = true}: {message?: string; scale?: number; delay?: number; screenPink?: string; listening?: boolean; chirp?: boolean; type?: boolean}) {
  const {frame, fps} = useT();
  const id = useId().replace(/:/g, '');
  const a = sp(frame, fps, delay, SPRINGS.pop, 30);
  const bob = wave(frame, 5, 0.06);
  const tilt = wave(frame, 2.2, 0.035, 1.3);
  const blinkT = frame % 108;
  const blink = blinkT > 96 && blinkT < 102 ? 0.18 : 1;
  const safe = message.length > 16 ? `${message.slice(0, 15)}…` : message;
  const shown = type ? Math.max(0, Math.floor((frame - delay - 14) / 2)) : safe.length;
  const typed = safe.slice(0, shown);
  const typing = shown < safe.length;
  const cursor = typing && frame % 10 < 6 ? '▌' : '';
  const ring = listening ? (frame % 40) / 40 : 0;
  return (
    <div style={{width: 250 * scale, position: 'relative', opacity: Math.min(1, a * 1.5), transform: `translateY(${(1 - a) * 90 + bob}px) scale(${0.6 + 0.4 * a})`, transformOrigin: '50% 100%'}}>
      {chirp && <Chirp at={delay + 16} x={125 * scale} y={120 * scale} color={screenPink} />}
      {listening && (
        <div style={{position: 'absolute', left: 125 * scale - 150 * scale * (1 + ring), top: 120 * scale - 150 * scale * (1 + ring), width: 300 * scale * (1 + ring), height: 300 * scale * (1 + ring), borderRadius: '50%', border: `${5 * scale}px solid ${C.sky}`, opacity: (1 - ring) * 0.6}} />
      )}
      <svg width={250 * scale} height={300 * scale} viewBox="0 0 200 240" style={{display: 'block', overflow: 'visible', position: 'relative'}}>
        <defs>
          <linearGradient id={`lbl-${id}`} x1="0" x2="1" y1="0" y2="1"><stop offset="0" stopColor="#9DB3EE" /><stop offset="1" stopColor="#B98BD8" /></linearGradient>
        </defs>
        <rect x="68" y="168" width="64" height="30" fill="#8C90A0" stroke="#4D5160" strokeWidth="2.5" />
        <rect x="46" y="194" width="108" height="38" rx="7" fill="#6F7384" stroke="#4D5160" strokeWidth="2.5" />
        <circle cx="64" cy="220" r="5" fill="#4D5160" /><circle cx="136" cy="220" r="5" fill="#4D5160" />
        <g transform={`rotate(${tilt} 100 170) translate(0 ${Math.sin(frame * 0.045) * 1.5})`}>
          <rect x="22" y="10" width="156" height="34" rx="7" fill={`url(#lbl-${id})`} stroke="#5E5F98" strokeWidth="2.5" />
          <text x="100" y="35" textAnchor="middle" fontFamily="Grandstander" fontWeight="700" fontSize="22" fill="#F4F4FB">eve</text>
          <rect x="12" y="42" width="176" height="14" rx="4" fill="#A487D0" stroke="#5E5F98" strokeWidth="2.5" />
          <rect x="30" y="52" width="140" height="122" rx="14" fill="#C3C6CF" stroke="#5C6070" strokeWidth="3" />
          <rect x="42" y="62" width="116" height="100" rx="6" fill={C.screen} />
          {listening && <rect x="42" y="62" width="116" height="100" rx="6" fill={C.sky} opacity={0.12 + Math.sin(frame * 0.2) * 0.08} />}
          <g fill={screenPink} transform={`translate(0 ${blink === 1 ? 0 : 7}) scale(1 ${blink})`}>
            <path d="M72 75H88V80H92V94H87V89H73V94H68V80H72Z M112 75H128V80H132V94H127V89H113V94H108V80H112Z" />
          </g>
          <text x="100" y="134" textAnchor="middle" fontFamily="VT323" fontSize="22" fill={screenPink}>{typed}{cursor}</text>
        </g>
      </svg>
    </div>
  );
}

export function BrowserWindow({children, style, title = "eve · learning"}: {children: React.ReactNode; style?: CSS; title?: string}) {
  return (
    <div style={{background: C.sheet, border: `3px solid ${C.ink}`, borderRadius: 12, boxShadow: `12px 12px 0 ${C.sky}`, overflow: 'hidden', ...style}}>
      <div style={{height: 42, background: C.pinkSoft, borderBottom: `3px solid ${C.ink}`, display: 'flex', alignItems: 'center', gap: 10, padding: '0 16px'}}>
        <span style={{width: 13, height: 13, borderRadius: '50%', background: C.pink, border: `2px solid ${C.ink}`}} />
        <span style={{width: 13, height: 13, borderRadius: '50%', background: C.sun, border: `2px solid ${C.ink}`}} />
        <span style={{width: 13, height: 13, borderRadius: '50%', background: C.teal, border: `2px solid ${C.ink}`}} />
        <span style={{marginLeft: 8, font: '700 15px Andika', color: C.inkSoft}}>{title}</span>
      </div>
      {children}
    </div>
  );
}

export function Speech({children, color, delay = 0, tilt = -1, style}: {children: React.ReactNode; color: string; delay?: number; tilt?: number; style?: CSS}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, delay, SPRINGS.pop, 26);
  return (
    <div style={{padding: '18px 22px', border: `3px solid ${C.ink}`, borderRadius: 999, background: color, font: '400 28px Gochi Hand', color: C.ink, opacity: Math.min(1, a * 1.4), transform: `rotate(${tilt + wave(frame, 0.8, 0.07, delay)}deg) scale(${0.5 + 0.5 * a})`, transformOrigin: '10% 100%', ...style}}>
      {children}
    </div>
  );
}

// ---------- scenes ----------

function ProblemScene() {
  const frame = useCurrentFrame();
  const line = fade(frame, 118, 142);
  const draw = fade(frame, 130, 160, Easing.inOut(Easing.quad));
  return (
    <SceneFrame accent={C.pink} duration={360}>
      <div style={{position: 'absolute', left: 112, top: 84}}><Tag color={C.pinkSoft} delay={2}>the problem</Tag></div>
      <div style={{position: 'absolute', left: 105, top: 178, width: 920}}>
        <Kicker delay={8}>When you are stuck…</Kicker>
        <H1 size={92} delay={14} text="an answer<br/>isn't enough." />
        <Body size={29} delay={44} style={{marginTop: 28, maxWidth: 820}}>
          A search result can be right. A busy day can still be hard.<br />
          What helps is knowing what to do next.
        </Body>
      </div>
      <Card color={C.sun} delay={30} from="right" style={{position: 'absolute', left: 1080, top: 118, width: 660}} inner={{minHeight: 270, padding: 34}}>
        <Kicker size={28} delay={44}>at the desk</Kicker>
        <div style={{font: '700 34px/1.08 Grandstander', color: C.ink}}>A hundred tiny asks.</div>
        <Speech color={C.sun} delay={56} tilt={-1} style={{marginTop: 26}}>“eve, where is that file?”</Speech>
      </Card>
      <Card color={C.teal} delay={52} from="right" style={{position: 'absolute', left: 1180, top: 456, width: 620}} inner={{minHeight: 290, padding: 34}}>
        <Kicker size={28} delay={66}>in the lesson</Kicker>
        <div style={{font: '700 34px/1.08 Grandstander', color: C.ink}}>A learner needs the next thought.</div>
        <Speech color={C.teal} delay={80} tilt={1} style={{marginTop: 26}}>“where do I start?”</Speech>
      </Card>
      <div style={{position: 'absolute', left: 110, bottom: 90, font: '400 34px Gochi Hand', color: C.ink, opacity: line, transform: `translateX(${(1 - line) * -40}px)`}}>
        Meet the eve that can do both.
      </div>
      <div style={{position: 'absolute', right: 130, bottom: 78, transform: `rotate(${-8 + wave(frame, 2, 0.08)}deg)`, opacity: fade(frame, 128, 134)}}>
        <svg width="170" height="100" viewBox="0 0 170 100">
          <path d="M8 85C48 72 95 43 146 12" fill="none" stroke={C.ink} strokeWidth="5" strokeLinecap="round" strokeDasharray="170" strokeDashoffset={170 * (1 - draw)} />
          <path d="M124 12l24-1-8 23" fill="none" stroke={C.ink} strokeWidth="5" strokeLinecap="round" strokeLinejoin="round" opacity={fade(frame, 154, 162)} />
        </svg>
      </div>
    </SceneFrame>
  );
}

function Callout({label, delay, x, y, color, side = 'left'}: {label: string; delay: number; x: number; y: number; color: string; side?: 'left' | 'right'}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, delay, SPRINGS.pop, 24);
  const line = fade(frame, delay + 6, delay + 20, Easing.out(Easing.quad));
  return (
    <div style={{position: 'absolute', left: x, top: y, display: 'flex', alignItems: 'center', gap: 8, flexDirection: side === 'left' ? 'row' : 'row-reverse'}}>
      <span style={{display: 'inline-block', padding: '10px 16px', border: `3px solid ${C.ink}`, borderRadius: 999, background: color, font: '700 21px Andika', color: C.ink, opacity: Math.min(1, a * 1.4), transform: `scale(${0.5 + 0.5 * a}) translateY(${wave(frame, 3, 0.06, delay)}px)`, boxShadow: `3px 3px 0 ${C.ink}`}}>{label}</span>
      <svg width="90" height="12" viewBox="0 0 90 12" style={{transform: side === 'left' ? undefined : 'scaleX(-1)'}}><path d="M2 6H86" stroke={C.ink} strokeWidth="4" strokeLinecap="round" strokeDasharray="84" strokeDashoffset={84 * (1 - line)} /><circle cx="86" cy="6" r="5" fill={C.ink} opacity={line} /></svg>
    </div>
  );
}

function RealRobotScene() {
  const frame = useCurrentFrame();
  const msg = frame < 90 ? 'hey there!' : frame < 170 ? 'working...' : frame < 240 ? 'hmm...' : 'hey there!';
  return (
    <SceneFrame accent={C.sun} duration={300}>
      <div style={{position: 'absolute', left: 108, top: 88}}><Kicker delay={4}>so, what is eve?</Kicker><H1 size={70} delay={10} text="A real robot<br/>on your desk." /></div>
      <div style={{position: 'absolute', left: 108, top: 468, width: 730}}>
        <Body size={28} delay={34}>A physical head, a local Mac app,<br />and a memory that stays useful.</Body>
        <div style={{marginTop: 30}}><Tag color={C.teal} delay={52}>works today</Tag></div>
      </div>
      <div style={{position: 'absolute', left: 1130, top: 150, width: 540, height: 640}}>
        <div style={{position: 'absolute', left: 90, top: 40, width: 360, height: 360, borderRadius: '50%', background: C.sun, opacity: 0.35, transform: `scale(${1 + wave(frame, 0.04, 0.05)})`}} />
        <div style={{position: 'absolute', left: 120, top: 20}}><Robot message={msg} scale={1.55} delay={14} listening={frame > 200 && frame < 240} /></div>
        <Callout label="camera" delay={70} x={-140} y={150} color={C.sun} />
        <Callout label="screen face" delay={84} x={-190} y={250} color={C.pinkSoft} />
        <Callout label="mic + speaker" delay={98} x={-210} y={350} color={C.teal} />
        <Callout label="2 motors" delay={112} x={410} y={440} color={C.sun} side="right" />
        <Callout label="12 lights" delay={126} x={400} y={190} color={C.teal} side="right" />
        <Callout label="touch" delay={140} x={430} y={90} color={C.pinkSoft} side="right" />
        <Kicker size={27} color={C.ink} delay={160} style={{position: 'absolute', left: 40, top: 560, marginBottom: 0, whiteSpace: 'nowrap'}}>a little machine, a lot of personality</Kicker>
        <span style={{position: 'absolute', left: 40, top: 600, font: '700 18px Andika', color: C.inkSoft, opacity: fade(frame, 180, 194)}}>M5StackChan · real hardware</span>
      </div>
    </SceneFrame>
  );
}

function Flow({items, delay, gap = 14}: {items: {label: string; color: string}[]; delay: number; gap?: number}) {
  return (
    <div style={{display: 'flex', alignItems: 'center', gap: 8, marginTop: 16}}>
      {items.map((n, i) => (
        <React.Fragment key={n.label}>
          <Node color={n.color} delay={delay + i * gap} litAt={delay + 40 + i * 12}>{n.label}</Node>
          {i < items.length - 1 && <Arrow width={78} delay={delay + 8 + i * gap} />}
        </React.Fragment>
      ))}
    </div>
  );
}

function EverydayScene() {
  const frame = useCurrentFrame();
  const message = frame < 110 ? 'listening…' : frame < 180 ? 'hmm…' : frame < 300 ? 'found it!' : 'on it…';
  const answerA = fade(frame, 196, 214, Easing.out(Easing.back(1.6)));
  return (
    <SceneFrame accent={C.sky} duration={390}>
      <div style={{position: 'absolute', left: 108, top: 76}}><Kicker delay={2}>everyday sidekick · works today</Kicker><H1 size={71} delay={8} text="You say it.<br/>eve routes it." /></div>
      <div style={{position: 'absolute', left: 108, top: 420, width: 700}}>
        <Kicker size={30} delay={40} style={{marginBottom: 22}}>“hey eve, where's my mug?”</Kicker>
        <div style={{display: 'flex', alignItems: 'center', gap: 12}}>
          <Sequence from={0} layout="none"><Robot message={message} scale={0.65} delay={30} listening={frame > 70 && frame < 180} /></Sequence>
          <Arrow width={82} delay={72} />
          <div style={{font: '700 29px/1.1 Grandstander', display: 'flex', gap: 10, alignItems: 'center'}}>
            {['look', 'find', 'answer'].map((w, i) => <React.Fragment key={w}><span style={{opacity: fade(frame, 84 + i * 30, 96 + i * 30), transform: `scale(${1 + bump(frame, 84 + i * 30, 18) * 0.25})`, display: 'inline-block'}}>{w}</span>{i < 2 && <span style={{opacity: fade(frame, 96 + i * 30, 104 + i * 30)}}>→</span>}</React.Fragment>)}
          </div>
        </div>
        <div style={{marginTop: 24, display: 'inline-block', padding: '12px 20px', background: C.teal, border: `3px solid ${C.ink}`, borderRadius: 999, font: '400 26px Gochi Hand', opacity: answerA, transform: `scale(${answerA})`, transformOrigin: '0 50%'}}>“On the shelf, left of the plant.”</div>
      </div>
      <Card color={C.teal} delay={20} from="right" style={{position: 'absolute', left: 840, top: 112, width: 910}} inner={{minHeight: 255, padding: 28}}>
        <div style={{font: '700 31px Grandstander'}}><Words text="Questions take the light path." delay={30} step={2} shadow={0} /></div>
        <Flow delay={40} items={[{label: 'voice', color: C.pinkSoft}, {label: 'gpt-live-1', color: C.sun}, {label: 'web search', color: C.teal}]} />
        <Kicker size={23} delay={90} style={{marginTop: 18, marginBottom: 0}}>Today's facts are checked, not guessed.</Kicker>
      </Card>
      <Card color={C.sun} delay={150} from="right" style={{position: 'absolute', left: 900, top: 480, width: 850}} inner={{minHeight: 285, padding: 28}}>
        <div style={{font: '700 31px Grandstander'}}><Words text="Mac work takes the guarded path." delay={160} step={2} shadow={0} /></div>
        <Flow delay={170} items={[{label: 'request', color: C.pinkSoft}, {label: 'gpt-6-astra', color: C.sun}, {label: 'start_task', color: C.teal}]} />
        <div style={{marginTop: 20, display: 'flex', gap: 10}}>
          <Pill color={C.teal} size={20} delay={240}>click + type</Pill>
          <Pill color={C.pinkSoft} size={20} delay={252}>ask before big actions</Pill>
          <Pill color={C.sheet} size={20} delay={264}>stop anytime</Pill>
        </div>
      </Card>
      <Kicker size={26} delay={300} style={{position: 'absolute', right: 106, bottom: 62, marginBottom: 0}}>not magic — a system with boundaries.</Kicker>
    </SceneFrame>
  );
}

export function NodeBox({title, detail, color, delay = 0, dashed = false}: {title: string; detail?: string; color: string; delay?: number; dashed?: boolean}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, delay, SPRINGS.pop, 22);
  const b = bump(frame, delay + 10, 16);
  return (
    <div style={{background: C.sheet, border: `3px ${dashed ? 'dashed' : 'solid'} ${C.ink}`, borderRadius: 10, boxShadow: `${7 * a}px ${7 * a}px 0 ${color}`, padding: '14px 18px', opacity: Math.min(1, a * 1.4), transform: `translateX(${(1 - a) * -40}px) scale(${1 + b * 0.05})`}}>
      <div style={{font: '700 24px/1 Grandstander', color: C.ink}}>{title}</div>
      {detail && <div style={{font: '400 17px/1.2 Andika', color: C.inkSoft, marginTop: 8}}>{detail}</div>}
    </div>
  );
}

function Column({title, sub, color, delay, children, dashed = false, style}: {title: string; sub: string; color?: string; delay: number; children: React.ReactNode; dashed?: boolean; style?: CSS}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, delay, SPRINGS.rise, 30);
  return (
    <div style={{position: 'absolute', background: dashed ? C.paper : C.sheet, border: `3px ${dashed ? 'dashed' : 'solid'} ${C.ink}`, borderRadius: 12, boxShadow: color ? `${10 * a}px ${10 * a}px 0 ${color}` : undefined, padding: 24, opacity: a, transform: `translateY(${(1 - a) * 60}px)`, ...style}}>
      <div style={{font: '700 28px Grandstander'}}>{title}</div><div style={{font: '400 18px Andika', color: C.inkSoft, marginTop: 6}}>{sub}</div>
      <div style={{marginTop: 22, display: 'grid', gap: 12}}>{children}</div>
    </div>
  );
}

// A dot that travels along a horizontal link, then repeats.
export function Packet({x, y, length, start, period = 70, color = C.pink}: {x: number; y: number; length: number; start: number; period?: number; color?: string}) {
  const frame = useCurrentFrame();
  if (frame < start) return null;
  const t = ((frame - start) % period) / period;
  const px = interpolate(t, [0, 1], [0, length], {easing: Easing.inOut(Easing.quad)});
  const op = Math.sin(t * Math.PI);
  return <div style={{position: 'absolute', left: x + px - 9, top: y - 9, width: 18, height: 18, borderRadius: '50%', background: color, border: `3px solid ${C.ink}`, opacity: op}} />;
}

function UnderHoodScene() {
  const frame = useCurrentFrame();
  return (
    <SceneFrame accent={C.teal} duration={330}>
      <div style={{position: 'absolute', left: 108, top: 56}}><Kicker delay={0}>show & tell · under the hood</Kicker><H1 size={64} delay={6} text="The physical eve<br/>is one system." /></div>
      <Column title="the robot" sub="firmware on the ESP32-S3" color={C.sun} delay={20} style={{left: 108, top: 280, width: 490, height: 620}}>
        <NodeBox title="screen face" detail="captions + eyes" color={C.sky} delay={40} />
        <NodeBox title="head + lights" detail="motors, moods, chirps" color={C.pinkSoft} delay={48} />
        <NodeBox title="camera + touch" detail="frames, pats, gestures" color={C.teal} delay={56} />
        <NodeBox title="local state" detail="the body reacts here" color={C.sun} delay={64} />
      </Column>
      <div style={{position: 'absolute', left: 650, top: 504, width: 250, textAlign: 'center'}}>
        <Arrow width={245} delay={80} /><div style={{font: '700 18px Andika', color: C.inkSoft, opacity: fade(frame, 90, 104)}}>events · state · captions</div>
      </div>
      <Packet x={655} y={521} length={235} start={110} period={80} color={C.pink} />
      <Column title="the Mac" sub="eve app · local control" color={C.teal} delay={70} style={{left: 930, top: 280, width: 430, height: 620}}>
        <NodeBox title="wake word" detail="heard locally: hey eve" color={C.pinkSoft} delay={96} />
        <NodeBox title="voice + tools" detail="routes questions and tasks" color={C.sun} delay={104} />
        <NodeBox title="computer worker" detail="PyAutoGUI + approvals" color={C.teal} delay={112} />
        <NodeBox title="memory" detail="chat notes + lesson store" color={C.sky} delay={120} />
      </Column>
      <div style={{position: 'absolute', left: 1372, top: 560, width: 80}}><Arrow width={80} delay={136} /></div>
      <Packet x={1376} y={577} length={70} start={150} period={60} color={C.sun} />
      <Column title="AI services" sub="called for the job" delay={130} dashed style={{left: 1460, top: 280, width: 350, height: 620}}>
        <NodeBox title="gpt-live-1" detail="voice receptionist" color={C.pinkSoft} delay={150} />
        <NodeBox title="gpt-6-astra" detail="tools + computer use" color={C.sun} delay={158} />
        <NodeBox title="gpt-5.4-nano" detail="camera understanding" color={C.teal} delay={166} />
        <NodeBox title="Exa" detail="optional practice references" color={C.sky} delay={174} dashed />
        <Kicker size={18} delay={200} style={{position: 'absolute', left: 24, right: 24, bottom: 22, marginBottom: 0}}>Exa receives topic + level;<br />the tutor creates the adapted problem.</Kicker>
      </Column>
      <Kicker size={25} delay={230} style={{position: 'absolute', left: 644, top: 892, marginBottom: 0}}>A Mac app connects the body to the right model at the right moment.</Kicker>
    </SceneFrame>
  );
}

function TeacherScene() {
  const frame = useCurrentFrame();
  return (
    <SceneFrame accent={C.pink} duration={360}>
      <div style={{position: 'absolute', left: 108, top: 88}}><Kicker delay={0}>the other half · designed for learners</Kicker><H1 size={77} delay={8} text="Not a homework<br/>answer machine." /></div>
      <div style={{position: 'absolute', left: 110, top: 470, width: 700}}>
        <Body size={30} delay={40}>More like a patient teacher who knows<br />when to be small.</Body>
        <Kicker size={31} delay={70} style={{marginTop: 28}}>the goal is the next thought.</Kicker>
      </div>
      <Card color={C.sun} delay={24} from="right" style={{position: 'absolute', left: 940, top: 122, width: 740}} inner={{minHeight: 250, padding: 30}}>
        <Kicker size={28} delay={38}>the kid</Kicker><div style={{font: '700 34px/1.08 Grandstander'}}><Words text="“I got stuck right here.”" delay={44} step={3} shadow={0} /></div><Body size={24} delay={70} style={{marginTop: 20}}>They want help without losing the feeling of solving it.</Body>
      </Card>
      <Card color={C.teal} delay={90} from="right" style={{position: 'absolute', left: 1050, top: 460, width: 700}} inner={{minHeight: 250, padding: 30}}>
        <Kicker size={28} delay={104}>the grown-up</Kicker><div style={{font: '700 34px/1.08 Grandstander'}}><Words text="“Please keep them thinking.”" delay={110} step={3} shadow={0} /></div><Body size={24} delay={136} style={{marginTop: 20}}>A helper that explains the next move, not the whole game.</Body>
      </Card>
      <div style={{position: 'absolute', left: 120, bottom: 82, display: 'flex', alignItems: 'center', gap: 18}}>
        {['nudge', 'check', 'one step'].map((x, i) => (
          <React.Fragment key={x}>
            <span style={{display: 'inline-block', padding: '16px 25px', background: [C.sun, C.teal, C.pink][i], border: `3px solid ${C.ink}`, borderRadius: 999, font: '700 24px Grandstander', opacity: fade(frame, 160 + i * 16, 172 + i * 16), transform: `scale(${1 + bump(frame, 160 + i * 16, 20) * 0.18}) translateY(${(1 - fade(frame, 160 + i * 16, 176 + i * 16)) * 20}px)`, boxShadow: `4px 4px 0 ${C.ink}`}}>{x}</span>
            {i < 2 && <Arrow width={58} delay={170 + i * 16} />}
          </React.Fragment>
        ))}
      </div>
    </SceneFrame>
  );
}

// Handwriting that writes itself: a clip reveal plus a pen tip riding the edge.
export function Handwriting({text, at, length = 30, width, style}: {text: string; at: number; length?: number; width: number; style?: CSS}) {
  const frame = useCurrentFrame();
  const p = fade(frame, at, at + length, Easing.inOut(Easing.quad));
  const writing = frame >= at && frame <= at + length;
  return (
    <div style={{position: 'relative', display: 'inline-block', ...style}}>
      <div style={{clipPath: `inset(-20% ${(1 - p) * 100}% -20% 0)`}}>{text}</div>
      {writing && <div style={{position: 'absolute', left: width * p - 6, top: '55%', width: 12, height: 12, borderRadius: '50%', background: C.ink, transform: `translateY(${Math.sin(frame * 0.9) * 4}px)`}} />}
    </div>
  );
}

export function Cursor({path, style}: {path: {at: number; x: number; y: number}[]; style?: CSS}) {
  const frame = useCurrentFrame();
  const first = path[0];
  if (frame < first.at) return null;
  const xs = path.map((p) => p.at);
  const x = interpolate(frame, xs, path.map((p) => p.x), {extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.inOut(Easing.cubic)});
  const y = interpolate(frame, xs, path.map((p) => p.y), {extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.inOut(Easing.cubic)});
  return (
    <svg width="34" height="40" viewBox="0 0 34 40" style={{position: 'absolute', left: x, top: y, zIndex: 5, filter: `drop-shadow(3px 3px 0 ${C.pinkSoft})`, ...style}}>
      <path d="M3 2l26 18-11 2 7 13-5 3-7-13-8 9z" fill={C.sheet} stroke={C.ink} strokeWidth="2.5" strokeLinejoin="round" />
    </svg>
  );
}

function LessonButton({label, color, pressAt}: {label: string; color: string; pressAt: number}) {
  const frame = useCurrentFrame();
  const down = frame >= pressAt && frame < pressAt + 8 ? 1 : 0;
  const glow = bump(frame, pressAt + 6, 24);
  return (
    <div style={{flex: 1, padding: '17px 10px', textAlign: 'center', background: color, border: `3px solid ${C.ink}`, borderRadius: 9, boxShadow: `${5 - 4 * down}px ${5 - 4 * down}px 0 ${C.ink}`, transform: `translate(${4 * down}px, ${4 * down}px) scale(${1 + glow * 0.04})`, font: '700 19px Andika'}}>{label}</div>
  );
}

function BoardNote({children, at, color, style}: {children: React.ReactNode; at: number; color: string; style?: CSS}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, at, SPRINGS.pop, 24);
  return <div style={{position: 'absolute', padding: '12px 18px', border: `3px solid ${C.ink}`, borderRadius: 8, background: color, font: '700 18px Andika', opacity: Math.min(1, a * 1.4), transform: `scale(${0.6 + 0.4 * a}) translateY(${(1 - a) * 20}px)`, ...style}}>{children}</div>;
}

function LessonBoard() {
  const frame = useCurrentFrame();
  const buttonA = fade(frame, 24, 40);
  const tagA = bump(frame, 6, 20);
  return (
    <BrowserWindow style={{width: 1000, height: 560}}>
      <div style={{padding: 24, background: C.paper, height: '100%', position: 'relative'}}>
        <div style={{display: 'flex', justifyContent: 'space-between', alignItems: 'center'}}>
          <div style={{font: '700 30px Grandstander'}}><Words text="Solve 2x + 3 = 11." delay={4} step={3} shadow={0} /></div>
          <span style={{padding: '8px 14px', border: `2px solid ${C.ink}`, borderRadius: 999, background: C.sun, font: '700 16px Andika', transform: `scale(${1 + tagA * 0.1})`}}>algebra · level 7</span>
        </div>
        <div style={{marginTop: 20, height: 265, background: C.sheet, border: `3px dashed ${C.ink}`, borderRadius: 8, padding: 26, position: 'relative', overflow: 'hidden'}}>
          <Handwriting text="2x + 3 = 11" at={40} length={34} width={210} style={{font: '400 34px Gochi Hand', color: C.ink}} />
          <Handwriting text="2x = 8" at={96} length={26} width={125} style={{position: 'absolute', left: 238, top: 116, font: '700 42px Grandstander', color: C.ink, transform: 'rotate(-1deg)'}} />
          <div style={{position: 'absolute', left: 610, top: 58, font: '400 24px Gochi Hand', color: C.inkSoft, opacity: fade(frame, 60, 74)}}>learner's board</div>
          <BoardNote at={172} color={C.sun} style={{left: 420, top: 40}}>“What could you do to both sides?”</BoardNote>
          <BoardNote at={262} color={C.teal} style={{left: 470, top: 128}}>✓ line two looks right</BoardNote>
          <BoardNote at={372} color={C.pinkSoft} style={{right: 34, bottom: 24}}>eve's separate panel → x = 4</BoardNote>
        </div>
        <div style={{display: 'flex', gap: 16, marginTop: 22, opacity: buttonA, transform: `translateY(${(1 - buttonA) * 20}px)`}}>
          <LessonButton label="Give me a hint" color={C.sun} pressAt={160} />
          <LessonButton label="Check my work" color={C.teal} pressAt={250} />
          <LessonButton label="Show one step" color={C.pink} pressAt={360} />
        </div>
        <Cursor path={[{at: 120, x: 700, y: 300}, {at: 156, x: 160, y: 472}, {at: 210, x: 190, y: 480}, {at: 246, x: 470, y: 472}, {at: 320, x: 500, y: 482}, {at: 356, x: 790, y: 472}, {at: 420, x: 820, y: 500}]} />
      </div>
    </BrowserWindow>
  );
}

function LessonScene() {
  const {frame, fps} = useT();
  const board = sp(frame, fps, 20, SPRINGS.rise, 34);
  const robotMsg = frame < 150 ? 'hmm…' : frame < 240 ? 'try this' : frame < 350 ? 'nice!' : frame < 440 ? 'one step' : 'your turn';
  return (
    <SceneFrame accent={C.sun} duration={720}>
      <div style={{position: 'absolute', left: 108, top: 52}}><Kicker delay={0}>show & tell · a math lesson</Kicker><H1 size={64} delay={6} text="A teacher-like loop,<br/>with a real whiteboard." /></div>
      <div style={{position: 'absolute', left: 116, top: 290, width: 480}}>
        <Kicker size={31} delay={20}>the learner says:</Kicker>
        <Card color={C.ink} delay={30} from="left" inner={{padding: '20px 24px', background: C.teal, borderRadius: 18, font: '700 30px/1.1 Grandstander'}}>“Okay eve, I want<br />to do a math lesson!”</Card>
        <div style={{marginTop: 30, display: 'grid', gap: 11}}>
          {['pick a topic', 'bring a problem', 'write your ideas'].map((x, i) => {
            const a = sp(frame, fps, 60 + i * 12, SPRINGS.pop, 22);
            return (
              <div key={x} style={{display: 'flex', gap: 12, alignItems: 'center', opacity: Math.min(1, a * 1.4), transform: `translateX(${(1 - a) * -30}px)`}}>
                <span style={{width: 28, height: 28, borderRadius: '50%', background: [C.sun, C.pinkSoft, C.sky][i], border: `3px solid ${C.ink}`, display: 'inline-flex', justifyContent: 'center', alignItems: 'center', font: '700 17px Andika', transform: `scale(${0.5 + 0.5 * a})`}}>{i + 1}</span>
                <span style={{font: '700 23px Andika'}}>{x}</span>
              </div>
            );
          })}
        </div>
        <div style={{marginTop: 36}}><Sequence from={0} layout="none"><Robot message={robotMsg} scale={0.62} delay={120} listening={frame > 130 && frame < 150} /></Sequence></div>
      </div>
      <div style={{position: 'absolute', left: 670, top: 210, opacity: board, transform: `translateY(${(1 - board) * 80 + wave(frame, 3, 0.04)}px) rotate(${(1 - board) * 2}deg)`}}><LessonBoard /></div>
      <div style={{position: 'absolute', left: 724, top: 814, display: 'flex', gap: 18, alignItems: 'center'}}>
        <Kicker size={28} delay={470} style={{marginBottom: 0}}>topic + level</Kicker><Arrow width={100} delay={486} />
        <Node color={C.sheet} delay={496} litAt={540} style={{border: `3px dashed ${C.ink}`, borderRadius: 9, font: '700 21px Andika', padding: '12px 18px'}}>Exa · references</Node>
        <Arrow width={100} delay={512} />
        <Node color={C.sun} delay={522} litAt={566} style={{borderRadius: 9, font: '700 21px Andika', padding: '12px 18px'}}>Astra · adapted problem</Node>
      </div>
      <Kicker size={23} delay={580} style={{position: 'absolute', right: 100, bottom: 48, marginBottom: 0}}>optional practice search · learner work stays out of Exa</Kicker>
    </SceneFrame>
  );
}

function FlipCard({title, quote, body, color, delay}: {title: string; quote: string; body: string; color: string; delay: number}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, delay, SPRINGS.rise, 32);
  return (
    <div style={{width: 500, perspective: 1200}}>
      <div style={{minHeight: 270, padding: 25, background: C.sheet, border: `3px solid ${C.ink}`, borderRadius: 8, boxShadow: `${10 * a}px ${10 * a}px 0 ${color}`, opacity: a, transform: `rotateY(${(1 - a) * 70}deg) translateY(${(1 - a) * 30 + wave(frame, 4, 0.05, delay)}px)`, transformOrigin: '20% 50%'}}>
        <div style={{font: '700 30px Grandstander'}}>{title}</div>
        <Kicker size={26} delay={delay + 16} style={{marginTop: 18, marginBottom: 0, color: C.ink}}>{quote}</Kicker>
        <Body size={21} delay={delay + 34} style={{marginTop: 18}}>{body}</Body>
      </div>
    </div>
  );
}

function GuardrailsScene() {
  const frame = useCurrentFrame();
  const mark = fade(frame, 60, 84, Easing.inOut(Easing.quad));
  const ruleA = fade(frame, 190, 214);
  return (
    <SceneFrame accent={C.teal} duration={360}>
      <div style={{position: 'absolute', left: 108, top: 78}}>
        <Kicker delay={0}>the teaching rule</Kicker>
        <div style={{font: '900 81px/0.98 Grandstander', letterSpacing: '-0.035em', color: C.ink}}>
          <Words text="The whole answer is" delay={6} />
          <span style={{display: 'inline-block', backgroundImage: `linear-gradient(transparent 56%, ${C.pinkSoft} 56%)`, backgroundRepeat: 'no-repeat', backgroundSize: `${mark * 100}% 100%`}}>
            <Words text="never on the menu." delay={28} step={4} />
          </span>
        </div>
      </div>
      <div style={{position: 'absolute', left: 110, top: 430, display: 'flex', gap: 22}}>
        <FlipCard title="Give me a hint" quote="“What could you do to both sides?”" body="A nudge. It never performs a step." color={C.sun} delay={70} />
        <FlipCard title="Check my work" quote="“Let's look at line two.”" body="It checks what is on the board and explains the first slip." color={C.teal} delay={96} />
        <FlipCard title="Show one step" quote="“Here's the next line, and why.”" body="Exactly one transformation, in its own panel." color={C.pink} delay={122} />
      </div>
      <div style={{position: 'absolute', left: 110, bottom: 62, font: '400 29px Gochi Hand', color: C.inkSoft, opacity: ruleA, transform: `translateX(${(1 - ruleA) * -30}px)`}}>Your board stays yours. Eve adds the smallest useful move.</div>
    </SceneFrame>
  );
}

function TwoWorldsScene() {
  const {frame, fps} = useT();
  const bridge = sp(frame, fps, 70, SPRINGS.pop, 30);
  const link = fade(frame, 90, 120, Easing.inOut(Easing.quad));
  return (
    <SceneFrame accent={C.pink} duration={300}>
      <div style={{position: 'absolute', left: 108, top: 84}}><Kicker delay={0}>same eve · two kinds of help</Kicker><H1 size={82} delay={6} text="The everyday build<br/>and the teacherly bit." /></div>
      <svg width="1920" height="1080" viewBox="0 0 1920 1080" style={{position: 'absolute', inset: 0, pointerEvents: 'none'}}>
        <path d="M808 470 C 860 420, 900 420, 940 425" fill="none" stroke={C.ink} strokeWidth="4" strokeDasharray="160" strokeDashoffset={160 * (1 - link)} strokeLinecap="round" />
        <path d="M1112 470 C 1060 420, 1020 420, 980 425" fill="none" stroke={C.ink} strokeWidth="4" strokeDasharray="160" strokeDashoffset={160 * (1 - link)} strokeLinecap="round" />
      </svg>
      <Card color={C.sun} delay={30} from="left" style={{position: 'absolute', left: 108, top: 430, width: 700}} inner={{padding: 30, minHeight: 280}}>
        <Kicker size={28} delay={44}>at the desk · works today</Kicker><div style={{font: '700 32px Grandstander', marginTop: 12}}><Words text="Listen. Look. Act. Remember." delay={50} step={5} shadow={0} /></div>
        <div style={{display: 'flex', flexWrap: 'wrap', gap: 10, marginTop: 22}}>{['weather', 'find my mug', 'open Spotify', 'go explore'].map((x, i) => <Pill key={x} color={C.sun} size={18} delay={80 + i * 8}>{x}</Pill>)}</div>
      </Card>
      <Card color={C.teal} delay={48} from="right" style={{position: 'absolute', right: 108, top: 430, width: 700}} inner={{padding: 30, minHeight: 280}}>
        <Kicker size={28} delay={62}>in a lesson · designed</Kicker><div style={{font: '700 32px Grandstander', marginTop: 12}}><Words text="Nudge. Check. Show one step." delay={68} step={5} shadow={0} /></div>
        <div style={{display: 'flex', flexWrap: 'wrap', gap: 10, marginTop: 22}}>{['topic or problem', 'whiteboard', 'hint', 'recap'].map((x, i) => <Pill key={x} color={C.teal} size={18} delay={100 + i * 8}>{x}</Pill>)}</div>
      </Card>
      <div style={{position: 'absolute', left: 804, top: 383, width: 310, height: 80, background: C.pinkSoft, border: `3px solid ${C.ink}`, borderRadius: 999, display: 'flex', alignItems: 'center', justifyContent: 'center', font: '700 24px Grandstander', opacity: Math.min(1, bridge * 1.4), transform: `scale(${0.5 + 0.5 * bridge}) translateY(${wave(frame, 5, 0.06)}px)`, boxShadow: `5px 5px 0 ${C.ink}`}}>one physical eve</div>
      <Kicker size={26} delay={84} style={{position: 'absolute', left: 824, top: 350, marginBottom: 0}}>the bridge</Kicker>
    </SceneFrame>
  );
}

// Cut-paper confetti: small rects and circles falling with a spin.
export function Confetti({count = 46, start = 10}: {count?: number; start?: number}) {
  const frame = useCurrentFrame();
  const colors = [C.sun, C.teal, C.pink, C.sky, C.pinkSoft];
  return (
    <div style={{position: 'absolute', inset: 0, pointerEvents: 'none', overflow: 'hidden'}}>
      {Array.from({length: count}, (_, i) => {
        const t = frame - start - rnd(i) * 30;
        if (t < 0) return null;
        const x = rnd(i + 100) * 1920 + Math.sin(t * 0.05 + i) * 40;
        const y = -60 + t * (5 + rnd(i + 200) * 5);
        if (y > 1140) return null;
        const size = 14 + rnd(i + 300) * 18;
        const round = rnd(i + 400) > 0.6;
        return <div key={i} style={{position: 'absolute', left: x, top: y, width: size, height: round ? size : size * 0.6, borderRadius: round ? '50%' : 3, background: colors[i % colors.length], border: `2px solid ${C.ink}`, transform: `rotate(${t * (2 + rnd(i + 500) * 6)}deg)`, opacity: 0.9}} />;
      })}
    </div>
  );
}

function EndScene() {
  const {frame, fps} = useT();
  const b = fade(frame, 60, 84);
  return (
    <SceneFrame accent={C.sky} duration={180}>
      <Confetti />
      <div style={{position: 'absolute', left: 180, right: 150, top: 210, height: 590, display: 'flex', alignItems: 'center', justifyContent: 'space-between'}}>
        <div style={{width: 900}}>
          <div style={{font: '900 180px/0.9 Grandstander', letterSpacing: '-0.06em', color: C.ink, display: 'flex'}}>
            {'eve'.split('').map((ch, i) => {
              const a = sp(frame, fps, 6 + i * 4, SPRINGS.pop, 30);
              return <span key={i} style={{display: 'inline-block', opacity: Math.min(1, a * 1.4), transform: `translateY(${(1 - a) * 120}px) rotate(${(1 - a) * -12}deg) scale(${0.6 + 0.4 * a})`, textShadow: `${9 * a}px ${9 * a}px 0 ${C.pinkSoft}`}}>{ch}</span>;
            })}
          </div>
          <Kicker size={45} color={C.ink} delay={34} style={{marginTop: 18, marginBottom: 0}}>your desktop sidekick</Kicker>
          <div style={{marginTop: 54, display: 'flex', gap: 14}}><Tag color={C.sun} delay={52}>voice</Tag><Tag color={C.teal} delay={60}>desk</Tag><Tag color={C.pinkSoft} delay={68}>learning</Tag></div>
        </div>
        <div style={{width: 420, display: 'flex', justifyContent: 'center'}}><Robot message="see you!" scale={1.32} delay={20} /></div>
      </div>
      <div style={{position: 'absolute', left: 220, bottom: 98, font: '700 24px Andika', color: C.inkSoft, opacity: b, transform: `translateY(${(1 - b) * 14}px)`}}>open source · github.com/gurul/eveTinkerer</div>
      <Kicker size={30} delay={70} style={{position: 'absolute', right: 180, bottom: 92, marginBottom: 0}}>keep thinking.</Kicker>
    </SceneFrame>
  );
}

function MontageScene() {
  const frame = useCurrentFrame();
  const feats = [
    ['uses your computer', C.sun], ['web search', C.teal], ['finds your mug', C.pinkSoft], ['has moods', C.sky],
    ['keeps a diary', C.sun], ['dreams at night', C.teal], ['takes photos', C.pinkSoft], ['explores the room', C.sky],
    ['dances', C.sun], ['loves a head pat', C.teal], ['desktop widget', C.pinkSoft], ['meeting notes', C.sky],
    ['remembers you', C.sun], ['mirrors Claude Code', C.teal], ['thinks hard', C.pinkSoft], ['asks before big actions', C.sky],
  ];
  const msg = frame < 60 ? 'on it…' : frame < 120 ? 'curious!' : frame < 180 ? 'click!' : 'zzz';
  return (
    <SceneFrame accent={C.teal} duration={240}>
      <div style={{position: 'absolute', left: 108, top: 80}}><Kicker delay={0}>and it also…</Kicker><H1 size={78} delay={6} text="A whole sidekick,<br/>not one trick." /></div>
      <div style={{position: 'absolute', left: 108, top: 420, width: 1150, display: 'flex', flexWrap: 'wrap', gap: 14}}>
        {feats.map(([x, c], i) => <Pill key={x} color={c} size={26} delay={30 + i * 7} lit>{x}</Pill>)}
      </div>
      <div style={{position: 'absolute', left: 1400, top: 300}}><Robot message={msg} scale={1.4} delay={16} listening={frame > 40 && frame < 110} /></div>
      <Kicker size={28} delay={150} style={{position: 'absolute', left: 110, bottom: 70, marginBottom: 0}}>all on the desk, all works today.</Kicker>
    </SceneFrame>
  );
}

// ---------- assembly ----------

const SCENES: {from: number; duration: number; accent: string; el: React.ReactNode}[] = [
  {from: 0, duration: 360, accent: C.pink, el: <ProblemScene />},
  {from: 360, duration: 300, accent: C.sun, el: <RealRobotScene />},
  {from: 660, duration: 390, accent: C.sky, el: <EverydayScene />},
  {from: 1050, duration: 240, accent: C.teal, el: <MontageScene />},
  {from: 1290, duration: 330, accent: C.sun, el: <UnderHoodScene />},
  {from: 1620, duration: 360, accent: C.pink, el: <TeacherScene />},
  {from: 1980, duration: 720, accent: C.sun, el: <LessonScene />},
  {from: 2700, duration: 360, accent: C.teal, el: <GuardrailsScene />},
  {from: 3060, duration: 300, accent: C.pink, el: <TwoWorldsScene />},
  {from: 3360, duration: 180, accent: C.sky, el: <EndScene />},
];

export function LaunchVideo() {
  const frame = useCurrentFrame();
  const volume = interpolate(frame, [0, 36, TOTAL_FRAMES - 90, TOTAL_FRAMES], [0, 0.72, 0.72, 0], {extrapolateLeft: 'clamp', extrapolateRight: 'clamp'});
  return (
    <AbsoluteFill style={{background: C.paper}}>
      <style dangerouslySetInnerHTML={{__html: fontFace}} />
      <Audio src={staticFile('audio/eve-launch.mp3')} volume={volume} />
      {SCENES.map((s) => (
        <Sequence key={s.from} from={s.from} durationInFrames={s.duration}><AbsoluteFill>{s.el}</AbsoluteFill></Sequence>
      ))}
      {SCENES.slice(1).map((s) => (
        <Sequence key={`wipe-${s.from}`} from={s.from - 16} durationInFrames={32}><Wipe color={s.accent} /></Sequence>
      ))}
    </AbsoluteFill>
  );
}

export const RemotionRoot: React.FC = () => {
  return (
    <>
      <Composition id="LaunchVideo" component={LaunchVideo} durationInFrames={TOTAL_FRAMES} fps={FPS} width={1920} height={1080} />
    </>
  );
};
