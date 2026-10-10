import React from 'react';
import {AbsoluteFill, Audio, Easing, Img, Sequence, interpolate, staticFile, useCurrentFrame} from 'remotion';
import {
  Arrow,
  BrowserWindow,
  C,
  CSS,
  Card,
  Chirp,
  Confetti,
  Cursor,
  H1,
  Handwriting,
  Kicker,
  Body,
  Pill,
  SPRINGS,
  SceneFrame,
  Speech,
  Tag,
  Wipe,
  Words,
  bump,
  fade,
  fontFace,
  sp,
  useT,
  wave,
} from './Root';

/*
 * eve — the holistic launch film. 92 s, 1920×1080, 30 fps.
 *
 * The Sep 13 cut (LaunchVideo in Root.tsx) is built around lessons. This one is
 * the whole eve: the body and its senses, its life of its own, the phone door,
 * your own Chrome, how it picks a path, coding sessions, the second brain, and
 * everything else in one orbit. Every claim maps to the repo:
 *   README "What eve does"; DESIGN.md (gaze: the eyes lead the head);
 *   docs/stackchan/telegram.md (text door, rundown, new claude, claude on);
 *   docs/stackchan/routing.md "Controlling your logged-in Chrome";
 *   docs/codex-computer-use/README.md (yes / allow for task / always allow);
 *   docs/stackchan/second-brain.md.
 * The Telegram door, Chrome attach and the second brain are off by default, so
 * those scenes carry an "opt-in" tag.
 *
 * Motion system: Root.tsx's (springs, kinetic type, paper wipes, camera push),
 * plus an expressive robot (moods, gaze, head turn, squash, LEDs, assembly and
 * power-on), objects that fly between devices on bezier paths, bursts on
 * contact, and a camera push through the robot's screen into the phone.
 */

export const POCKET_FPS = 30;

const MONO = "Menlo, 'SF Mono', monospace";
const SKY_DAY = '#BFD3F2';

type XY = [number, number];

// ---------- math ----------

function lerp(a: number, b: number, t: number) {
  return a + (b - a) * t;
}

function mixColor(a: string, b: string, t: number) {
  const pa = [1, 3, 5].map((i) => parseInt(a.slice(i, i + 2), 16));
  const pb = [1, 3, 5].map((i) => parseInt(b.slice(i, i + 2), 16));
  return `rgb(${pa.map((v, i) => Math.round(lerp(v, pb[i], t))).join(',')})`;
}

function quad(p0: XY, c: XY, p1: XY, t: number): XY {
  const u = 1 - t;
  return [u * u * p0[0] + 2 * u * t * c[0] + t * t * p1[0], u * u * p0[1] + 2 * u * t * c[1] + t * t * p1[1]];
}

function cubic(p0: XY, c1: XY, c2: XY, p1: XY, t: number): XY {
  const u = 1 - t;
  const f = (i: 0 | 1) => u * u * u * p0[i] + 3 * u * u * t * c1[i] + 3 * u * t * t * c2[i] + t * t * t * p1[i];
  return [f(0), f(1)];
}

function prog(frame: number, t0: number, t1: number, easing = Easing.inOut(Easing.cubic)) {
  return interpolate(frame, [t0, t1], [0, 1], {extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing});
}

// ---------- the expressive robot ----------

type Mood = 'open' | 'wide' | 'happy' | 'wink' | 'closed' | 'heart';

const EYE = 'M-8 -9.5H8V-4.5H12V9.5H7V4.5H-7V9.5H-12V-4.5H-8Z';
const HEART = 'M0 10L-11 0C-15 -4 -13 -10 -7 -10C-3 -10 -1 -7 0 -5C1 -7 3 -10 7 -10C13 -10 15 -4 11 0Z';

function Eye({cx, cy, mood, blink, side}: {cx: number; cy: number; mood: Mood; blink: number; side: 'L' | 'R'}) {
  if (mood === 'closed') return <rect x={cx - 12} y={cy + 1} width={24} height={4} />;
  if (mood === 'happy') return <path d={`M${cx - 12} ${cy + 5}L${cx} ${cy - 6}L${cx + 12} ${cy + 5}L${cx + 12} ${cy + 10}L${cx} ${cy - 1}L${cx - 12} ${cy + 10}Z`} />;
  if (mood === 'wink' && side === 'R') return <path d={`M${cx - 12} ${cy + 3}Q${cx} ${cy - 9} ${cx + 12} ${cy + 3}`} fill="none" stroke={C.screenPink} strokeWidth={5} strokeLinecap="round" />;
  if (mood === 'heart') return <path d={HEART} transform={`translate(${cx} ${cy}) scale(1.15)`} />;
  const s = mood === 'wide' ? 1.2 : 1;
  return <path d={EYE} transform={`translate(${cx} ${cy}) scale(${s} ${s * blink})`} />;
}

type BotProps = {
  scale?: number;
  delay?: number;
  text?: string;
  typeFrom?: number;
  mood?: Mood;
  look?: XY;
  yaw?: number;
  pitch?: number;
  listening?: boolean;
  leds?: string | null;
  assemble?: boolean;
  bob?: boolean;
  squash?: number;
  chirpAt?: number;
  flash?: number;
  dim?: number;
};

function Bot({scale = 1, delay = 0, text = '', typeFrom, mood = 'open', look = [0, 0], yaw = 0, pitch = 0, listening = false, leds = null, assemble = false, bob = true, squash = 0, chirpAt, flash = 0, dim = 0}: BotProps) {
  const {frame, fps} = useT();
  const d = delay;
  const part = (k: number) => (assemble ? sp(frame, fps, d + k, SPRINGS.pop, 28) : 1);
  const base = part(0);
  const neck = part(7);
  const head = part(14);
  const label = part(24);
  const power = assemble ? prog(frame, d + 32, d + 46, Easing.inOut(Easing.quad)) : 1;
  const enter = assemble ? (frame >= d ? 1 : 0) : sp(frame, fps, d, SPRINGS.pop, 30);
  const bobY = bob ? wave(frame, 5, 0.06) : 0;
  const tilt = wave(frame, 2.2, 0.035, 1.3);
  const bt = (frame + d * 7) % 108;
  const blink = bt > 96 && bt < 102 ? 0.15 : 1;
  const safe = text.length > 15 ? `${text.slice(0, 14)}…` : text;
  const shown = typeFrom === undefined ? safe.length : Math.max(0, Math.floor((frame - typeFrom) / 2));
  const typed = safe.slice(0, shown);
  const cursor = shown < safe.length && frame % 10 < 6 ? '▌' : '';
  const ex = look[0] * 13 + yaw * 0.22;
  const ey = look[1] * 8;
  const sx = 1 - Math.min(0.22, Math.abs(yaw) / 170);
  const ring = listening ? (frame % 40) / 40 : 0;
  const litColor = leds ?? '#4D5160';
  const headT = `translate(${yaw * 0.9} ${pitch + (1 - head) * -300}) translate(100 170) rotate(${tilt}) scale(${sx * (1 + squash * 0.07)} ${1 - squash * 0.13}) translate(-100 -170)`;
  const lineW = 116 * Math.min(1, power * 2);
  const screenH = power < 0.5 ? 3 : 100 * ((power - 0.5) * 2);
  return (
    <div style={{width: 250 * scale, height: 300 * scale, position: 'relative', opacity: Math.min(1, enter * 1.5), transform: `translateY(${(assemble ? 0 : (1 - enter) * 90) + bobY}px) scale(${assemble ? 1 : 0.6 + 0.4 * enter})`, transformOrigin: '50% 100%'}}>
      {chirpAt !== undefined && <Chirp at={chirpAt} x={125 * scale} y={120 * scale} color={C.screenPink} />}
      {listening && (
        <div style={{position: 'absolute', left: 125 * scale - 150 * scale * (1 + ring), top: 120 * scale - 150 * scale * (1 + ring), width: 300 * scale * (1 + ring), height: 300 * scale * (1 + ring), borderRadius: '50%', border: `${5 * scale}px solid ${C.sky}`, opacity: (1 - ring) * 0.6}} />
      )}
      <svg width={250 * scale} height={300 * scale} viewBox="0 0 200 240" style={{display: 'block', overflow: 'visible', position: 'relative'}}>
        <defs>
          <linearGradient id={`lbl-${d}-${scale}`} x1="0" x2="1" y1="0" y2="1"><stop offset="0" stopColor="#9DB3EE" /><stop offset="1" stopColor="#B98BD8" /></linearGradient>
        </defs>
        <g transform={`translate(0 ${(1 - neck) * -320})`}>
          <rect x="68" y="168" width="64" height="30" fill="#8C90A0" stroke="#4D5160" strokeWidth="2.5" />
        </g>
        <g transform={`translate(0 ${(1 - base) * -360})`}>
          <rect x="46" y="194" width="108" height="38" rx="7" fill="#6F7384" stroke="#4D5160" strokeWidth="2.5" />
          {Array.from({length: 12}, (_, i) => {
            const on = leds ? 0.35 + 0.65 * ((Math.sin(frame * 0.32 - i * 0.62) + 1) / 2) : 1;
            return <circle key={i} cx={54 + i * 8.4} cy={206} r={2.9} fill={litColor} opacity={on} />;
          })}
          {leds && <rect x="46" y="194" width="108" height="38" rx="7" fill={leds} opacity={0.12 + Math.sin(frame * 0.2) * 0.06} />}
          <circle cx="64" cy="222" r="5" fill="#4D5160" /><circle cx="136" cy="222" r="5" fill="#4D5160" />
        </g>
        <g transform={headT}>
          <g transform={`translate(${(1 - label) * -240} 0)`}>
            <rect x="22" y="10" width="156" height="34" rx="7" fill={`url(#lbl-${d}-${scale})`} stroke="#5E5F98" strokeWidth="2.5" />
            <text x="100" y="35" textAnchor="middle" fontFamily="Grandstander" fontWeight="700" fontSize="22" fill="#F4F4FB">eve</text>
          </g>
          <rect x="12" y="42" width="176" height="14" rx="4" fill="#A487D0" stroke="#5E5F98" strokeWidth="2.5" />
          <rect x="30" y="52" width="140" height="122" rx="14" fill="#C3C6CF" stroke="#5C6070" strokeWidth="3" />
          <rect x="42" y="62" width="116" height="100" rx="6" fill="#0E1330" />
          {power > 0 && power < 1 && <rect x={100 - lineW / 2} y={112 - screenH / 2} width={lineW} height={screenH} rx={power < 0.5 ? 1 : 6} fill={power < 0.5 ? '#FFFFFF' : C.screen} />}
          {power >= 1 && (
            <>
              <rect x="42" y="62" width="116" height="100" rx="6" fill={C.screen} />
              {listening && <rect x="42" y="62" width="116" height="100" rx="6" fill={C.sky} opacity={0.12 + Math.sin(frame * 0.2) * 0.08} />}
              <g fill={C.screenPink} transform={`translate(${ex} ${ey})`}>
                <Eye cx={80} cy={84} mood={mood} blink={blink} side="L" />
                <Eye cx={120} cy={84} mood={mood} blink={blink} side="R" />
              </g>
              <text x={100 + ex * 0.4} y="134" textAnchor="middle" fontFamily="VT323" fontSize="22" fill={C.screenPink}>{typed}{cursor}</text>
              {dim > 0 && <rect x="42" y="62" width="116" height="100" rx="6" fill="#050817" opacity={dim} />}
              {flash > 0 && <rect x="42" y="62" width="116" height="100" rx="6" fill="#FFFFFF" opacity={flash} />}
            </>
          )}
        </g>
      </svg>
    </div>
  );
}

// ---------- motion primitives ----------

// Something that travels from `from` to `to` on a curve between t0 and t1, then vanishes
// (the thing it becomes is drawn by the receiver). A dotted trail draws behind it.
function Flyer({from, to, t0, t1, lift = -180, spin = 10, scaleTo = 0.7, trail = C.ink, children}: {from: XY; to: XY; t0: number; t1: number; lift?: number; spin?: number; scaleTo?: number; trail?: string | null; children: React.ReactNode}) {
  const frame = useCurrentFrame();
  if (frame < t0 || frame > t1 + 10) return null;
  const p = prog(frame, t0, t1);
  const c: XY = [(from[0] + to[0]) / 2, Math.min(from[1], to[1]) + lift];
  const [x, y] = quad(from, c, to, p);
  const arrived = frame > t1;
  const trailOp = arrived ? 1 - prog(frame, t1, t1 + 10) : 0.55;
  return (
    <>
      {trail && (
        <svg width="1920" height="1080" style={{position: 'absolute', left: 0, top: 0, pointerEvents: 'none', overflow: 'visible', zIndex: 20}}>
          <path d={`M${from[0]} ${from[1]}Q${c[0]} ${c[1]} ${x} ${y}`} fill="none" stroke={trail} strokeWidth="4" strokeLinecap="round" strokeDasharray="2 14" opacity={trailOp} />
        </svg>
      )}
      {!arrived && (
        <div style={{position: 'absolute', left: x, top: y, zIndex: 21, transform: `translate(-50%, -50%) rotate(${Math.sin(p * Math.PI) * spin}deg) scale(${lerp(1, scaleTo, p) * (1 + Math.sin(p * Math.PI) * 0.12)})`}}>
          {children}
        </div>
      )}
    </>
  );
}

// Cut-paper diamonds bursting out from a point.
function Burst({at, x, y, colors = [C.sun, C.teal, C.pink, C.sky], n = 12, r = 110}: {at: number; x: number; y: number; colors?: string[]; n?: number; r?: number}) {
  const frame = useCurrentFrame();
  const t = (frame - at) / 24;
  if (t < 0 || t > 1) return null;
  const e = Easing.out(Easing.cubic)(t);
  return (
    <div style={{position: 'absolute', left: x, top: y, width: 0, height: 0, zIndex: 25, pointerEvents: 'none'}}>
      {Array.from({length: n}, (_, i) => {
        const ang = (i / n) * Math.PI * 2 + i * 0.37;
        const dist = r * e * (0.7 + ((i * 7) % 5) * 0.08);
        const s = 12 + ((i * 5) % 3) * 5;
        return <div key={i} style={{position: 'absolute', left: Math.cos(ang) * dist - s / 2, top: Math.sin(ang) * dist - s / 2, width: s, height: s, background: colors[i % colors.length], border: `2px solid ${C.ink}`, transform: `rotate(${45 + t * 180}deg) scale(${1 - t * 0.6})`, opacity: 1 - t * t}} />;
      })}
    </div>
  );
}

// A check mark that draws itself inside a round box.
function Tick({at, color = C.teal, size = 30}: {at: number; color?: string; size?: number}) {
  const frame = useCurrentFrame();
  const p = prog(frame, at, at + 12, Easing.out(Easing.quad));
  const b = bump(frame, at + 8, 14);
  return (
    <svg width={size} height={size} viewBox="0 0 30 30" style={{flex: 'none', transform: `scale(${1 + b * 0.25})`}}>
      <circle cx="15" cy="15" r="12.5" fill={p > 0 ? color : C.sheet} stroke={C.ink} strokeWidth="3" />
      <path d="M8.5 15.5l4.5 4.5 8.5-9" fill="none" stroke={C.ink} strokeWidth="3.4" strokeLinecap="round" strokeLinejoin="round" strokeDasharray="20" strokeDashoffset={20 * (1 - p)} />
    </svg>
  );
}

// ---------- phone + chat ----------

type Msg = {at: number; me?: boolean; text: React.ReactNode; wide?: boolean};

function Bubble({m}: {m: Msg}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, m.at, SPRINGS.pop, 20);
  return (
    <div
      style={{
        alignSelf: m.me ? 'flex-end' : 'flex-start',
        maxWidth: m.wide ? '94%' : '84%',
        padding: '12px 16px',
        background: m.me ? C.teal : C.sheet,
        border: `3px solid ${C.ink}`,
        borderRadius: m.me ? '20px 20px 6px 20px' : '20px 20px 20px 6px',
        boxShadow: m.me ? undefined : `3px 3px 0 ${C.pinkSoft}`,
        font: '400 21px/1.25 Andika',
        color: C.ink,
        opacity: Math.min(1, a * 1.5),
        transform: `scale(${0.6 + 0.4 * a}) translateY(${(1 - a) * 16}px)`,
        transformOrigin: m.me ? '100% 100%' : '0 100%',
        whiteSpace: 'pre-line',
        flex: 'none',
      }}
    >
      {m.text}
    </div>
  );
}

function Typing() {
  const frame = useCurrentFrame();
  return (
    <div style={{alignSelf: 'flex-start', padding: '14px 18px', background: C.sheet, border: `3px solid ${C.ink}`, borderRadius: '20px 20px 20px 6px', display: 'flex', gap: 7, flex: 'none'}}>
      {[0, 1, 2].map((i) => (
        <span key={i} style={{width: 10, height: 10, borderRadius: '50%', background: C.inkSoft, transform: `translateY(${Math.sin(frame * 0.35 - i * 0.9) * 4}px)`}} />
      ))}
    </div>
  );
}

// Newest message at the bottom; older ones scroll off the top.
function Chat({msgs}: {msgs: Msg[]}) {
  const frame = useCurrentFrame();
  const shown = msgs.filter((m) => frame >= m.at);
  const next = msgs.find((m) => frame < m.at);
  const typing = next && !next.me && frame >= next.at - 22;
  return (
    <div style={{display: 'flex', flexDirection: 'column', justifyContent: 'flex-end', gap: 12, height: '100%', padding: 16, overflow: 'hidden'}}>
      {shown.map((m, i) => <Bubble key={i} m={m} />)}
      {typing && <Typing />}
    </div>
  );
}

function Avatar({size = 38}: {size?: number}) {
  return (
    <svg width={size} height={size} viewBox="0 0 40 40" style={{flex: 'none'}}>
      <rect x="1.5" y="1.5" width="37" height="37" rx="10" fill={C.screen} stroke={C.ink} strokeWidth="3" />
      <rect x="10" y="13" width="7" height="9" fill={C.screenPink} />
      <rect x="23" y="13" width="7" height="9" fill={C.screenPink} />
    </svg>
  );
}

const PHONE_W = 430;

function Phone({msgs, delay = 0, color = C.sky, height = 760, left, top, from}: {msgs: Msg[]; delay?: number; color?: string; height?: number; left: number; top: number; from?: {x: number; y: number; scale: number}}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, delay, from ? SPRINGS.soft : SPRINGS.rise, 34);
  const dx = from ? (from.x - (left + PHONE_W / 2)) * (1 - a) : 0;
  const dy = from ? (from.y - (top + height / 2)) * (1 - a) : (1 - a) * 80;
  const s = from ? lerp(from.scale, 1, a) : 1;
  return (
    <div style={{position: 'absolute', left, top, zIndex: 3}}>
      <div
        style={{
          width: PHONE_W,
          height,
          background: C.sheet,
          border: `3px solid ${C.ink}`,
          borderRadius: 48,
          boxShadow: `${12 * a}px ${12 * a}px 0 ${color}`,
          padding: 14,
          display: 'flex',
          flexDirection: 'column',
          opacity: from ? (frame >= delay ? 1 : 0) : a,
          transform: `translate(${dx}px, ${dy + wave(frame, 4, 0.05, delay)}px) scale(${s}) rotate(${(1 - a) * -3 + wave(frame, 0.3, 0.04)}deg)`,
        }}
      >
        <div style={{alignSelf: 'center', width: 110, height: 12, borderRadius: 999, background: C.ink, opacity: 0.85, marginTop: 4}} />
        <div style={{display: 'flex', alignItems: 'center', gap: 12, padding: '14px 12px 12px'}}>
          <Avatar />
          <div>
            <div style={{font: '700 24px/1 Grandstander', color: C.ink}}>eve</div>
            <div style={{font: '400 15px Andika', color: C.inkSoft, marginTop: 3}}>on Telegram · your Mac is home</div>
          </div>
        </div>
        <div style={{flex: 1, background: C.paper, border: `3px solid ${C.ink}`, borderRadius: 32, overflow: 'hidden'}}>
          <Chat msgs={msgs} />
        </div>
      </div>
    </div>
  );
}

// Where the newest bubble lands inside a phone, for things flying in or out of it.
function phoneMe(left: number, top: number, height: number): XY {
  return [left + PHONE_W - 110, top + height - 70];
}
function phoneEve(left: number, top: number, height: number): XY {
  return [left + 150, top + height - 90];
}

// ---------- cut-paper props ----------

function Person({x, y, turn = 0}: {x: number; y: number; turn?: number}) {
  const frame = useCurrentFrame();
  return (
    <svg width="170" height="190" viewBox="0 0 170 190" style={{position: 'absolute', left: x - 85, top: y - 60, overflow: 'visible', transform: `rotate(${wave(frame, 2, 0.05)}deg)`}}>
      <path d="M12 190C14 140 44 118 85 118S156 140 158 190Z" fill={C.sky} stroke={C.ink} strokeWidth="3" />
      <circle cx="85" cy="60" r="46" fill={C.pinkSoft} stroke={C.ink} strokeWidth="3" />
      <path d="M40 52C44 18 70 8 92 12C116 16 132 34 130 54C118 38 96 34 74 40C60 44 48 50 40 52Z" fill={C.ink} />
      <circle cx={70 + turn * 8} cy="66" r="4.5" fill={C.ink} />
      <circle cx={100 + turn * 8} cy="66" r="4.5" fill={C.ink} />
      <path d={`M${72 + turn * 8} 84Q${85 + turn * 8} 94 ${98 + turn * 8} 84`} fill="none" stroke={C.ink} strokeWidth="3.5" strokeLinecap="round" />
    </svg>
  );
}

function Hand({x, y}: {x: number; y: number}) {
  return (
    <svg width="150" height="200" viewBox="0 0 150 200" style={{position: 'absolute', left: x - 75, top: y - 190, overflow: 'visible', zIndex: 6}}>
      <rect x="40" y="-200" width="70" height="250" rx="8" fill={C.sky} stroke={C.ink} strokeWidth="3" />
      <rect x="22" y="40" width="106" height="92" rx="30" fill={C.pinkSoft} stroke={C.ink} strokeWidth="3" />
      {[0, 1, 2, 3].map((i) => <rect key={i} x={26 + i * 25} y={110} width={22} height={70 - Math.abs(i - 1.5) * 10} rx={11} fill={C.pinkSoft} stroke={C.ink} strokeWidth="3" />)}
    </svg>
  );
}

function FloatHearts({start, x, y}: {start: number; x: number; y: number}) {
  const frame = useCurrentFrame();
  return (
    <>
      {[0, 1, 2, 3].map((i) => {
        const t = (frame - start - i * 9) / 45;
        if (t < 0 || t > 1) return null;
        return (
          <svg key={i} width="44" height="40" viewBox="-16 -14 32 28" style={{position: 'absolute', left: x + (i - 1.5) * 50 + Math.sin(t * 6 + i) * 16, top: y - t * 180, opacity: 1 - t, transform: `scale(${0.6 + t * 0.6})`, zIndex: 7}}>
            <path d={HEART} fill={C.pink} stroke={C.ink} strokeWidth="2.5" />
          </svg>
        );
      })}
    </>
  );
}

function FloatZs({start, x, y}: {start: number; x: number; y: number}) {
  const frame = useCurrentFrame();
  if (frame < start) return null;
  return (
    <>
      {[0, 1, 2].map((i) => {
        const t = (((frame - start) / 50 + i / 3) % 1);
        return <div key={i} style={{position: 'absolute', left: x + t * 70 + Math.sin(t * 5) * 10, top: y - t * 140, font: `900 ${30 + i * 8}px Grandstander`, color: C.ink, opacity: Math.sin(t * Math.PI), textShadow: `3px 3px 0 ${C.pinkSoft}`, zIndex: 7}}>z</div>;
      })}
    </>
  );
}

// Viewfinder corners, the way a camera frames what it found.
function Brackets({x, y, w, h, label, color = C.teal, opacity = 1}: {x: number; y: number; w: number; h: number; label?: string; color?: string; opacity?: number}) {
  const L = 26;
  return (
    <div style={{position: 'absolute', left: x, top: y, width: w, height: h, opacity, zIndex: 5, pointerEvents: 'none'}}>
      <svg width={w} height={h} viewBox={`0 0 ${w} ${h}`} style={{overflow: 'visible', position: 'absolute'}}>
        {[[0, 0, 1, 1], [w, 0, -1, 1], [0, h, 1, -1], [w, h, -1, -1]].map(([cx, cy, sx, sy], i) => (
          <path key={i} d={`M${cx} ${cy + sy * L}V${cy}H${cx + sx * L}`} fill="none" stroke={color} strokeWidth="7" strokeLinecap="round" />
        ))}
        {[[0, 0, 1, 1], [w, 0, -1, 1], [0, h, 1, -1], [w, h, -1, -1]].map(([cx, cy, sx, sy], i) => (
          <path key={`i${i}`} d={`M${cx} ${cy + sy * L}V${cy}H${cx + sx * L}`} fill="none" stroke={C.ink} strokeWidth="2.5" strokeLinecap="round" />
        ))}
      </svg>
      {label && <span style={{position: 'absolute', left: 0, top: -44, padding: '6px 12px', background: color, border: `3px solid ${C.ink}`, borderRadius: 999, font: '700 18px Andika', color: C.ink, whiteSpace: 'nowrap'}}>{label}</span>}
    </div>
  );
}

// ---------- 1 · hello ----------

function HelloScene() {
  const {frame, fps} = useT();
  const mood: Mood = frame < 68 ? 'wide' : frame < 150 ? 'open' : 'happy';
  const underline = prog(frame, 138, 160, Easing.inOut(Easing.quad));
  return (
    <SceneFrame accent={C.pink} duration={180}>
      <div style={{position: 'absolute', left: 0, right: 0, top: 110, display: 'flex', flexDirection: 'column', alignItems: 'center'}}>
        <Bot assemble delay={8} scale={1.5} text="hey there!" typeFrom={66} mood={mood} chirpAt={56} leds={frame > 70 ? C.screenPink : null} look={frame > 100 && frame < 130 ? [0.6, -0.2] : [0, 0]} />
        <div style={{font: '900 150px/0.9 Grandstander', letterSpacing: '-0.06em', color: C.ink, display: 'flex', marginTop: 28, position: 'relative'}}>
          {'eve'.split('').map((ch, i) => {
            const at = 92 + i * 5;
            const a = sp(frame, fps, at, SPRINGS.pop, 30);
            return (
              <span key={i} style={{display: 'inline-block', position: 'relative', opacity: Math.min(1, a * 1.4), transform: `translateY(${(1 - a) * -140}px) rotate(${(1 - a) * (i % 2 ? 14 : -14)}deg) scaleY(${1 - bump(frame, at + 7, 8) * 0.18})`, transformOrigin: '50% 100%', textShadow: `${8 * a}px ${8 * a}px 0 ${C.pinkSoft}`}}>
                {ch}
              </span>
            );
          })}
          {[0, 1, 2, 3, 4].map((i) => <Burst key={i} at={99 + i * 5} x={48 + i * 78} y={128} n={6} r={60} />)}
        </div>
        <div style={{position: 'relative', marginTop: 18}}>
          <Kicker size={40} color={C.ink} delay={128} style={{marginBottom: 0}}>your desktop sidekick</Kicker>
          <svg width="380" height="24" viewBox="0 0 380 24" style={{position: 'absolute', left: 0, top: 46}}>
            <path d="M4 14C80 4 160 22 240 10S350 8 376 14" fill="none" stroke={C.pink} strokeWidth="6" strokeLinecap="round" strokeDasharray="400" strokeDashoffset={400 * (1 - underline)} />
          </svg>
        </div>
      </div>
    </SceneFrame>
  );
}

// ---------- 2 · senses ----------

const BOT2 = {left: 1150, top: 250, scale: 1.5};
const BOT2_EYES: XY = [BOT2.left + 125 * BOT2.scale, BOT2.top + 105 * BOT2.scale];

function personX(frame: number) {
  return interpolate(frame, [0, 96, 160, 330], [900, 900, 1760, 1760], {extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.inOut(Easing.cubic)});
}

function Shelf({frame}: {frame: number}) {
  const a = prog(frame, 168, 186, Easing.out(Easing.back(1.4)));
  return (
    <div style={{position: 'absolute', left: 860, top: 760, width: 940, height: 150, opacity: a, transform: `translateY(${(1 - a) * 40}px)`}}>
      <svg width="940" height="150" viewBox="0 0 940 150" style={{overflow: 'visible'}}>
        {/* mug */}
        <path d="M205 58h62v58c0 10-8 18-18 18h-26c-10 0-18-8-18-18z" fill={C.pink} stroke={C.ink} strokeWidth="3" />
        <path d="M267 72c20 0 20 32 0 32" fill="none" stroke={C.ink} strokeWidth="6" />
        {/* plant */}
        <path d="M380 86h70l-8 48h-54z" fill={C.sun} stroke={C.ink} strokeWidth="3" />
        <path d="M415 86C400 50 372 40 360 44C372 62 390 76 415 86Z M415 86C426 44 452 30 470 32C460 56 440 76 415 86Z M415 86C414 56 418 30 420 18C428 40 426 64 415 86Z" fill={C.teal} stroke={C.ink} strokeWidth="3" />
        {/* books */}
        {[0, 1, 2, 3].map((i) => <rect key={i} x={620 + i * 34} y={40 + (i % 2) * 14} width={30} height={94 - (i % 2) * 14} rx={3} fill={[C.sky, C.sun, C.pinkSoft, C.teal][i]} stroke={C.ink} strokeWidth="3" />)}
        <rect x="0" y="134" width="940" height="14" rx="4" fill={C.desk} stroke={C.ink} strokeWidth="3" />
      </svg>
    </div>
  );
}

function SensesScene() {
  const {frame, fps} = useT();
  const px = personX(frame);
  const pxLag = personX(frame - 14);
  const toYaw = (x: number) => Math.max(-32, Math.min(32, (x - BOT2_EYES[0]) / 14));
  let look: XY = [Math.max(-1, Math.min(1, (px - BOT2_EYES[0]) / 380)), 0.1];
  let yaw = toYaw(pxLag);
  let pitch = 0;
  if (frame >= 206 && frame < 262) {
    look = [-0.9, 0.9];
    yaw = interpolate(frame, [206, 222], [toYaw(pxLag), -20], {extrapolateRight: 'clamp'});
    pitch = interpolate(frame, [206, 222], [0, 10], {extrapolateRight: 'clamp'});
  } else if (frame >= 262) {
    look = [0, -0.4];
    yaw = interpolate(frame, [262, 280], [-20, 0], {extrapolateRight: 'clamp'});
  }
  const pats = bump(frame, 276, 10) + bump(frame, 290, 10);
  const handY = interpolate(frame, [258, 274, 304, 322], [-140, BOT2.top + 10, BOT2.top + 10, -140], {extrapolateLeft: 'clamp', extrapolateRight: 'clamp', easing: Easing.inOut(Easing.cubic)}) + pats * 18;
  const mood: Mood = frame < 20 ? 'open' : frame < 70 ? 'wide' : frame < 206 ? 'open' : frame < 236 ? 'wide' : frame < 276 ? 'happy' : 'heart';
  const text = frame < 28 ? '' : frame < 80 ? 'listening…' : frame < 170 ? 'hi!' : frame < 236 ? 'looking…' : frame < 276 ? 'found it!' : 'hehe';
  const typeFrom = frame < 80 ? 28 : frame < 170 ? 80 : frame < 236 ? 170 : frame < 276 ? 236 : 276;
  const waves = frame >= 26 && frame < 76;
  const scan = prog(frame, 184, 214, Easing.inOut(Easing.quad));
  const box = sp(frame, fps, 214, SPRINGS.pop, 20);
  const beats: [string, number, number][] = [
    ['hears “hey eve”, on your Mac', 20, 76],
    ['follows whoever is talking', 90, 166],
    ['finds things for you', 172, 250],
    ['loves a head pat', 258, 320],
  ];
  return (
    <SceneFrame accent={C.sun} duration={330}>
      <div style={{position: 'absolute', left: 108, top: 84}}><Kicker delay={2}>on your desk · works today</Kicker><H1 size={80} delay={8} text="It listens, looks<br/>and feels." /></div>
      <div style={{position: 'absolute', left: 112, top: 420, display: 'grid', gap: 22}}>
        {beats.map(([label, s, e], i) => {
          const a = fade(frame, 24 + i * 8, 40 + i * 8);
          const active = frame >= s && frame < e;
          return (
            <div key={label} style={{display: 'flex', gap: 16, alignItems: 'center', opacity: a * (active ? 1 : frame >= e ? 0.9 : 0.45), transform: `translateX(${(1 - a) * -30 + (active ? 12 : 0)}px)`}}>
              <Tick at={e - 6} color={[C.pinkSoft, C.teal, C.sun, C.pink][i]} size={40} />
              <span style={{font: `700 ${active ? 32 : 28}px Andika`, color: C.ink, background: active ? `linear-gradient(transparent 58%, ${C.pinkSoft} 58%)` : undefined}}>{label}</span>
            </div>
          );
        })}
      </div>
      <Kicker size={27} delay={120} style={{position: 'absolute', left: 116, top: 760, opacity: frame < 200 ? 1 : 1 - fade(frame, 200, 214)}}>the eyes lead; the head follows.</Kicker>

      {frame >= 12 && <Person x={px} y={600} turn={px < BOT2_EYES[0] ? 1 : -1} />}
      {waves && [0, 1, 2].map((i) => {
        const t = ((frame - 26) / 24 + i / 3) % 1;
        return <div key={i} style={{position: 'absolute', left: px + 60 + t * 170, top: 520 - t * 30, width: 50, height: 110, borderRight: `6px solid ${C.sky}`, borderRadius: '0 60px 60px 0', opacity: Math.sin(t * Math.PI), transform: `scale(${0.6 + t * 0.6})`}} />;
      })}
      <div style={{position: 'absolute', left: px - 140, top: 380, opacity: frame >= 20 && frame < 90 ? 1 : 0}}><Speech color={C.sky} delay={20} tilt={-3}>“hey eve!”</Speech></div>
      {frame >= 96 && frame < 172 && <Brackets x={px - 70} y={528} w={140} h={130} label="face" color={C.teal} opacity={fade(frame, 96, 106) * (1 - fade(frame, 164, 172))} />}
      <div style={{position: 'absolute', left: 1520, top: 380, opacity: frame >= 176 && frame < 262 ? 1 : 0}}><Speech color={C.sun} delay={176} tilt={2}>“where's my mug?”</Speech></div>
      <Shelf frame={frame} />
      {frame >= 184 && frame < 222 && <div style={{position: 'absolute', left: 860 + scan * 880, top: 740, width: 60, height: 190, background: C.teal, opacity: 0.35 * (1 - fade(frame, 214, 222)), borderRadius: 10}} />}
      {frame >= 214 && <Brackets x={1052} y={805} w={130} h={108} label="mug" color={C.sun} opacity={Math.min(1, box * 1.3) * (1 - fade(frame, 300, 316))} />}
      <div style={{position: 'absolute', left: 1440, top: 196, opacity: frame >= 236 && frame < 300 ? 1 - fade(frame, 290, 300) : 0}}><Speech color={C.teal} delay={236} tilt={1}>“on the shelf, left of the plant.”</Speech></div>
      <div style={{position: 'absolute', left: BOT2.left, top: BOT2.top}}>
        <Bot scale={BOT2.scale} delay={4} text={text} typeFrom={typeFrom} mood={mood} look={look} yaw={yaw} pitch={pitch} listening={frame > 30 && frame < 80} squash={pats} leds={frame >= 276 ? C.pink : frame >= 30 && frame < 80 ? C.sky : null} />
      </div>
      <Chirp at={30} x={BOT2_EYES[0]} y={BOT2_EYES[1]} color={C.screenPink} />
      {frame >= 258 && frame < 324 && <Hand x={BOT2_EYES[0] + 10} y={handY} />}
      <FloatHearts start={280} x={BOT2_EYES[0] - 20} y={BOT2.top - 20} />
    </SceneFrame>
  );
}

// ---------- 3 · a life of its own ----------

function SkyWindow({frame}: {frame: number}) {
  const dusk = prog(frame, 110, 160, Easing.inOut(Easing.quad));
  const night = prog(frame, 160, 210, Easing.inOut(Easing.quad));
  const sky = night > 0 ? mixColor(C.pinkSoft, C.screen, night) : mixColor(SKY_DAY, C.pinkSoft, dusk);
  const sunT = prog(frame, 0, 190, Easing.inOut(Easing.sin));
  const sunX = lerp(90, 560, sunT);
  const sunY = 240 - Math.sin(lerp(0.35, 1, sunT) * Math.PI) * 170 + sunT * 120;
  const moon = prog(frame, 180, 250, Easing.out(Easing.cubic));
  return (
    <div style={{position: 'absolute', left: 1030, top: 196, width: 760, height: 440, borderRadius: 16, border: `3px solid ${C.ink}`, boxShadow: `12px 12px 0 ${C.sky}`, overflow: 'hidden', background: sky}}>
      <div style={{position: 'absolute', left: sunX - 55, top: sunY - 55, width: 110, height: 110, borderRadius: '50%', background: C.sun, border: `3px solid ${C.ink}`, boxShadow: `0 0 0 16px rgba(241,200,91,${0.25 * (1 - night)})`}} />
      <svg width="760" height="440" style={{position: 'absolute', inset: 0}}>
        {Array.from({length: 18}, (_, i) => {
          const tw = 0.5 + 0.5 * Math.sin(frame * 0.2 + i * 1.7);
          return <path key={i} d="M0 -8L2.5 -2.5 8 0 2.5 2.5 0 8-2.5 2.5-8 0-2.5-2.5Z" transform={`translate(${40 + ((i * 97) % 680)} ${30 + ((i * 53) % 220)}) scale(${0.8 + (i % 3) * 0.4})`} fill="#FFF6D8" opacity={night * tw} />;
        })}
        <g transform={`translate(600 ${lerp(470, 110, moon)})`}>
          <circle r="46" fill="#FFF6D8" stroke={C.ink} strokeWidth="3" />
          <circle cx="22" cy="-12" r="40" fill={sky} />
        </g>
        <path d="M0 360C120 320 220 350 330 330S560 300 760 340V440H0Z" fill={mixColor(C.teal, '#2B3A6B', night)} stroke={C.ink} strokeWidth="3" />
        <path d="M0 400C160 370 300 400 460 380S640 370 760 390V440H0Z" fill={mixColor('#5FAE99', '#223058', night)} stroke={C.ink} strokeWidth="3" />
      </svg>
      <div style={{position: 'absolute', left: 0, right: 0, top: 0, height: 440, boxShadow: 'inset 0 0 0 10px rgba(255,253,248,0.35)'}} />
    </div>
  );
}

function Polaroid({tilt = -6, small = false}: {tilt?: number; small?: boolean}) {
  const w = small ? 150 : 180;
  return (
    <div style={{width: w, padding: 10, paddingBottom: 30, background: C.sheet, border: `3px solid ${C.ink}`, borderRadius: 4, transform: `rotate(${tilt}deg)`, boxShadow: `5px 5px 0 ${C.pinkSoft}`}}>
      <div style={{height: w * 0.62, borderRadius: 3, border: `2px solid ${C.ink}`, background: `linear-gradient(180deg, ${C.desk} 0 60%, #CDBFA7 60%)`, position: 'relative', overflow: 'hidden'}}>
        <div style={{position: 'absolute', left: '22%', top: '34%', width: '18%', height: '40%', background: C.pink, border: `2px solid ${C.ink}`, borderRadius: '3px 3px 8px 8px'}} />
        <div style={{position: 'absolute', left: '52%', top: '46%', width: '22%', height: '28%', background: C.sun, border: `2px solid ${C.ink}`}} />
        <div style={{position: 'absolute', left: '50%', top: '14%', width: '26%', height: '34%', background: C.teal, border: `2px solid ${C.ink}`, borderRadius: '50% 50% 10% 10%'}} />
      </div>
    </div>
  );
}

function LifeScene() {
  const {frame, fps} = useT();
  const exploring = frame >= 16 && frame < 150;
  const yaw = exploring ? Math.sin((frame - 16) * 0.06) * 28 : 0;
  const snap = bump(frame, 78, 10);
  const night = frame >= 214;
  const mood: Mood = night ? 'closed' : frame < 150 ? (frame > 78 && frame < 100 ? 'happy' : 'open') : 'open';
  const text = night ? 'zzz' : frame < 78 ? 'exploring' : frame < 100 ? 'snap!' : frame < 150 ? 'exploring' : 'sleepy…';
  const typeFrom = night ? 214 : frame < 78 ? 18 : frame < 100 ? 78 : frame < 150 ? 100 : 150;
  const pinned = sp(frame, fps, 110, SPRINGS.pop, 20);
  const widget = sp(frame, fps, 236, SPRINGS.rise, 30);
  const moods: [string, string, number, number][] = [['curious', C.sun, 0, 120], ['content', C.teal, 120, 205], ['sleepy', C.sky, 205, 999]];
  const BOTL = {left: 1170, top: 470, scale: 1.1};
  return (
    <SceneFrame accent={C.sky} duration={300}>
      <div style={{position: 'absolute', left: 108, top: 70}}><Kicker delay={0}>a life of its own · works today</Kicker><H1 size={74} delay={6} text="When it's quiet,<br/>it lives a little." /></div>
      <Card color={C.sun} delay={16} from="left" style={{position: 'absolute', left: 108, top: 320, width: 820}} inner={{padding: '30px 34px 30px 70px', minHeight: 520, backgroundImage: `repeating-linear-gradient(transparent 0 57px, ${C.pinkSoft} 57px 60px)`, backgroundPosition: '0 84px'}}>
        <div style={{position: 'absolute', left: 18, top: 30, bottom: 30, display: 'flex', flexDirection: 'column', justifyContent: 'space-between'}}>
          {Array.from({length: 8}, (_, i) => <span key={i} style={{width: 22, height: 22, borderRadius: '50%', background: C.paper, border: `3px solid ${C.ink}`}} />)}
        </div>
        <div style={{font: '700 30px Grandstander', color: C.ink}}>eve's diary</div>
        <div style={{display: 'grid', gap: 22, marginTop: 30, font: '400 31px Gochi Hand', color: C.ink}}>
          <Handwriting text="10:14  a mug appeared by the plant." at={34} length={40} width={440} />
          <Handwriting text="15:02  quiet, so I went exploring." at={124} length={40} width={430} />
          <Handwriting text="22:40  thinking about today…" at={218} length={34} width={360} />
        </div>
      </Card>
      {frame >= 110 && <div style={{position: 'absolute', left: 660, top: 520, zIndex: 4, opacity: Math.min(1, pinned * 1.5), transform: `scale(${1.2 - 0.2 * pinned})`}}><Polaroid tilt={6} small /></div>}
      <Flyer from={[BOTL.left + 137, BOTL.top + 110]} to={[735, 590]} t0={82} t1={110} lift={-260} spin={-20} scaleTo={0.83} trail={C.pink}><Polaroid tilt={-8} /></Flyer>
      <Burst at={110} x={735} y={590} n={8} r={80} />
      <SkyWindow frame={frame} />
      <div style={{position: 'absolute', left: BOTL.left, top: BOTL.top, zIndex: 3}}>
        <Bot scale={BOTL.scale} delay={0} text={text} typeFrom={typeFrom} mood={mood} yaw={yaw} look={[yaw / 30, 0]} flash={snap} dim={night ? 0.35 : 0} leds={frame > 60 && frame < 110 ? C.sun : null} bob={!night} />
      </div>
      <FloatZs start={222} x={BOTL.left + 210} y={BOTL.top + 40} />
      <div style={{position: 'absolute', left: 108, top: 890, display: 'flex', gap: 12, alignItems: 'center'}}>
        <span style={{font: '400 28px Gochi Hand', color: C.inkSoft, marginRight: 6}}>mood:</span>
        {moods.map(([m, c, s, e]) => {
          const on = frame >= s && frame < e;
          return <span key={m} style={{padding: '8px 16px', border: `3px solid ${C.ink}`, borderRadius: 999, font: '700 20px Andika', background: on ? c : C.sheet, opacity: on ? 1 : 0.5, transform: `scale(${1 + bump(frame, s, 16) * 0.18})`, boxShadow: on ? `3px 3px 0 ${C.ink}` : undefined}}>{m}</span>;
        })}
        <span style={{font: '400 26px Gochi Hand', color: C.inkSoft, marginLeft: 12, opacity: fade(frame, 230, 246)}}>+ a reflection every night</span>
      </div>
      <div style={{position: 'absolute', left: 1470, top: 690, zIndex: 8, opacity: Math.min(1, widget * 1.4), transform: `translate(${(1 - widget) * 300}px, 0) rotate(${(1 - widget) * 6 + 2}deg)`}}>
        <div style={{background: C.sheet, border: `3px solid ${C.ink}`, borderRadius: 18, padding: 10, boxShadow: `8px 8px 0 ${C.pink}`}}>
          <Img src={staticFile('assets/widget-notes-medium.png')} style={{display: 'block', width: 380, borderRadius: 12}} />
        </div>
        <div style={{font: '400 26px Gochi Hand', color: C.ink, marginTop: 10, textAlign: 'right'}}>…and on your Mac's desktop.</div>
      </div>
    </SceneFrame>
  );
}

// ---------- 4 · pocket ----------

function AppTile({label, color, x, y, at}: {label: string; color: string; x: number; y: number; at: number}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, at, SPRINGS.pop, 22);
  return (
    <div style={{position: 'absolute', left: x, top: y + wave(frame, 6, 0.08, at), padding: '12px 18px', background: C.sheet, border: `3px solid ${C.ink}`, borderRadius: 12, boxShadow: `5px 5px 0 ${color}`, font: '700 22px Andika', color: C.ink, display: 'flex', gap: 10, alignItems: 'center', opacity: Math.min(1, a * 1.4), transform: `scale(${0.4 + 0.6 * a}) rotate(${wave(frame, 3, 0.07, at)}deg)`, zIndex: 4}}>
      <span style={{width: 16, height: 16, borderRadius: 4, background: color, border: `2px solid ${C.ink}`}} />{label}
    </div>
  );
}

function RundownRow({dot, text, at}: {dot: string; text: string; at: number}) {
  const frame = useCurrentFrame();
  const a = fade(frame, at, at + 10, Easing.out(Easing.back(2)));
  return (
    <div style={{display: 'flex', alignItems: 'center', gap: 10, opacity: Math.min(1, a), transform: `translateX(${(1 - a) * -14}px) scale(${0.8 + 0.2 * a})`, transformOrigin: '0 50%'}}>
      <span style={{width: 14, height: 14, borderRadius: 4, background: dot, border: `2px solid ${C.ink}`, flex: 'none'}} />
      <span>{text}</span>
    </div>
  );
}

function PocketScene() {
  const frame = useCurrentFrame();
  const P = {left: 1290, top: 140, h: 800};
  const zoom = prog(frame, 16, 46, Easing.in(Easing.cubic));
  const into: XY = [960, 438];
  const tiles: [string, string, XY][] = [['email', C.pink, [1000, 300]], ['calendar', C.sun, [960, 440]], ['Slack', C.sky, [1010, 580]], ['todos', C.teal, [960, 720]]];
  const land = phoneEve(P.left, P.top, P.h);
  const msgs: Msg[] = [
    {at: 76, me: true, text: 'rundown'},
    {
      at: 128,
      wide: true,
      text: (
        <div>
          <div style={{font: '700 22px Grandstander', marginBottom: 8}}>Here's today:</div>
          <div style={{display: 'grid', gap: 6}}>
            <RundownRow dot={C.pink} text="3 emails need you" at={134} />
            <RundownRow dot={C.sun} text="2 meetings, first at 10:30" at={142} />
            <RundownRow dot={C.sky} text="1 Slack mention" at={150} />
            <RundownRow dot={C.teal} text="4 todos, 1 overdue" at={158} />
          </div>
        </div>
      ),
    },
    {at: 184, me: true, text: 'send me a photo of my desk'},
    {at: 214, text: <div><Polaroid tilt={0} small /><div style={{marginTop: 6}}>from my camera, just now.</div></div>},
  ];
  const caps: [string, string][] = [
    ['chat with it, from anywhere', C.pinkSoft],
    ['start a task on your Mac, or stop one', C.sun],
    ['answer the questions a task asks', C.teal],
    ['get a photo from its camera', C.sky],
    ['“rundown”: email, calendar, Slack, todos', C.pinkSoft],
  ];
  return (
    <SceneFrame accent={C.teal} duration={270}>
      {frame < 52 && (
        <div style={{position: 'absolute', inset: 0, zIndex: 30, opacity: 1 - prog(frame, 38, 52), transform: `scale(${lerp(1, 11, zoom)})`, transformOrigin: `${into[0]}px ${into[1]}px`}}>
          <div style={{position: 'absolute', left: into[0] - 125, top: into[1] - 140}}><Bot scale={1} text="…" mood="wide" /></div>
          <Kicker size={34} color={C.ink} delay={0} style={{position: 'absolute', left: 760, top: 720}}>and when you leave the desk…</Kicker>
        </div>
      )}
      <div style={{position: 'absolute', left: 108, top: 84, opacity: fade(frame, 44, 52)}}>
        <Kicker delay={48}>new · text eve on Telegram</Kicker>
        <H1 size={86} delay={54} text="…it fits in<br/>your pocket." />
      </div>
      <div style={{position: 'absolute', left: 112, top: 410, width: 800, display: 'grid', gap: 16}}>
        {caps.map(([x, c], i) => {
          const a = fade(frame, 70 + i * 10, 84 + i * 10, Easing.out(Easing.back(1.4)));
          return (
            <div key={x} style={{display: 'flex', gap: 14, alignItems: 'center', opacity: Math.min(1, a), transform: `translateX(${(1 - a) * -30}px)`}}>
              <Tick at={80 + i * 10} color={c} size={34} />
              <span style={{font: '700 27px Andika', color: C.ink}}>{x}</span>
            </div>
          );
        })}
      </div>
      <div style={{position: 'absolute', left: 112, top: 820, display: 'flex', gap: 12}}><Tag color={C.teal} delay={130}>works today</Tag><Tag color={C.sun} delay={138}>opt-in</Tag></div>
      <Kicker size={26} delay={190} style={{position: 'absolute', left: 112, top: 900}}>rundown only reads: it never sends, marks or edits.</Kicker>
      <Phone msgs={msgs} delay={38} color={C.teal} height={P.h} left={P.left} top={P.top} from={{x: into[0], y: into[1], scale: 0.12}} />
      {tiles.map(([label, color, xy], i) => {
        const t0 = 104 + i * 7;
        return (
          <React.Fragment key={label}>
            {frame < t0 && <AppTile label={label} color={color} x={xy[0]} y={xy[1]} at={84 + i * 5} />}
            <Flyer from={[xy[0] + 80, xy[1] + 26]} to={[land[0], land[1] - 60 + i * 30]} t0={t0} t1={t0 + 20} lift={-60} spin={14} scaleTo={0.4} trail={color}>
              <div style={{padding: '12px 18px', background: C.sheet, border: `3px solid ${C.ink}`, borderRadius: 12, boxShadow: `5px 5px 0 ${color}`, font: '700 22px Andika', color: C.ink}}>{label}</div>
            </Flyer>
          </React.Fragment>
        );
      })}
      <Burst at={128} x={land[0]} y={land[1] - 30} n={10} r={90} />
    </SceneFrame>
  );
}

// ---------- 5 · your own Chrome ----------

const CH = {left: 700, top: 236, w: 1080};
const CP = {left: 120, top: 236, h: 740};
const ALLOW: XY = [CH.left + 3 + 290 + 430, CH.top + 45 + 150 + 128];

function ConsentDialog({showAt, pressAt, goneAt}: {showAt: number; pressAt: number; goneAt: number}) {
  const {frame, fps} = useT();
  if (frame < showAt || frame >= goneAt) return null;
  const a = sp(frame, fps, showAt, SPRINGS.pop, 20);
  const out = fade(frame, goneAt - 8, goneAt);
  const down = frame >= pressAt && frame < pressAt + 8 ? 1 : 0;
  const glow = bump(frame, pressAt, 20);
  return (
    <div style={{position: 'absolute', left: 290, top: 150, width: 470, padding: 26, background: C.sheet, border: `3px solid ${C.ink}`, borderRadius: 12, boxShadow: `8px 8px 0 ${C.sun}`, zIndex: 4, opacity: Math.min(1, a * 1.4) * (1 - out), transform: `scale(${(0.7 + 0.3 * a) * (1 - out * 0.1)})`}}>
      <div style={{font: '700 26px Grandstander', color: C.ink}}>Allow remote debugging?</div>
      <div style={{font: '400 18px Andika', color: C.inkSoft, marginTop: 8}}>Chrome asks this for every new connection.</div>
      <div style={{display: 'flex', justifyContent: 'flex-end', gap: 12, marginTop: 20}}>
        <span style={{padding: '10px 18px', border: `3px solid ${C.ink}`, borderRadius: 8, font: '700 18px Andika', background: C.sheet}}>Cancel</span>
        <span style={{padding: '10px 18px', border: `3px solid ${C.ink}`, borderRadius: 8, font: '700 18px Andika', background: C.teal, boxShadow: `${4 - 3 * down}px ${4 - 3 * down}px 0 ${C.ink}`, transform: `translate(${3 * down}px, ${3 * down}px) scale(${1 + glow * 0.1})`}}>Allow</span>
      </div>
    </div>
  );
}

function Product({i, at, pick, readAt}: {i: number; at: number; pick: boolean; readAt: number}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, at + i * 4, SPRINGS.pop, 20);
  const prices = ['$24', '$39', '$19', '$45', '$29', '$52'];
  const hl = pick ? fade(frame, readAt + i * 5, readAt + 12 + i * 5) : 0;
  const dimmed = !pick && frame > readAt + 30 ? 0.45 : 1;
  return (
    <div style={{background: C.sheet, border: `3px solid ${C.ink}`, borderRadius: 8, padding: 14, opacity: Math.min(1, a * 1.4) * dimmed, transform: `scale(${(0.7 + 0.3 * a) * (1 + bump(frame, readAt + i * 5, 12) * 0.05 * (pick ? 1 : 0))})`, boxShadow: `${7 * hl}px ${7 * hl}px 0 ${C.teal}`, position: 'relative', display: 'flex', gap: 14, alignItems: 'center'}}>
      <div style={{width: 96, height: 82, borderRadius: 6, background: [C.pinkSoft, C.desk, '#DDE6F6'][i % 3], border: `2px solid ${C.ink}`, flex: 'none', position: 'relative'}}>
        <div style={{position: 'absolute', left: 22, top: 30, width: 52, height: 22, borderRadius: 6, background: C.inkSoft, opacity: 0.6}} />
      </div>
      <div style={{flex: 1}}>
        <div style={{height: 10, width: '90%', background: C.desk, borderRadius: 4}} />
        <div style={{height: 10, width: '60%', background: C.desk, borderRadius: 4, marginTop: 8}} />
        <div style={{font: '700 24px Andika', color: C.ink, marginTop: 10}}>{prices[i]}</div>
      </div>
      {pick && <span style={{position: 'absolute', right: -12, top: -14, width: 36, height: 36, borderRadius: '50%', background: C.teal, border: `3px solid ${C.ink}`, font: '700 20px/30px Andika', textAlign: 'center', opacity: hl, transform: `scale(${hl * (1 + bump(frame, readAt + 8 + i * 5, 10) * 0.4)})`}}>✓</span>}
    </div>
  );
}

function ChromeWindow() {
  const frame = useCurrentFrame();
  const tabOpen = fade(frame, 152, 168);
  const url = 'amazon.com/s?k=usb-c+hub';
  const typed = url.slice(0, Math.max(0, Math.floor((frame - 170) / 1.3)));
  const results = 206;
  const readAt = 244;
  const scan = prog(frame, readAt - 6, readAt + 40, Easing.inOut(Easing.quad));
  const shutter = bump(frame, 300, 12);
  return (
    <BrowserWindow title="Chrome · your profile, already signed in" style={{width: CH.w, height: 600}}>
      <div style={{position: 'relative', height: 555, background: C.paper, overflow: 'hidden'}}>
        <div style={{display: 'flex', gap: 6, padding: '10px 14px 0', borderBottom: `3px solid ${C.ink}`}}>
          <span style={{padding: '8px 16px', border: `3px solid ${C.ink}`, borderBottom: 'none', borderRadius: '10px 10px 0 0', font: '700 16px Andika', background: frame < 160 ? C.sheet : C.desk, color: C.inkSoft}}>your tab</span>
          <span style={{padding: '8px 16px', border: `3px solid ${C.ink}`, borderBottom: 'none', borderRadius: '10px 10px 0 0', font: '700 16px Andika', background: C.sun, opacity: tabOpen, transform: `translateY(${(1 - tabOpen) * 30}px)`}}>eve's own tab</span>
        </div>
        <div style={{margin: '14px 18px', padding: '10px 16px', border: `3px solid ${C.ink}`, borderRadius: 999, background: C.sheet, font: `400 19px ${MONO}`, color: C.ink, minHeight: 26}}>
          {frame >= 170 ? typed : frame < 160 ? 'mail.google.com · your inbox, left alone' : ''}
          {frame >= 170 && typed.length < url.length && frame % 10 < 6 ? '▌' : ''}
        </div>
        <div style={{display: 'grid', gridTemplateColumns: 'repeat(3, 1fr)', gap: 18, padding: '8px 22px'}}>
          {frame >= results && [0, 1, 2, 3, 4, 5].map((i) => <Product key={i} i={i} at={results} pick={[0, 2, 4].includes(i)} readAt={readAt} />)}
        </div>
        {frame >= readAt - 6 && frame < readAt + 48 && (
          <div style={{position: 'absolute', left: 0, right: 0, top: 118 + scan * 330, height: 8, background: C.teal, opacity: 0.8 * (1 - fade(frame, readAt + 40, readAt + 48)), boxShadow: `0 0 0 6px rgba(121,198,178,0.3)`}}>
            <span style={{position: 'absolute', right: 20, top: -38, padding: '4px 12px', background: C.teal, border: `3px solid ${C.ink}`, borderRadius: 999, font: '700 16px Andika'}}>reading the page</span>
          </div>
        )}
        <ConsentDialog showAt={56} pressAt={136} goneAt={152} />
        <Cursor path={[{at: 186, x: 900, y: 420}, {at: 214, x: 640, y: 210}, {at: 250, x: 200, y: 260}, {at: 280, x: 520, y: 400}, {at: 300, x: 860, y: 300}]} />
        {shutter > 0 && <div style={{position: 'absolute', inset: 0, background: '#FFFFFF', opacity: shutter * 0.9}} />}
      </div>
    </BrowserWindow>
  );
}

function ChromeScene() {
  const {frame, fps} = useT();
  const win = sp(frame, fps, 16, SPRINGS.rise, 34);
  const me = phoneMe(CP.left, CP.top, CP.h);
  const shot = (
    <div style={{width: 260, height: 104, borderRadius: 8, border: `3px solid ${C.ink}`, background: C.paper, display: 'grid', gridTemplateColumns: 'repeat(3, 1fr)', gap: 6, padding: 8}}>
      {[0, 1, 2].map((i) => <div key={i} style={{borderRadius: 4, border: `2px solid ${C.ink}`, background: [C.pinkSoft, '#DDE6F6', C.desk][i], boxShadow: `3px 3px 0 ${C.teal}`}} />)}
    </div>
  );
  const msgs: Msg[] = [
    {at: 30, me: true, text: 'find a usb-c hub under $30 on amazon'},
    {at: 72, text: 'eve wants to control your Chrome.\nAllow it? yes / no'},
    {at: 112, me: true, text: 'yes'},
    {at: 306, text: 'Found 3 under $30. Here is the page:'},
    {at: 336, text: shot},
  ];
  return (
    <SceneFrame accent={C.sun} duration={420}>
      <div style={{position: 'absolute', left: 108, top: 50}}><Kicker delay={0}>new · from your phone to your Mac</Kicker><H1 size={66} delay={6} text="Your own Chrome, from anywhere." /></div>
      <Phone msgs={msgs} delay={10} color={C.sun} height={CP.h} left={CP.left} top={CP.top} />
      <div style={{position: 'absolute', left: CH.left, top: CH.top, opacity: win, transform: `translateY(${(1 - win) * 70 + wave(frame, 3, 0.04)}px) rotate(${(1 - win) * 2}deg)`}}>
        <ChromeWindow />
      </div>
      <Flyer from={me} to={ALLOW} t0={116} t1={136} lift={-220} spin={-16} scaleTo={0.8} trail={C.teal}>
        <div style={{padding: '10px 18px', background: C.teal, border: `3px solid ${C.ink}`, borderRadius: 20, font: '700 24px Andika', color: C.ink}}>yes</div>
      </Flyer>
      <Burst at={136} x={ALLOW[0]} y={ALLOW[1]} />
      <Flyer from={[CH.left + CH.w / 2, CH.top + 330]} to={[phoneEve(CP.left, CP.top, CP.h)[0] + 40, phoneEve(CP.left, CP.top, CP.h)[1] - 10]} t0={306} t1={334} lift={-200} spin={12} scaleTo={0.35} trail={C.pink}>
        <div style={{width: 560, height: 300, borderRadius: 10, border: `3px solid ${C.ink}`, background: C.paper, boxShadow: `8px 8px 0 ${C.pink}`, display: 'grid', gridTemplateColumns: 'repeat(3, 1fr)', gap: 12, padding: 16}}>
          {[0, 1, 2].map((i) => <div key={i} style={{borderRadius: 6, border: `3px solid ${C.ink}`, background: [C.pinkSoft, '#DDE6F6', C.desk][i], boxShadow: `4px 4px 0 ${C.teal}`}} />)}
        </div>
      </Flyer>
      <div style={{position: 'absolute', left: CH.left, top: 880, display: 'flex', gap: 12, alignItems: 'center'}}>
        <Pill color={C.teal} size={20} delay={170} lit>your logged-in Chrome</Pill>
        <Pill color={C.sun} size={20} delay={182} lit>a yes from your phone</Pill>
        <Pill color={C.pinkSoft} size={20} delay={320} lit>a screenshot back</Pill>
        <Tag color={C.sun} delay={194}>opt-in</Tag>
      </div>
      <Kicker size={26} delay={356} style={{position: 'absolute', left: CH.left, top: 954, marginBottom: 0}}>a no, or no answer at all, means Cancel.</Kicker>
    </SceneFrame>
  );
}

// ---------- 6 · how it chooses ----------

const ROUTER: XY = [600, 590];
const STATIONS = [352, 582, 812];
const STATION_X = 1170;

function track(i: number): [XY, XY, XY, XY] {
  return [ROUTER, [ROUTER[0] + 280, ROUTER[1]], [STATION_X - 300, STATIONS[i]], [STATION_X, STATIONS[i]]];
}

function Station({i, lane, detail, color, arriveAt, chips}: {i: number; lane: string; detail: string; color: string; arriveAt: number; chips?: string[]}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, 30 + i * 8, SPRINGS.rise, 28);
  const hit = bump(frame, arriveAt, 18);
  const lit = frame >= arriveAt;
  return (
    <div style={{position: 'absolute', left: STATION_X, top: STATIONS[i] - 72, width: 640, minHeight: 144, padding: '20px 24px', background: lit ? C.sheet : C.paper, border: `3px ${lit ? 'solid' : 'dashed'} ${C.ink}`, borderRadius: 12, boxShadow: `${(lit ? 9 : 0) * a}px ${(lit ? 9 : 0) * a}px 0 ${color}`, opacity: a, transform: `translateX(${(1 - a) * 80 - hit * 14}px) scale(${1 + hit * 0.05})`, boxSizing: 'border-box'}}>
      <div style={{font: '700 32px/1 Grandstander', color: C.ink}}>{lane}</div>
      <div style={{display: 'flex', alignItems: 'center', gap: 10, marginTop: 12, flexWrap: 'wrap'}}>
        <span style={{font: '400 21px Andika', color: C.inkSoft}}>{detail}</span>
        {chips?.map((c, k) => <Pill key={c} color={C.paper} size={17} delay={arriveAt + 6 + k * 7} lit>{c}</Pill>)}
      </div>
    </div>
  );
}

function RoutesScene() {
  const frame = useCurrentFrame();
  const reqs = [
    {say: '“open Safari”', token: 'open Safari', color: C.teal, at: 26, go: 52, arrive: 84, look: [0.8, -0.8] as XY},
    {say: '“how many unread in my inbox?”', token: 'unread?', color: C.sun, at: 100, go: 126, arrive: 158, look: [1, 0] as XY},
    {say: '“sort my downloads by type”', token: 'sort downloads', color: C.pink, at: 174, go: 200, arrive: 232, look: [0.8, 0.8] as XY},
  ];
  const cur = [...reqs].reverse().find((r) => frame >= r.at);
  const draw = prog(frame, 16, 50, Easing.inOut(Easing.quad));
  const mood: Mood = cur && frame >= cur.arrive && frame < cur.arrive + 14 ? 'happy' : cur && frame >= cur.at ? 'wide' : 'open';
  return (
    <SceneFrame accent={C.pink} duration={300}>
      <div style={{position: 'absolute', left: 108, top: 60}}><Kicker delay={0}>how it chooses · works today</Kicker><H1 size={66} delay={6} text="The quickest path that's still safe." /></div>
      <svg width="1920" height="1080" style={{position: 'absolute', left: 0, top: 0, overflow: 'visible'}}>
        {[0, 1, 2].map((i) => {
          const [p0, c1, c2, p1] = track(i);
          const active = reqs[i] && frame >= reqs[i].go && frame < reqs[i].arrive + 20;
          const used = reqs[i] && frame >= reqs[i].arrive;
          return (
            <g key={i}>
              <path d={`M${p0[0]} ${p0[1]}C${c1[0]} ${c1[1]} ${c2[0]} ${c2[1]} ${p1[0]} ${p1[1]}`} fill="none" stroke={C.ink} strokeWidth={active ? 7 : 4} strokeLinecap="round" strokeDasharray="900" strokeDashoffset={900 * (1 - draw)} opacity={used || active ? 1 : 0.35} />
              {(active || used) && <path d={`M${p0[0]} ${p0[1]}C${c1[0]} ${c1[1]} ${c2[0]} ${c2[1]} ${p1[0]} ${p1[1]}`} fill="none" stroke={reqs[i].color} strokeWidth={active ? 14 : 8} strokeLinecap="round" opacity={0.55} />}
            </g>
          );
        })}
      </svg>
      {reqs.map((r, i) => {
        if (frame < r.go || frame > r.arrive) return null;
        const t = prog(frame, r.go, r.arrive);
        const [x, y] = cubic(...track(i), t);
        return <div key={r.token} style={{position: 'absolute', left: x, top: y, zIndex: 8, transform: `translate(-50%, -50%) rotate(${Math.sin(t * Math.PI) * 6}deg)`, padding: '10px 16px', background: r.color, border: `3px solid ${C.ink}`, borderRadius: 999, font: '700 20px Andika', color: C.ink, whiteSpace: 'nowrap', boxShadow: `3px 3px 0 ${C.ink}`}}>{r.token}</div>;
      })}
      {reqs.map((r, i) => <Burst key={i} at={r.arrive} x={STATION_X} y={STATIONS[i]} n={9} r={80} colors={[r.color, C.sheet]} />)}
      <Station i={0} lane="a code shortcut" detail="no model, no waiting" color={C.teal} arriveAt={84} />
      <Station i={1} lane="your own Chrome" detail="reads the page you'd see" color={C.sun} arriveAt={158} />
      <Station i={2} lane="Codex Computer Use" detail="asks first:" color={C.pink} arriveAt={232} chips={['yes', 'allow for task', 'always allow']} />
      <div style={{position: 'absolute', left: ROUTER[0] - 240, top: ROUTER[1] - 150}}>
        <Bot scale={0.9} delay={10} mood={mood} look={cur ? cur.look : [0, 0]} text={cur ? (frame < cur.arrive ? 'routing' : 'on it') : 'hi'} typeFrom={cur ? (frame < cur.arrive ? cur.at : cur.arrive) : 10} />
      </div>
      {reqs.map((r) => {
        const on = frame >= r.at && (r === cur);
        const out = cur !== r && frame >= r.at;
        return (
          <div key={r.say} style={{position: 'absolute', left: 108, top: 290, opacity: on ? 1 : out ? 0 : 0}}>
            <Speech color={r.color} delay={r.at} tilt={-2}>{r.say}</Speech>
          </div>
        );
      })}
      <div style={{position: 'absolute', left: 108, top: 790, display: 'grid', gap: 12, justifyItems: 'start'}}>
        <Pill color={C.sheet} size={21} delay={244} lit>asks before anything big</Pill>
        <Pill color={C.sheet} size={21} delay={252} lit>say “stop” anytime</Pill>
        <Pill color={C.sheet} size={21} delay={260} lit>a timeout never approves</Pill>
      </div>
    </SceneFrame>
  );
}

// ---------- 7 · coding sessions ----------

const KP = {left: 120, top: 236, h: 740};
const TERM = {left: 700, top: 250, w: 1080};

function TermLine({at, children, color = '#E8ECFA', typeIt = false}: {at: number; children: string; color?: string; typeIt?: boolean}) {
  const frame = useCurrentFrame();
  if (frame < at) return null;
  const shown = typeIt ? children.slice(0, Math.floor((frame - at) / 0.9)) : children;
  const caret = typeIt && shown.length < children.length && frame % 10 < 6 ? '▌' : '';
  return <div style={{font: `400 22px/1.55 ${MONO}`, color, whiteSpace: 'pre'}}>{shown}{caret}</div>;
}

function Terminal() {
  const {frame, fps} = useT();
  const a = sp(frame, fps, 152, SPRINGS.rise, 34);
  const pick = frame >= 312;
  const lift = frame >= 250 && frame < 254;
  return (
    <div style={{perspective: 1400}}>
      <div style={{width: TERM.w, background: C.screen, border: `3px solid ${C.ink}`, borderRadius: 12, boxShadow: `12px 12px 0 ${C.pink}`, overflow: 'hidden', opacity: frame >= 152 ? 1 : 0, transform: `rotateX(${(1 - a) * 88}deg)`, transformOrigin: '50% 0%'}}>
        <div style={{height: 42, background: C.pinkSoft, borderBottom: `3px solid ${C.ink}`, display: 'flex', alignItems: 'center', gap: 10, padding: '0 16px'}}>
          {[C.pink, C.sun, C.teal].map((c) => <span key={c} style={{width: 13, height: 13, borderRadius: '50%', background: c, border: `2px solid ${C.ink}`}} />)}
          <span style={{marginLeft: 8, font: '700 15px Andika', color: C.inkSoft}}>Warp · a new window, opened by eve</span>
        </div>
        <div style={{padding: '22px 28px', height: 330}}>
          <TermLine at={172} color="#9BB6E6">~/Documents/personal/eve</TermLine>
          <TermLine at={180} typeIt>{'$ claude --dangerously-skip-permissions'}</TermLine>
          <TermLine at={222} color={C.teal}>{'● Claude Code is running in eve'}</TermLine>
          {frame >= 246 && <div style={{font: `400 22px/1.55 ${MONO}`, color: C.sun, whiteSpace: 'pre', opacity: lift ? 0.3 : 1}}>{'? Quit all?'}</div>}
          {frame >= 250 && (
            <div style={{font: `400 22px/1.55 ${MONO}`, whiteSpace: 'pre'}}>
              <span style={{color: pick ? C.screen : '#E8ECFA', background: pick ? C.teal : 'transparent', padding: '0 6px', borderRadius: 4, display: 'inline-block', transform: `scale(${1 + bump(frame, 312, 14) * 0.08})`}}>{'› 1. Keep terminals'}</span>
              <div style={{color: '#E8ECFA', padding: '0 6px'}}>{'  2. Everything'}</div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function StateChip({label, color, from, to}: {label: string; color: string; from: number; to: number}) {
  const frame = useCurrentFrame();
  const on = frame >= from && frame < to;
  const b = bump(frame, from, 16);
  return <span style={{display: 'inline-block', padding: '10px 18px', border: `3px solid ${C.ink}`, borderRadius: 999, font: '700 20px Andika', background: on ? color : C.sheet, color: C.ink, opacity: on ? 1 : 0.5, transform: `scale(${1 + b * 0.15})`, boxShadow: on ? `3px 3px 0 ${C.ink}` : undefined}}>{label}</span>;
}

function PaperPlane() {
  return (
    <svg width="90" height="70" viewBox="0 0 90 70">
      <path d="M4 34L86 4 60 66 42 44Z" fill={C.sheet} stroke={C.ink} strokeWidth="3" strokeLinejoin="round" />
      <path d="M42 44L86 4 30 38" fill={C.sky} stroke={C.ink} strokeWidth="3" strokeLinejoin="round" />
    </svg>
  );
}

function CodingScene() {
  const frame = useCurrentFrame();
  const msgs: Msg[] = [
    {at: 16, me: true, text: 'new claude'},
    {at: 40, text: 'Personal or Work?'},
    {at: 62, me: true, text: 'personal'},
    {at: 86, text: 'General, or which folder?'},
    {at: 108, me: true, text: 'eve'},
    {at: 130, text: 'Opening Claude Code in eve.'},
    {at: 196, me: true, text: 'claude on'},
    {at: 272, text: 'Claude asks: Quit all?\n1. Keep terminals\n2. Everything'},
    {at: 294, me: true, text: '1'},
  ];
  const working = frame >= 172 && frame < 250;
  const needs = frame >= 250 && frame < 312;
  const done = frame >= 312;
  const mood: Mood = frame < 172 ? 'closed' : working ? 'open' : needs ? 'wide' : 'happy';
  const text = frame < 172 ? 'zzz' : working ? 'working' : needs ? 'needs you' : 'done!';
  const typeFrom = frame < 172 ? 0 : working ? 172 : needs ? 250 : 312;
  return (
    <SceneFrame accent={C.pink} duration={360}>
      <div style={{position: 'absolute', left: 108, top: 50}}><Kicker delay={0}>new · for people who build things</Kicker><H1 size={66} delay={6} text="Start coding with a text." /></div>
      <Phone msgs={msgs} delay={8} color={C.pink} height={KP.h} left={KP.left} top={KP.top} />
      <div style={{position: 'absolute', left: TERM.left, top: TERM.top}}><Terminal /></div>
      <Flyer from={phoneEve(KP.left, KP.top, KP.h)} to={[TERM.left + TERM.w / 2, TERM.top + 120]} t0={134} t1={158} lift={-260} spin={24} scaleTo={1.2} trail={C.sky}><PaperPlane /></Flyer>
      <Flyer from={[TERM.left + 150, TERM.top + 170]} to={phoneEve(KP.left, KP.top, KP.h)} t0={250} t1={272} lift={-160} spin={-10} scaleTo={0.8} trail={C.sun}>
        <div style={{padding: '10px 16px', background: C.screen, border: `3px solid ${C.ink}`, borderRadius: 10, font: `400 22px ${MONO}`, color: C.sun}}>? Quit all?</div>
      </Flyer>
      <Flyer from={phoneMe(KP.left, KP.top, KP.h)} to={[TERM.left + 170, TERM.top + 205]} t0={296} t1={312} lift={-140} spin={10} scaleTo={0.9} trail={C.teal}>
        <div style={{padding: '8px 18px', background: C.teal, border: `3px solid ${C.ink}`, borderRadius: 20, font: '700 24px Andika'}}>1</div>
      </Flyer>
      <Burst at={312} x={TERM.left + 170} y={TERM.top + 205} />
      <div style={{position: 'absolute', left: TERM.left, top: 690, display: 'flex', alignItems: 'center', gap: 26}}>
        <Bot scale={0.8} delay={20} text={text} typeFrom={typeFrom} mood={mood} look={working ? [0.3, 0.8] : [0, 0]} leds={needs ? C.sun : done ? C.teal : null} dim={frame < 172 ? 0.3 : 0} bob={frame >= 172} />
        <div>
          <Kicker size={28} color={C.ink} delay={176}>and the robot on your desk keeps up:</Kicker>
          <div style={{display: 'flex', gap: 10, alignItems: 'center'}}>
            <StateChip label="working" color={C.sky} from={172} to={250} />
            <StateChip label="needs you" color={C.sun} from={250} to={312} />
            <StateChip label="done!" color={C.teal} from={312} to={9999} />
          </div>
        </div>
      </div>
      {done && <FloatHearts start={316} x={TERM.left + 80} y={700} />}
      <Kicker size={25} delay={110} style={{position: 'absolute', right: 110, top: 160, marginBottom: 0}}>each step is plain code: no model call.</Kicker>
    </SceneFrame>
  );
}

// ---------- 8 · second brain ----------

const BP = {left: 1330, top: 200, h: 760};

function VaultRow({depth = 0, name, at, folder = false, highlight, checkAt, bumpAt}: {depth?: number; name: string; at?: number; folder?: boolean; highlight?: string; checkAt?: number; bumpAt?: number[]}) {
  const {frame, fps} = useT();
  if (at !== undefined && frame < at) return null;
  const a = at === undefined ? 1 : sp(frame, fps, at, SPRINGS.pop, 20);
  const hl = at === undefined ? 0 : 1 - fade(frame, at + 60, at + 100);
  const b = (bumpAt ?? []).reduce((s, t) => s + bump(frame, t, 14), 0);
  const checked = checkAt !== undefined && frame >= checkAt;
  return (
    <div style={{display: 'flex', alignItems: 'center', gap: 12, paddingLeft: depth * 36, opacity: Math.min(1, a * 1.4), transform: `translateX(${(1 - a) * -20}px)`}}>
      {folder ? (
        <svg width="30" height="24" viewBox="0 0 30 24" style={{transform: `translateY(${-b * 6}px) rotate(${-b * 8}deg)`, overflow: 'visible'}}>
          <path d="M2 4h9l3 3h14v15H2z" fill={C.sun} stroke={C.ink} strokeWidth="2.5" strokeLinejoin="round" />
          <path d={`M2 ${10 - b * 6}h26v12H2z`} fill="#F6D98A" stroke={C.ink} strokeWidth="2.5" strokeLinejoin="round" />
        </svg>
      ) : checkAt !== undefined ? (
        <Tick at={checkAt} color={C.teal} size={28} />
      ) : (
        <svg width="22" height="26" viewBox="0 0 22 26"><path d="M2 2h12l6 6v16H2z" fill={C.sheet} stroke={C.ink} strokeWidth="2.5" strokeLinejoin="round" /></svg>
      )}
      <span style={{font: `${folder ? 700 : 400} 22px ${folder ? 'Andika' : MONO}`, color: C.ink, padding: '4px 8px', borderRadius: 6, background: highlight && hl > 0 ? `color-mix(in srgb, ${highlight} ${Math.round(hl * 100)}%, transparent)` : 'transparent', textDecoration: checked ? 'line-through' : undefined, textDecorationThickness: 3}}>{name}</span>
    </div>
  );
}

function NotePaper({lines = 2, color = C.sun}: {lines?: number; color?: string}) {
  return (
    <div style={{width: 120, padding: '14px 12px', background: C.sheet, border: `3px solid ${C.ink}`, borderRadius: 4, boxShadow: `4px 4px 0 ${color}`, display: 'grid', gap: 7}}>
      {Array.from({length: lines}, (_, i) => <div key={i} style={{height: 6, width: `${90 - i * 25}%`, background: C.inkSoft, borderRadius: 3, opacity: 0.6}} />)}
    </div>
  );
}

function BrainScene() {
  const me = phoneMe(BP.left, BP.top, BP.h);
  const msgs: Msg[] = [
    {at: 34, me: true, text: 'note: the pasta place is Doppio'},
    {at: 72, text: 'Saved to your inbox.'},
    {at: 104, me: true, text: 'todo: p1 book the dentist'},
    {at: 142, text: 'Added to your todos.'},
    {at: 176, me: true, text: 'booked the dentist'},
    {at: 214, text: 'Checked it off.'},
  ];
  return (
    <SceneFrame accent={C.teal} duration={270}>
      <div style={{position: 'absolute', left: 108, top: 60}}><Kicker delay={0}>new · the second brain</Kicker><H1 size={70} delay={6} text="Text it. It's in your notes." /></div>
      <Card color={C.teal} delay={18} from="left" style={{position: 'absolute', left: 108, top: 270, width: 1080}} inner={{padding: 34, minHeight: 520}}>
        <Kicker size={28} delay={30}>your Obsidian vault, on your Mac</Kicker>
        <div style={{display: 'grid', gap: 14, marginTop: 10}}>
          <VaultRow folder name="Second Brain/" />
          <VaultRow folder depth={1} name="01-inbox/" bumpAt={[64]} />
          <VaultRow depth={2} name="2026-09-23-1830-the-pasta-place-is-doppio.md" at={66} highlight={C.pinkSoft} />
          <VaultRow folder depth={1} name="02-todos/" bumpAt={[134]} />
          <VaultRow depth={2} name="master.md" />
          <VaultRow depth={3} name="p1 book the dentist" at={136} highlight={C.sun} checkAt={206} />
          <VaultRow folder depth={1} name="08-journals/daily/" />
        </div>
      </Card>
      <Flyer from={me} to={[250, 430]} t0={40} t1={64} lift={-240} spin={-18} scaleTo={0.6} trail={C.pink}><NotePaper color={C.pink} /></Flyer>
      <Flyer from={me} to={[250, 560]} t0={110} t1={134} lift={-240} spin={-18} scaleTo={0.6} trail={C.sun}><NotePaper color={C.sun} lines={1} /></Flyer>
      <Flyer from={me} to={[290, 640]} t0={182} t1={206} lift={-240} spin={20} scaleTo={0.8} trail={C.teal}>
        <svg width="54" height="54" viewBox="0 0 30 30"><circle cx="15" cy="15" r="12.5" fill={C.teal} stroke={C.ink} strokeWidth="3" /><path d="M8.5 15.5l4.5 4.5 8.5-9" fill="none" stroke={C.ink} strokeWidth="3.4" strokeLinecap="round" strokeLinejoin="round" /></svg>
      </Flyer>
      <Burst at={66} x={560} y={458} n={8} r={80} />
      <Burst at={206} x={290} y={640} n={10} r={90} />
      <div style={{position: 'absolute', left: 108, top: 880, display: 'flex', gap: 12, alignItems: 'center'}}>
        <Tag color={C.teal} delay={150}>plain markdown</Tag>
        <Tag color={C.pinkSoft} delay={158}>every edit can be undone</Tag>
        <Tag color={C.sun} delay={166}>opt-in</Tag>
      </div>
      <Phone msgs={msgs} delay={12} color={C.teal} height={BP.h} left={BP.left} top={BP.top} />
    </SceneFrame>
  );
}

// ---------- 9 · everything else ----------

function OrbitScene() {
  const {frame, fps} = useT();
  const feats: [string, string][] = [
    ['meeting notes', C.sun], ['finds your mug', C.teal], ['keeps a diary', C.pinkSoft], ['desktop widget', C.sky],
    ['explores the room', C.sun], ['takes photos', C.teal], ['web search', C.pinkSoft], ['math lessons, one step at a time', C.sky],
    ['remembers what matters', C.sun], ['dances', C.teal], ['mirrors Claude Code', C.pinkSoft], ['night reflections', C.sky],
  ];
  const center: XY = [960, 650];
  const mood: Mood = frame < 60 ? 'happy' : frame < 110 ? 'wink' : 'heart';
  return (
    <SceneFrame accent={C.sky} duration={180}>
      <div style={{position: 'absolute', left: 0, right: 0, top: 70, textAlign: 'center'}}><Kicker delay={0} style={{display: 'inline-block'}}>and everything it already does</Kicker><H1 size={76} delay={6} text="A whole sidekick, not one trick." /></div>
      <svg width="1920" height="1080" style={{position: 'absolute', left: 0, top: 0}}>
        <ellipse cx={center[0]} cy={center[1]} rx={720} ry={290} fill="none" stroke={C.ink} strokeWidth="3" strokeDasharray="4 16" strokeDashoffset={-frame * 1.2} opacity={fade(frame, 10, 30) * 0.5} />
      </svg>
      {feats.map(([x, c], i) => {
        const ang = (i / feats.length) * Math.PI * 2 + frame * 0.006 - Math.PI / 2;
        const px = center[0] + Math.cos(ang) * 720;
        const py = center[1] + Math.sin(ang) * 290;
        const a = sp(frame, fps, 18 + i * 5, SPRINGS.pop, 24);
        const depth = (Math.sin(ang) + 1) / 2;
        return (
          <div key={x} style={{position: 'absolute', left: px, top: py, zIndex: Math.round(depth * 10) + (depth > 0.5 ? 10 : 0), transform: `translate(-50%, -50%) scale(${(0.35 + 0.65 * a) * (0.82 + depth * 0.28)})`, opacity: Math.min(1, a * 1.4)}}>
            <span style={{display: 'inline-block', padding: '14px 22px', border: `3px solid ${C.ink}`, borderRadius: 999, background: c, font: '700 25px Andika', color: C.ink, whiteSpace: 'nowrap', boxShadow: `4px 4px 0 ${C.ink}`}}>{x}</span>
          </div>
        );
      })}
      <div style={{position: 'absolute', left: center[0] - 187, top: center[1] - 250, zIndex: 15}}>
        <Bot scale={1.5} delay={4} mood={mood} text={frame < 60 ? 'all this?' : frame < 110 ? 'yep!' : '♥'} typeFrom={frame < 60 ? 10 : frame < 110 ? 60 : 110} leds={C.screenPink} look={[Math.sin(frame * 0.05) * 0.8, 0]} />
      </div>
      <Kicker size={27} delay={120} style={{position: 'absolute', right: 110, bottom: 60, marginBottom: 0}}>all real, all on the desk today.</Kicker>
    </SceneFrame>
  );
}

// ---------- 10 · outro ----------

function MiniDevice({kind, at}: {kind: 'desk' | 'phone' | 'mac'; at: number}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, at, SPRINGS.pop, 22);
  const label = {desk: 'desk', phone: 'phone', mac: 'Mac'}[kind];
  const color = {desk: C.sun, phone: C.teal, mac: C.pinkSoft}[kind];
  return (
    <div style={{display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 8, opacity: Math.min(1, a * 1.4), transform: `scale(${0.4 + 0.6 * a}) translateY(${wave(frame, 4, 0.07, at)}px)`}}>
      <div style={{width: 84, height: 84, borderRadius: 18, background: color, border: `3px solid ${C.ink}`, boxShadow: `4px 4px 0 ${C.ink}`, display: 'flex', alignItems: 'center', justifyContent: 'center'}}>
        {kind === 'desk' && <Avatar size={50} />}
        {kind === 'phone' && <div style={{width: 30, height: 52, borderRadius: 8, border: `3px solid ${C.ink}`, background: C.sheet}} />}
        {kind === 'mac' && <div style={{display: 'flex', flexDirection: 'column', alignItems: 'center'}}><div style={{width: 52, height: 34, borderRadius: 4, border: `3px solid ${C.ink}`, background: C.screen}} /><div style={{width: 64, height: 7, borderRadius: 3, background: C.ink, marginTop: 2}} /></div>}
      </div>
      <span style={{font: '700 20px Andika', color: C.ink}}>{label}</span>
    </div>
  );
}

function OutroScene() {
  const {frame, fps} = useT();
  const b = fade(frame, 70, 90);
  const link = prog(frame, 70, 96, Easing.inOut(Easing.quad));
  const mood: Mood = frame >= 132 && frame < 150 ? 'wink' : 'happy';
  return (
    <SceneFrame accent={C.sky} duration={180}>
      <Confetti start={6} />
      <div style={{position: 'absolute', left: 180, right: 150, top: 170, height: 640, display: 'flex', alignItems: 'center', justifyContent: 'space-between'}}>
        <div style={{width: 1000}}>
          <div style={{font: '900 180px/0.9 Grandstander', letterSpacing: '-0.06em', color: C.ink, display: 'flex'}}>
            {'eve'.split('').map((ch, i) => {
              const a = sp(frame, fps, 6 + i * 4, SPRINGS.pop, 30);
              return <span key={i} style={{display: 'inline-block', opacity: Math.min(1, a * 1.4), transform: `translateY(${(1 - a) * 120}px) rotate(${(1 - a) * -12}deg) scale(${0.6 + 0.4 * a})`, textShadow: `${9 * a}px ${9 * a}px 0 ${C.pinkSoft}`}}>{ch}</span>;
            })}
          </div>
          <div style={{font: '700 46px Grandstander', color: C.ink, marginTop: 20}}><Words text="On your desk. In your pocket." delay={28} step={4} shadow={4} /></div>
          <div style={{marginTop: 40, display: 'flex', alignItems: 'center', gap: 0, position: 'relative'}}>
            <MiniDevice kind="phone" at={56} />
            <svg width="120" height="20" viewBox="0 0 120 20"><path d="M6 10H114" stroke={C.ink} strokeWidth="4" strokeLinecap="round" strokeDasharray="4 12" strokeDashoffset={-frame * 1.5} opacity={link} /></svg>
            <MiniDevice kind="desk" at={62} />
            <svg width="120" height="20" viewBox="0 0 120 20"><path d="M6 10H114" stroke={C.ink} strokeWidth="4" strokeLinecap="round" strokeDasharray="4 12" strokeDashoffset={frame * 1.5} opacity={link} /></svg>
            <MiniDevice kind="mac" at={68} />
            {frame >= 96 && [0, 1].map((k) => {
              const t = ((frame - 96) % 40) / 40;
              const x = k === 0 ? 92 + t * 116 : 420 - t * 116;
              return <div key={k} style={{position: 'absolute', left: x, top: 42, width: 14, height: 14, borderRadius: '50%', background: C.pink, border: `2px solid ${C.ink}`, opacity: Math.sin(t * Math.PI)}} />;
            })}
          </div>
        </div>
        <div style={{width: 420, display: 'flex', justifyContent: 'center'}}><Bot scale={1.6} delay={16} text="see you!" typeFrom={40} mood={mood} chirpAt={34} leds={C.teal} /></div>
      </div>
      <div style={{position: 'absolute', left: 220, bottom: 90, font: '700 24px Andika', color: C.inkSoft, opacity: b, transform: `translateY(${(1 - b) * 14}px)`}}>open source · github.com/gurul/eve</div>
      <Kicker size={32} delay={80} style={{position: 'absolute', right: 180, bottom: 84, marginBottom: 0}}>your desktop sidekick.</Kicker>
    </SceneFrame>
  );
}

// ---------- assembly ----------

const SCENE_LIST: {name: string; duration: number; accent: string; el: React.ReactNode}[] = [
  {name: 'hello', duration: 180, accent: C.pink, el: <HelloScene />},
  {name: 'senses', duration: 330, accent: C.sun, el: <SensesScene />},
  {name: 'life', duration: 300, accent: C.sky, el: <LifeScene />},
  {name: 'pocket', duration: 270, accent: C.teal, el: <PocketScene />},
  {name: 'chrome', duration: 420, accent: C.sun, el: <ChromeScene />},
  {name: 'routes', duration: 300, accent: C.pink, el: <RoutesScene />},
  {name: 'coding', duration: 360, accent: C.pink, el: <CodingScene />},
  {name: 'brain', duration: 270, accent: C.teal, el: <BrainScene />},
  {name: 'orbit', duration: 180, accent: C.sky, el: <OrbitScene />},
  {name: 'outro', duration: 180, accent: C.sun, el: <OutroScene />},
];

export const POCKET_SCENES = SCENE_LIST.reduce<{name: string; from: number; duration: number; accent: string; el: React.ReactNode}[]>((acc, s) => {
  const from = acc.length ? acc[acc.length - 1].from + acc[acc.length - 1].duration : 0;
  return [...acc, {...s, from}];
}, []);

export const POCKET_FRAMES = POCKET_SCENES.reduce((n, s) => n + s.duration, 0);

// Robot sounds on the moments things land (scripts/make-chirps.py).
const SFX: [string, number, 'wake' | 'pop' | 'happy' | 'ok', number][] = [
  ['hello', 56, 'wake', 0.5],
  ['senses', 30, 'wake', 0.4], ['senses', 214, 'pop', 0.5], ['senses', 278, 'happy', 0.45],
  ['life', 78, 'pop', 0.5], ['life', 110, 'pop', 0.35],
  ['pocket', 76, 'pop', 0.35], ['pocket', 128, 'happy', 0.35],
  ['chrome', 112, 'pop', 0.35], ['chrome', 136, 'ok', 0.5], ['chrome', 334, 'pop', 0.45],
  ['routes', 84, 'pop', 0.45], ['routes', 158, 'pop', 0.45], ['routes', 232, 'ok', 0.45],
  ['coding', 158, 'pop', 0.45], ['coding', 272, 'pop', 0.35], ['coding', 312, 'happy', 0.45],
  ['brain', 66, 'pop', 0.45], ['brain', 136, 'pop', 0.45], ['brain', 206, 'ok', 0.45],
  ['outro', 34, 'wake', 0.5], ['outro', 132, 'happy', 0.4],
];

export function PocketLaunch() {
  const frame = useCurrentFrame();
  const volume = interpolate(frame, [0, 36, POCKET_FRAMES - 90, POCKET_FRAMES], [0, 0.72, 0.72, 0], {extrapolateLeft: 'clamp', extrapolateRight: 'clamp'});
  return (
    <AbsoluteFill style={{background: C.paper}}>
      <style dangerouslySetInnerHTML={{__html: fontFace}} />
      <Audio src={staticFile('audio/eve-launch.mp3')} volume={volume} />
      {SFX.map(([scene, at, kind, vol]) => {
        const s = POCKET_SCENES.find((x) => x.name === scene)!;
        return <Sequence key={`${scene}-${at}`} from={s.from + at} durationInFrames={30}><Audio src={staticFile(`audio/sfx-${kind}.wav`)} volume={vol} /></Sequence>;
      })}
      {POCKET_SCENES.map((s) => (
        <Sequence key={s.name} from={s.from} durationInFrames={s.duration} name={s.name}><AbsoluteFill>{s.el}</AbsoluteFill></Sequence>
      ))}
      {POCKET_SCENES.slice(1).map((s) => (
        <Sequence key={`wipe-${s.name}`} from={s.from - 16} durationInFrames={32}><Wipe color={s.accent} /></Sequence>
      ))}
    </AbsoluteFill>
  );
}
