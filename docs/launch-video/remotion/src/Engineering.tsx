import React from 'react';
import {AbsoluteFill, Easing, Sequence, interpolate, useCurrentFrame} from 'remotion';
import {Arrow, C, CSS, Card, H1, Kicker, Pill, SPRINGS, SceneFrame, Tag, Wipe, bump, fade, fontFace, sp, useT, Robot} from './Root';
import script from './engineering/script.json';
import timeline from './engineering/timeline.json';

/*
 * eve, engineered — a technical breakdown for engineers. 1920×1080, 30 fps.
 *
 * Not the launch film: this one explains how eve is built and why. The narration is
 * authored in engineering/script.json (one entry per caption). The timeline generator
 * (docs/engineering-video/make-timeline.mjs) turns it into engineering/timeline.json and the
 * .srt subtitle track, so the burned-in captions, the scene lengths and the subtitles all
 * come from one list. Every visual beat is keyed to a caption index (`cue(i)`), not to a
 * hard-coded frame, so editing the narration re-times the film.
 *
 * Facts and their sources are in docs/engineering-video/SCRIPT.md.
 * Reuses Root.tsx's motion system: SceneFrame (paper background, camera push), Wipe,
 * H1/Kicker type, Card, Pill, Tag, Arrow, springs, and the Robot.
 */

export const ENG_FPS = timeline.fps;
export const ENG_FRAMES = timeline.totalFrames;

const MONO = "Menlo, 'SF Mono', monospace";
const ACCENTS = [C.pink, C.sky, C.teal, C.sun, C.purple, C.teal, C.sun, C.pink, C.sky, C.pink];

type SectionT = (typeof timeline.sections)[number];

// ---------- timing ----------

const SectionCtx = React.createContext<SectionT | null>(null);

// Local frame at which caption `i` of the current section starts.
function useCue() {
  const s = React.useContext(SectionCtx);
  return (i: number) => {
    if (!s) return 0;
    const c = s.cues[Math.min(i, s.cues.length - 1)];
    return i >= s.cues.length ? s.duration : c.from;
  };
}

// Index of the caption on screen (the last one started), -1 before the first.
function useCueIndex() {
  const s = React.useContext(SectionCtx);
  const frame = useCurrentFrame();
  if (!s) return -1;
  let idx = -1;
  s.cues.forEach((c, i) => {
    if (frame >= c.from) idx = i;
  });
  return idx;
}

// ---------- building blocks ----------

// Shows its children from caption `from` until caption `to` (exclusive), with a soft fade.
function Beat({from, to, children, style}: {from: number; to?: number; children: React.ReactNode; style?: CSS}) {
  const frame = useCurrentFrame();
  const cue = useCue();
  const a = cue(from);
  const b = to === undefined ? Infinity : cue(to);
  if (frame < a - 2 || frame > b + 10) return null;
  const inO = fade(frame, a - 2, a + 10);
  const outO = b === Infinity ? 1 : 1 - fade(frame, b - 4, b + 8);
  return <div style={{position: 'absolute', opacity: Math.min(inO, outO), transform: `translateY(${(1 - inO) * 16}px)`, ...style}}>{children}</div>;
}

function Block({title, sub, color = C.sky, at = 0, lit = false, w, style, mono = false}: {title: React.ReactNode; sub?: React.ReactNode; color?: string; at?: number; lit?: boolean; w?: number; style?: CSS; mono?: boolean}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, at, SPRINGS.pop, 22);
  const b = bump(frame, at + 8, 16);
  return (
    <div style={{position: 'absolute', width: w, background: lit ? C.sheet : C.sheet, border: `3px solid ${C.ink}`, borderRadius: 10, boxShadow: `${(lit ? 10 : 6) * a}px ${(lit ? 10 : 6) * a}px 0 ${lit ? color : C.desk}`, padding: '14px 18px', opacity: Math.min(1, a * 1.4), transform: `scale(${(0.7 + 0.3 * a) * (1 + b * 0.04) * (lit ? 1.03 : 1)})`, transformOrigin: '50% 50%', ...style}}>
      <div style={{font: `700 ${mono ? 22 : 25}px/1.1 ${mono ? MONO : 'Grandstander'}`, color: C.ink}}>{title}</div>
      {sub && <div style={{font: '400 19px/1.25 Andika', color: C.inkSoft, marginTop: 7}}>{sub}</div>}
    </div>
  );
}

// A code excerpt, short and large enough to read at 1080p.
function Code({lines, at = 0, title, w = 820, size = 22, style}: {lines: string[]; at?: number; title?: string; w?: number; size?: number; style?: CSS}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, at, SPRINGS.rise, 26);
  return (
    <div style={{position: 'absolute', width: w, background: C.screen, border: `3px solid ${C.ink}`, borderRadius: 10, boxShadow: `${8 * a}px ${8 * a}px 0 ${C.purple}`, opacity: a, transform: `translateY(${(1 - a) * 30}px)`, overflow: 'hidden', ...style}}>
      {title && <div style={{font: `700 16px ${MONO}`, color: '#9DA6D8', padding: '10px 18px', borderBottom: '2px solid #2C3670', letterSpacing: '0.04em'}}>{title}</div>}
      <pre style={{margin: 0, padding: '14px 18px', font: `400 ${size}px/1.45 ${MONO}`, color: '#EEF0FF', whiteSpace: 'pre'}}>
        {lines.map((l, i) => {
          const shown = fade(frame, at + 4 + i * 2, at + 10 + i * 2);
          return <div key={i} style={{opacity: shown}}>{paint(l)}</div>;
        })}
      </pre>
    </div>
  );
}

// Minimal highlighting: comments dim, strings warm, a few keywords pink.
function paint(line: string): React.ReactNode {
  const commentAt = line.search(/(#|--) /);
  const body = commentAt >= 0 ? line.slice(0, commentAt) : line;
  const comment = commentAt >= 0 ? line.slice(commentAt) : '';
  const parts = body.split(/("[^"]*"|'[^']*'|\b(?:def|class|async|await|return|raise|if|for|in|or|and|not|continue|theorem|let|by|with|try|except)\b)/g);
  return (
    <>
      {parts.map((p, i) => {
        if (!p) return null;
        if (/^["']/.test(p)) return <span key={i} style={{color: C.sun}}>{p}</span>;
        if (/^(def|class|async|await|return|raise|if|for|in|or|and|not|continue|theorem|let|by|with|try|except)$/.test(p)) return <span key={i} style={{color: C.screenPink}}>{p}</span>;
        return <span key={i}>{p}</span>;
      })}
      {comment && <span style={{color: '#8A93C4'}}>{comment}</span>}
      {line === '' ? ' ' : null}
    </>
  );
}

function Stat({n, label, at = 0, color = C.sun, style}: {n: string; label: string; at?: number; color?: string; style?: CSS}) {
  const {frame, fps} = useT();
  const a = sp(frame, fps, at, SPRINGS.pop, 24);
  return (
    <div style={{position: 'absolute', textAlign: 'center', opacity: Math.min(1, a * 1.4), transform: `scale(${0.6 + 0.4 * a})`, ...style}}>
      <div style={{font: '900 84px/1 Grandstander', color: C.ink, textShadow: `5px 5px 0 ${color}`}}>{n}</div>
      <div style={{font: '700 22px/1.2 Andika', color: C.inkSoft, marginTop: 8}}>{label}</div>
    </div>
  );
}

// A straight connector drawn on over 14 frames, with an arrow head and an optional label.
function Link({x1, y1, x2, y2, at = 0, color = C.ink, label, dashed = false, labelDy = -12, mono = true, size = 18}: {x1: number; y1: number; x2: number; y2: number; at?: number; color?: string; label?: string; dashed?: boolean; labelDy?: number; mono?: boolean; size?: number}) {
  const frame = useCurrentFrame();
  const p = fade(frame, at, at + 14, Easing.out(Easing.quad));
  if (frame < at) return null;
  const ang = Math.atan2(y2 - y1, x2 - x1);
  const hx = x1 + (x2 - x1) * p;
  const hy = y1 + (y2 - y1) * p;
  const h = (d: number) => `${hx - 14 * Math.cos(ang + d)},${hy - 14 * Math.sin(ang + d)}`;
  return (
    <>
      <svg width={1920} height={1080} style={{position: 'absolute', left: 0, top: 0, overflow: 'visible', pointerEvents: 'none'}}>
        <line x1={x1} y1={y1} x2={hx} y2={hy} stroke={color} strokeWidth={3.5} strokeLinecap="round" strokeDasharray={dashed ? '8 9' : undefined} />
        {p > 0.2 && <polyline points={`${h(0.45)} ${hx},${hy} ${h(-0.45)}`} fill="none" stroke={color} strokeWidth={3.5} strokeLinecap="round" strokeLinejoin="round" />}
      </svg>
      {label && (
        <div style={{position: 'absolute', left: Math.min(x1, x2), width: Math.max(80, Math.abs(x2 - x1)), top: (y1 + y2) / 2 + labelDy - size, textAlign: 'center', font: `400 ${size}px/1.2 ${mono ? MONO : 'Andika'}`, color: C.ink, opacity: fade(frame, at + 6, at + 16), whiteSpace: 'nowrap'}}>
          {label}
        </div>
      )}
    </>
  );
}

function Chip({children, color = C.sheet, at = 0, style, size = 20}: {children: React.ReactNode; color?: string; at?: number; style?: CSS; size?: number}) {
  return (
    <div style={{position: 'absolute', ...style}}>
      <Pill color={color} size={size} delay={at}>{children}</Pill>
    </div>
  );
}

function Header({kicker, title}: {kicker: string; title: string}) {
  return (
    <div style={{position: 'absolute', left: 100, top: 62}}>
      <Kicker size={30} delay={2}>{kicker}</Kicker>
      <H1 text={title} size={54} delay={6} />
    </div>
  );
}

// ---------- 1. what it is ----------

function WhatScene() {
  const cue = useCue();
  const i = useCueIndex();
  const {frame} = useT();
  return (
    <>
      <div style={{position: 'absolute', left: 150, top: 290, transform: 'scale(1.25)', transformOrigin: '0 0'}}>
        <Robot message="hey!" delay={cue(1)} />
      </div>
      <Chip at={cue(3)} color={C.pinkSoft} style={{left: 150, top: 700}} size={19}>USB serial · NDJSON · 115,200 baud</Chip>
      <Link x1={500} y1={560} x2={700} y2={560} at={cue(3)} label="one JSON object per line" mono={false} />
      <Block at={cue(2)} lit={i === 2} color={C.pink} w={440} style={{left: 720, top: 250}} title="StackChan K151" sub="M5Stack CoreS3 · ESP32-S3 · custom Arduino firmware" />
      <Block at={cue(3)} lit={i === 3 || i === 4} color={C.sky} w={440} style={{left: 720, top: 420}} title="Python daemon on the Mac" sub="one line of JSON per message, both directions" />
      <Beat from={4} style={{left: 720, top: 590}}>
        <div style={{display: 'flex', flexDirection: 'column', gap: 12}}>
          <Pill color={C.sheet} size={18} delay={cue(4)}>firmware prints [alive] every 5 s</Pill>
          <Pill color={C.sheet} size={18} delay={cue(4) + 8}>20 s of silence = link dead</Pill>
          <Pill color={C.sheet} size={18} delay={cue(4) + 16}>3 silent opens in 10 min → pulse RTS, reset board</Pill>
        </div>
      </Beat>
      <div style={{position: 'absolute', left: 1230, top: 250, font: '400 26px Gochi Hand', color: C.inkSoft, opacity: fade(frame, cue(5), cue(5) + 12)}}>the doors</div>
      <Block at={cue(5)} lit={i === 5} color={C.teal} w={560} style={{left: 1230, top: 300}} title="Home Assistant Voice PE" sub="reflashed · hold to talk · light ring shows state" />
      <Block at={cue(6)} lit={i === 6} color={C.sky} w={560} style={{left: 1230, top: 430}} title="Telegram" sub="long-poll over outbound HTTPS · no open port" />
      <Block at={cue(7)} lit={i === 7} color={C.sun} w={560} style={{left: 1230, top: 560}} title="Mini App" sub="builds small apps · push-to-talk calls" />
      <Block at={cue(7) + 12} lit={i === 7} color={C.purple} w={560} style={{left: 1230, top: 690}} title="Mac menu bar + widget" sub="SwiftUI · WidgetKit · the diary" />
    </>
  );
}

// ---------- 2. the hub and the contract ----------

function HubScene() {
  const cue = useCue();
  const i = useCueIndex();
  const {frame} = useT();
  const b = fade(frame, cue(5) - 12, cue(5) + 4);
  const doors = ['robot link (serial)', 'Voice PE controller', 'Telegram', 'Mini App + calls', 'wake word (Mac mic)'];
  const lanes = ['Firecrawl reader', 'Chrome lane', 'Codex', 'Holo'];
  const tasks = ['ipc', 'serial link', 'controller', 'heartbeat', 'telegram', 'watch', 'miniapp', 'spend-sync', 'memory-dream'];
  const stops = ['camera, explore stop', 'lanes close', 'tasks cancelled + gathered', 'conversations close (3 s)', 'dream drained (≤ 12 s)', 'link + IPC stop'];
  return (
    <>
      <div style={{position: 'absolute', inset: 0, opacity: 1 - b}}>
        {doors.map((d, k) => (
          <Block key={d} at={4 + k * 4} w={330} style={{left: 110, top: 250 + k * 92}} title={d} color={C.pink} />
        ))}
        <Block at={cue(0)} lit={i <= 2} color={C.sky} w={420} style={{left: 750, top: 380}} title="the daemon" sub="one asyncio loop · every door · every lane" />
        {doors.map((d, k) => (
          <Link key={d} x1={450} y1={285 + k * 92} x2={740} y2={430} at={cue(0) + 6 + k * 2} color={C.inkSoft} />
        ))}
        {lanes.map((l, k) => (
          <React.Fragment key={l}>
            <Block at={cue(0) + 14 + k * 4} w={330} style={{left: 1480, top: 290 + k * 100}} title={l} color={C.teal} />
            <Link x1={1180} y1={430} x2={1470} y2={325 + k * 100} at={cue(0) + 18 + k * 2} color={C.inkSoft} />
          </React.Fragment>
        ))}
        <Beat from={1} to={3} style={{left: 110, top: 740, width: 1700}}>
          <div style={{display: 'flex', flexWrap: 'wrap', gap: 12}}>
            {tasks.map((t, k) => <Pill key={t} color={C.sheet} size={19} delay={cue(1) + k * 3}><span style={{fontFamily: MONO}}>{t}</span></Pill>)}
          </div>
        </Beat>
        <Chip at={cue(2)} color={C.sun} style={{left: 750, top: 560}} size={19}>blocking I/O → worker threads</Chip>
        <Beat from={3} to={5} style={{left: 110, top: 740, width: 1720}}>
          <div style={{display: 'flex', gap: 10, alignItems: 'center'}}>
            {stops.map((s, k) => (
              <React.Fragment key={s}>
                <Pill color={k === 4 ? C.sun : C.sheet} lit={k === 4 && i === 4} size={17} delay={cue(3) + k * 5}>{`${k + 1}. ${s}`}</Pill>
              </React.Fragment>
            ))}
          </div>
        </Beat>
      </div>
      <div style={{position: 'absolute', inset: 0, opacity: b}}>
        {frame > cue(5) - 12 && (
          <>
            <Code at={cue(5)} w={760} title="the agent contract (duck-typed)" style={{left: 110, top: 250}} lines={[
              'async def run(self, goal: str) -> str',
              'def steer(self, text: str) -> bool',
              "def cancel(self, reason: str = '') -> None",
              'def status(self) -> dict[str, Any]',
            ]} />
            <Code at={cue(7)} w={800} title="agent_contract.py" style={{left: 960, top: 250}} lines={[
              '@dataclass(frozen=True)',
              'class AgentEvent:',
              '    """kind: started | turn |',
              '    commentary | exec | progress | ask |',
              '    final | cancelled | error."""',
              '    kind: str',
              '    text: str = ""',
              '    turn: int = 0',
            ]} />
            <Beat from={6} to={7} style={{left: 110, top: 520, width: 760}}>
              <div style={{font: '400 24px/1.5 Andika', color: C.ink}}>
                <b>run</b> a goal → the answer<br />
                <b>steer</b> a running task with a correction<br />
                <b>cancel</b> it · <b>status</b> says where it is
              </div>
            </Beat>
            <Beat from={8} style={{left: 110, top: 540}}><Tag color={C.pinkSoft} delay={cue(8)}>no Protocol class: shared method names</Tag></Beat>
            <Beat from={8} style={{left: 110, top: 610, font: '400 22px Andika', color: C.inkSoft}}>read by: the voice session · Telegram · the robot's screen</Beat>
            <Beat from={9} style={{left: 110, top: 700, width: 1700}}>
              <div style={{display: 'flex', gap: 14, flexWrap: 'wrap'}}>
                {['WebReaderAgent', 'ChromeLaneAgent', 'CodexComputerAgent', 'HoloComputerAgent', 'ReflexFirstAgent (wrapper)'].map((n, k) => (
                  <Pill key={n} color={k === 4 ? C.sun : C.teal} size={20} delay={cue(9) + k * 5}><span style={{fontFamily: MONO}}>{n}</span></Pill>
                ))}
              </div>
            </Beat>
          </>
        )}
      </div>
    </>
  );
}

// ---------- 3. the request path ----------

function PathScene() {
  const cue = useCue();
  const i = useCueIndex();
  const reflexLit = i >= 2 && i <= 4;
  const routerLit = i >= 5 && i <= 7;
  return (
    <>
      <Block at={cue(0)} w={200} style={{left: 100, top: 360}} title="request" sub="voice · text · call" color={C.pink} lit={i <= 1} />
      <Link x1={305} y1={400} x2={375} y2={400} at={cue(0) + 8} />
      <Block at={cue(0) + 10} w={330} lit={reflexLit} color={C.sun} style={{left: 385, top: 330}} title="1 · reflex" sub="rules in code · Jev for unknown wording · installed apps only" />
      <Link x1={720} y1={400} x2={790} y2={400} at={cue(0) + 14} />
      <Block at={cue(0) + 16} w={350} lit={routerLit} color={C.sky} style={{left: 800, top: 330}} title="2 · body router" sub="Jev fills a typed form · code applies the gates" />
      <Link x1={1155} y1={380} x2={1330} y2={290} at={cue(0) + 20} />
      <Link x1={1155} y1={410} x2={1330} y2={430} at={cue(0) + 22} />
      <Link x1={1155} y1={440} x2={1330} y2={575} at={cue(0) + 24} />
      <Block at={cue(0) + 22} w={470} lit={i === 6 || i === 7} color={C.teal} style={{left: 1340, top: 240}} title="Firecrawl reader" sub="public reading job · ≤ 5 tasks a day" />
      <Block at={cue(0) + 24} w={470} lit={i === 8} color={C.pink} style={{left: 1340, top: 385}} title="Chrome lane" sub="eve's own Chrome · Playwright · its own tab" />
      <Block at={cue(0) + 26} w={470} lit={i === 9} color={C.purple} style={{left: 1340, top: 530}} title="3 · desktop floor" sub="Codex computer use, or Holo (live on the owner's Mac)" />
      <Link x1={550} y1={330} x2={550} y2={275} at={cue(2)} color={C.sun} />
      <Chip at={cue(2)} color={C.sun} style={{left: 385, top: 225}} size={17}>skip on: send · buy · pay · delete …</Chip>

      <div style={{position: 'absolute', left: 100, top: 690, width: 1720, height: 170}}>
        <Beat from={0} to={1} style={{left: 0, top: 10, font: '400 30px/1.3 Andika', color: C.ink, width: 1600}}>
          Each tier <b>declines</b> unless it is sure, and the next tier takes over.
        </Beat>
        <Beat from={1} to={2} style={{left: 0, top: 0}}>
          <div style={{display: 'flex', gap: 60, alignItems: 'center'}}>
            <div style={{font: '900 70px Grandstander', color: C.ink, textShadow: `4px 4px 0 ${C.sun}`}}>¼</div>
            <div style={{font: '400 28px/1.3 Andika'}}>of requests are app launches<br />8–32 s each through the planner</div>
          </div>
        </Beat>
        <Beat from={2} to={3}><Code at={cue(2)} w={900} lines={['"open Spotify"  ->  open -a Spotify      # no model']} /></Beat>
        <Beat from={3} to={4}><Code at={cue(3)} w={900} title="task_router.py" lines={['# Jev may name an app; code decides if it counts', 'if app in installed: ...']} /></Beat>
        <Beat from={4} to={5}><Code at={cue(4)} w={1100} lines={['CONSEQUENTIAL: send | order | buy | pay | delete | password ...', '  -> planner only (only the planner has ask_user)']} /></Beat>
        <Beat from={5} to={6} style={{left: 0, top: 0}}>
          <div style={{display: 'flex', gap: 12, flexWrap: 'wrap', width: 1700}}>
            {['public_read_job', 'needs_accounts', 'needs_mac', 'needs_interaction', 'show_owner', 'p(auto)'].map((q, k) => (
              <Pill key={q} color={C.sheet} size={21} delay={cue(5) + k * 4}><span style={{fontFamily: MONO}}>{q}</span></Pill>
            ))}
          </div>
        </Beat>
        <Beat from={6} to={7}><Code at={cue(6)} w={1150} size={20} title="browser_router.py · decide()" lines={[
          'if a.error: return CODEX',
          'if a.accounts > g.accounts_max or a.mac > g.mac_max ...: return CODEX',
          'if a.public >= g.public and a.p_auto >= g.auto: return FIRECRAWL',
          'return CODEX',
        ]} /></Beat>
        <Beat from={7} to={8} style={{left: 0, top: 0, width: 1700, height: 170}}>
          <Stat n="30" label="routed on a blind holdout" at={cue(7)} style={{left: 0, top: 0}} />
          <Stat n="30" label="right" at={cue(7) + 6} color={C.teal} style={{left: 380, top: 0}} />
          <Stat n="0" label="unsafe" at={cue(7) + 12} color={C.pink} style={{left: 640, top: 0}} />
        </Beat>
        <Beat from={8} to={9} style={{left: 0, top: 10, font: '400 28px/1.35 Andika', color: C.ink, width: 1650}}>
          A code rule (not Jev) picks Chrome or the desktop. The lane refuses a <b>sensitive control</b> without the owner's approval, and never closes the owner's tabs.
        </Beat>
        <Beat from={9} to={10}><Code at={cue(9)} w={900} lines={['EVE_COMPUTER=codex   # default', 'EVE_COMPUTER=holo    # the owner\'s Mac today']} /></Beat>
        <Beat from={10} style={{left: 0, top: 10}}>
          <div style={{font: '900 50px Grandstander', color: C.ink}}>the model proposes · <span style={{textShadow: `4px 4px 0 ${C.teal}`}}>code decides</span></div>
        </Beat>
      </div>
    </>
  );
}

// ---------- 4. the Holo driver ----------

function HoloScene() {
  const cue = useCue();
  const i = useCueIndex();
  const {frame} = useT();
  const X = [370, 960, 1550];
  const top = 250;
  const msg = (k: number, y: number, from: number, to: number, label: string, color = C.ink, dashed = false) => (
    <Link key={`${label}-${y}`} x1={X[from] + (to > from ? 12 : -12)} y1={y} x2={X[to] + (to > from ? -12 : 12)} y2={y} at={cue(k)} color={color} label={label} dashed={dashed} size={17} labelDy={-6} />
  );
  return (
    <>
      <Block at={cue(1)} w={390} lit={i === 1} color={C.pink} style={{left: X[0] - 195, top}} title="eve daemon" sub="HoloComputerAgent · eve's venv" />
      <Block at={cue(2)} w={390} lit={i === 2 || i === 3} color={C.sun} style={{left: X[1] - 195, top}} title="holo_driver.py" sub="runs under holo's own Python" />
      <Block at={cue(4)} w={390} lit={i === 7 || i === 8} color={C.teal} style={{left: X[2] - 195, top}} title="holo agent-api" sub="127.0.0.1 · kept warm · holo4-27b" />
      <svg width={1920} height={1080} style={{position: 'absolute', left: 0, top: 0}}>
        {X.map((x, k) => {
          const at = [cue(1), cue(2), cue(4)][k];
          const p = fade(frame, at + 6, at + 30);
          return <line key={x} x1={x} y1={top + 100} x2={x} y2={top + 100 + 500 * p} stroke={C.inkSoft} strokeWidth={3} strokeDasharray="6 10" />;
        })}
      </svg>
      <Chip at={cue(1) + 10} color={C.pinkSoft} style={{left: 100, top: 180}} size={17}>holo's 15 dependencies stay out of eve</Chip>
      {msg(2, 380, 0, 1, 'spawn: holo python holo_driver.py --port 18795')}
      {msg(3, 425, 0, 1, '{"op": "run", "goal": …}', C.pink)}
      {msg(3, 470, 1, 0, 'ready · session · progress', C.inkSoft)}
      {msg(4, 450, 1, 2, 'session_runner.run_turn (the vendor\'s own)', C.teal)}
      {msg(5, 520, 0, 1, '{"op": "steer", "text": …}', C.pink)}
      {msg(5, 545, 1, 2, 'send_message · queued until running · 409 → retry ×4', C.teal)}
      {msg(5, 590, 1, 0, 'steered | refused', C.inkSoft)}
      {msg(6, 640, 0, 1, '{"op": "cancel"} or stdin closed', C.pink)}
      {msg(6, 665, 1, 2, 'request_stop · pause · cancel', C.teal)}
      {msg(6, 710, 1, 0, 'stopping … final', C.inkSoft)}
      <Beat from={7} to={9} style={{left: X[2] - 195, top: 760, width: 440}}>
        <div style={{display: 'flex', flexDirection: 'column', gap: 10}}>
          <Pill color={C.teal} size={17} delay={cue(7)}>spawned once, by the first task</Pill>
          <Pill color={C.sheet} size={17} delay={cue(7) + 6}>random token · waits for GET /health</Pill>
          <Pill color={C.sheet} size={17} delay={cue(8)}>daemon owns it: it exits with its parent</Pill>
        </div>
      </Beat>
      <Beat from={9} style={{left: 100, top: 770}}>
        <Code at={cue(9)} w={1000} size={20} lines={['{"ev": "progress", "text": "Opening Calculator"}', '# a note or tool names, never the arguments:', '# a typed password stays off the board and the chat']} />
      </Beat>
      <Beat from={0} to={2} style={{left: 100, top: 770, font: '900 44px Grandstander', color: C.ink}}>
        wrap the vendor's client · <span style={{textShadow: `4px 4px 0 ${C.sun}`}}>don't reimplement it</span>
      </Beat>
    </>
  );
}

// ---------- 5. the chat brain ----------

function BrainScene() {
  const cue = useCue();
  const i = useCueIndex();
  const inputs = [['Telegram text', C.sky], ['Mini App call', C.sun], ['Voice PE button', C.teal]] as const;
  const tools = ['start_task', 'steer_task', 'stop_task', 'take_photo', 'screenshot', 'move_head', 'memory_search', 'start_coding_session', 'web_search'];
  return (
    <>
      {inputs.map(([t, c], k) => (
        <React.Fragment key={t}>
          <Block at={cue(0) + k * 6} w={270} color={c} style={{left: 100, top: 260 + k * 110}} title={t} sub={k === 0 ? 'typed' : 'speech → transcribed'} />
          <Link x1={380} y1={300 + k * 110} x2={470} y2={390} at={cue(0) + 10 + k * 4} color={C.inkSoft} />
        </React.Fragment>
      ))}
      <Block at={cue(0) + 16} w={380} lit={i === 1} color={C.purple} style={{left: 480, top: 330}} title="chat brain" sub="Responses API · store off · ≤ 6 tool rounds" />
      <Beat from={2} style={{left: 100, top: 640, width: 800}}>
        <div style={{display: 'flex', gap: 10, flexWrap: 'wrap'}}>
          {tools.map((t, k) => <Pill key={t} color={C.sheet} size={17} delay={cue(2) + k * 3}><span style={{fontFamily: MONO}}>{t}</span></Pill>)}
        </div>
      </Beat>
      <div style={{position: 'absolute', left: 950, top: 250, width: 880, height: 600}}>
        <Beat from={3} to={4}>
          <Code at={cue(3)} w={860} size={20} title="telegram.py · accept()" lines={[
            'if user_id not in config.owner_ids:',
            '    return "stranger", None',
            'if sender.get("is_bot"):',
            '    return "bot", None',
            'if chat.get("type") != "private" or chat_id != user_id:',
            '    return "not-private", None',
          ]} />
        </Beat>
        <Beat from={4} to={5}>
          <div style={{display: 'grid', gridTemplateColumns: '260px 260px', gap: 16}}>
            {[['Gmail', 'read', C.sky, 'writes refused'], ['Calendar', 'write', C.teal, 'runs'], ['Drive', 'ask', C.sun, 'writes ask the owner']].map(([n, p, c, d], k) => (
              <React.Fragment key={n}>
                <Pill color={C.sheet} size={24} delay={cue(4) + k * 6}>{n}</Pill>
                <Pill color={c} size={24} delay={cue(4) + k * 6 + 3}>{`${p} · ${d}`}</Pill>
              </React.Fragment>
            ))}
          </div>
        </Beat>
        <Beat from={5} to={6}>
          <Code at={cue(5)} w={860} size={19} title="telegram.py · the consent gate" lines={[
            'decision = composio_tools.decide(name, args, policy)',
            'if decision.action == "refuse":',
            '    return {"ok": False, "reason": decision.why}',
            'if decision.action == "ask":',
            '    answer = await self._ask_user(question, ...)',
            '    if not consent.approves(answer):',
            '        return {"ok": False, "reason": "the owner said no"}',
          ]} />
        </Beat>
        <Beat from={6}>
          <div style={{display: 'flex', flexDirection: 'column', gap: 14}}>
            <Pill color={C.sheet} size={22} delay={cue(6)}>server binds 127.0.0.1 · Cloudflare quick tunnel</Pill>
            <Pill color={C.sun} size={22} delay={cue(6) + 6}>every call: Telegram initData, HMAC-checked</Pill>
            <Pill color={C.sheet} size={22} delay={cue(6) + 12}>age checked · user must be an owner</Pill>
          </div>
        </Beat>
      </div>
    </>
  );
}

// ---------- 6. memory ----------

function MemoryScene() {
  const cue = useCue();
  const i = useCueIndex();
  const stores = [
    ['transcripts', 'the source', 'every word, written as it is said', C.sky, 1],
    ['records', 'the truth', 'markdown in a local git repo · brains read, never write', C.sun, 2],
    ['mem0', 'the index', 'meaning search · local Qdrant · rebuilt from transcripts', C.teal, 3],
  ] as const;
  const steps = ['forget_preview', 'counts + token (10 min, single use)', 'owner confirms in a new message', 'forget_apply · squash git history'];
  return (
    <>
      {stores.map(([n, role, d, c, k], j) => (
        <Block key={n} at={cue(k)} lit={i === k} color={c} w={500} style={{left: 110 + j * 580, top: 250}} title={<span>{n} <span style={{font: '400 22px Gochi Hand', color: C.inkSoft}}>· {role}</span></span>} sub={d} />
      ))}
      <Chip at={cue(3) + 10} color={C.pinkSoft} style={{left: 1270, top: 410}} size={17}>leaves the Mac: extraction + embeddings</Chip>
      <Beat from={4} style={{left: 110, top: 500}}>
        <div style={{display: 'flex', gap: 12, alignItems: 'center'}}>
          <Pill color={C.purple} size={22} delay={cue(4)}>04:30 · the dream</Pill>
          {['reconcile the day → records', 'consolidate', 'feed mem0', 'git commit', 'catch up missed nights'].map((s, k) => (
            <React.Fragment key={s}>
              <Arrow width={50} delay={cue(4) + 4 + k * 5} />
              <Pill color={C.sheet} size={18} delay={cue(4) + 6 + k * 5}>{s}</Pill>
            </React.Fragment>
          ))}
        </div>
      </Beat>
      <Beat from={5} style={{left: 110, top: 640}}>
        <div style={{display: 'flex', gap: 12, alignItems: 'center'}}>
          <Pill color={C.pink} size={22} delay={cue(5)}>forget</Pill>
          {steps.map((s, k) => (
            <React.Fragment key={s}>
              <Arrow width={50} delay={cue(k < 2 ? 5 : 6) + 4 + k * 4} />
              <Pill color={k === 2 ? C.sun : C.sheet} size={18} delay={cue(k < 2 ? 5 : 6) + 6 + k * 4}><span style={{fontFamily: k === 0 || k === 3 ? MONO : undefined}}>{s}</span></Pill>
            </React.Fragment>
          ))}
        </div>
      </Beat>
      <Beat from={7} style={{left: 110, top: 760}}>
        <Tag color={C.teal} delay={cue(7)}>Forget.lean: the dream can no longer write forgotten lines back</Tag>
      </Beat>
    </>
  );
}

// ---------- 7. the watcher ----------

function WatchScene() {
  const cue = useCue();
  const i = useCueIndex();
  const {frame} = useT();
  const rungs: [string, number][] = [
    ['plain fetch', 2],
    ['Chrome-like TLS handshake', 2],
    ['page structured data · no model', 3],
    ['cheap model on page text', 3],
    ['headless Chromium + vision', 3],
    ['Firecrawl · ≤ 30 a day', 3],
    ['web search', 3],
  ];
  // A price line crossing a threshold, then re-arming.
  const pts = Array.from({length: 60}, (_, k) => [k * 11, 90 + Math.sin(k / 6) * 60 + Math.sin(k / 2.3) * 8] as const);
  const shown = Math.floor(fade(frame, cue(1), cue(1) + 60) * pts.length);
  return (
    <>
      <Beat from={0} style={{left: 110, top: 250, width: 760}}>
        <div style={{display: 'flex', gap: 10, flexWrap: 'wrap'}}>
          {['quote', 'page', 'search', 'ticketmaster'].map((k, n) => <Pill key={k} color={C.sheet} size={21} delay={cue(0) + n * 4}><span style={{fontFamily: MONO}}>{k}</span></Pill>)}
        </div>
      </Beat>
      <Beat from={1} style={{left: 110, top: 350}}>
        <svg width={700} height={220} style={{overflow: 'visible'}}>
          <line x1={0} y1={110} x2={660} y2={110} stroke={C.pink} strokeWidth={3} strokeDasharray="8 8" />
          <text x={560} y={100} fontFamily="Andika" fontSize={20} fill={C.ink}>below 300</text>
          <polyline points={pts.slice(0, shown).map(([x, y]) => `${x},${y}`).join(' ')} fill="none" stroke={C.ink} strokeWidth={4} />
          {shown > 15 && <circle cx={pts[15][0]} cy={pts[15][1]} r={10} fill={C.sun} stroke={C.ink} strokeWidth={3} />}
          {shown > 15 && <text x={pts[15][0] - 20} y={200} fontFamily="Andika" fontSize={20} fill={C.ink}>fires once</text>}
          {shown > 34 && <text x={pts[34][0] - 30} y={20} fontFamily="Andika" fontSize={20} fill={C.ink}>re-arms</text>}
        </svg>
      </Beat>
      <Beat from={4} style={{left: 110, top: 640}}>
        <div style={{display: 'flex', flexDirection: 'column', gap: 12}}>
          <Pill color={C.sheet} size={20} delay={cue(4)}>one global limiter · 12 requests a minute</Pill>
          <Pill color={C.sheet} size={20} delay={cue(4) + 8}>a page that climbed once starts there next time</Pill>
        </div>
      </Beat>
      {rungs.map(([r, c], k) => {
        const at = cue(c) + (c === 3 ? (k - 2) * 7 : k * 8);
        const lit = i >= c;
        return <Block key={r} at={at} lit={lit} color={k < 2 ? C.sky : C.sun} w={620} style={{left: 1150, top: 780 - k * 84, padding: '10px 18px'}} title={<span style={{fontSize: 22}}>{`${k + 1}. ${r}`}</span>} />;
      })}
    </>
  );
}

// ---------- 8. verification ----------

function ProofScene() {
  const cue = useCue();
  const i = useCueIndex();
  const {frame} = useT();
  const b = fade(frame, cue(5) - 12, cue(5) + 4);
  const traces: [string, string, string, string][] = [
    ['no retry', '[drop]', 'turn fails on one drop', C.pink],
    ['naive retry', '[text, drop, text]', 'sentence read twice', C.pink],
    ['the fix', 'every trace', 'holds (by induction)', C.teal],
  ];
  return (
    <>
      <div style={{position: 'absolute', inset: 0, opacity: 1 - b}}>
        <Stat n="20" label="Lean 4 models" at={cue(0)} style={{left: 130, top: 250}} />
        <Stat n="250" label="theorems" at={cue(0) + 8} color={C.teal} style={{left: 390, top: 250}} />
        <Stat n="3,467" label="pytest passing (2026-09-28)" at={cue(0) + 16} color={C.pink} style={{left: 690, top: 250}} />
        <Block at={cue(1)} lit={i === 1} color={C.pink} w={620} style={{left: 1180, top: 240}} title={<span style={{fontFamily: MONO}}>current_violates</span>} sub="one concrete trace on the old code: the bug, proved" />
        <Block at={cue(2)} lit={i === 2} color={C.teal} w={620} style={{left: 1180, top: 380}} title={<span style={{fontFamily: MONO}}>fixed_invariant</span>} sub="the property on the fixed code, for every event list" />
        <Block at={cue(3)} lit={i === 3} color={C.sun} w={620} style={{left: 1180, top: 520}} title="replayed as pytest" sub="the counterexample against the real Python: red before, green after" />
        <Block at={cue(4)} lit={i === 4} color={C.sky} w={620} style={{left: 1180, top: 660}} title="check-all" sub="no sorry · only propext, Classical.choice, Quot.sound" />
        <Beat from={1} to={5} style={{left: 110, top: 470}}>
          <Code at={cue(1)} w={1000} size={19} title="ResponseRetry.lean" lines={[
            'theorem current_violates :',
            '    let s := Cur.run [.drop]',
            '    s.failed = true ∧ ... ∧ Spec s = false := by',
            '  decide',
            '',
            'theorem fixed_invariant (evs : List Event) :',
            '    Spec (Fix.run evs) = true := by ...',
          ]} />
        </Beat>
      </div>
      <div style={{position: 'absolute', inset: 0, opacity: b}}>
        {frame > cue(5) - 12 && (
          <>
            <Beat from={5} style={{left: 110, top: 250, font: '400 28px/1.35 Andika', color: C.ink, width: 800}}>
              2026-09-28: three phone-call turns died on a bare <b style={{fontFamily: MONO}}>ssl.SSLError</b>.
            </Beat>
            <Beat from={6} style={{left: 110, top: 350, font: '400 26px/1.35 Andika', color: C.ink, width: 800}}>
              The reply streams into speech. Retry after a sentence was read → it is read <b>twice</b>.
            </Beat>
            <Beat from={7} style={{left: 110, top: 470}}>
              <div style={{display: 'flex', gap: 12, alignItems: 'center'}}>
                <span style={{font: '700 22px Andika'}}>events</span>
                {['drop', 'text', 'done'].map((e, k) => <Pill key={e} color={C.sheet} size={20} delay={cue(7) + k * 4}><span style={{fontFamily: MONO}}>{e}</span></Pill>)}
              </div>
              <div style={{font: `400 20px/1.4 ${MONO}`, marginTop: 16, color: C.ink, opacity: fade(frame, cue(7) + 14, cue(7) + 26)}}>
                Spec: !duplicated ∧ (!failed ∨ droppedTwice ∨ droppedAfterText)
              </div>
            </Beat>
            <Beat from={8} style={{left: 110, top: 620}}>
              <div style={{display: 'grid', gridTemplateColumns: '200px 260px 320px', rowGap: 12, font: '400 22px Andika', alignItems: 'center'}}>
                {traces.map(([who, tr, res, c], k) => (
                  <React.Fragment key={who}>
                    <div style={{opacity: fade(frame, cue(k < 2 ? 8 : 9) + k * 6, cue(k < 2 ? 8 : 9) + k * 6 + 10), fontWeight: 700}}>{who}</div>
                    <div style={{opacity: fade(frame, cue(k < 2 ? 8 : 9) + k * 6, cue(k < 2 ? 8 : 9) + k * 6 + 10), fontFamily: MONO, fontSize: 20}}>{tr}</div>
                    <div style={{opacity: fade(frame, cue(k < 2 ? 8 : 9) + k * 6 + 4, cue(k < 2 ? 8 : 9) + k * 6 + 14)}}><Pill color={c} size={18} delay={cue(k < 2 ? 8 : 9) + k * 6 + 4}>{res}</Pill></div>
                  </React.Fragment>
                ))}
              </div>
            </Beat>
            <Beat from={9} style={{left: 960, top: 250}}>
              <Code at={cue(9)} w={860} size={19} title="computer_agent.py · make_stream_creator" lines={[
                'for attempt in range(TLS_RETRIES + 1):  # = 2',
                '    spoke = False',
                '    try:',
                '        ...  # on each text delta:',
                '        spoke = True; on_text(event.delta)',
                '    except ssl.SSLError as e:',
                '        if attempt == TLS_RETRIES or spoke:',
                '            raise',
                '        continue  # once more',
              ]} />
            </Beat>
            <Beat from={10} style={{left: 960, top: 700}}>
              <Tag color={C.sun} delay={cue(10)}>3,467 passed · 15 skipped · 2026-09-28</Tag>
            </Beat>
          </>
        )}
      </div>
    </>
  );
}

// ---------- 9. spend and privacy ----------

function PrivacyScene() {
  const cue = useCue();
  const rows: [string, string, number][] = [
    ['reflex rules', 'nothing', 2],
    ['Jev (router)', "the request's words · owner's switch", 2],
    ['Firecrawl reader', 'query ≤ 500 chars · pages to a cheap model', 3],
    ['Chrome lane planner', 'screenshots + prompt', 4],
    ['Holo', 'hosted model API · local screenshot runs deleted', 4],
    ['Telegram · Mini App', 'Telegram servers (not E2E) · Cloudflare tunnel', 5],
    ['mem0', 'extraction + embeddings to OpenAI · telemetry off', 5],
  ];
  const {frame} = useT();
  return (
    <>
      <Beat from={0} style={{left: 110, top: 245}}>
        <div style={{display: 'flex', gap: 10, alignItems: 'center'}}>
          <span style={{font: '700 24px Andika', marginRight: 8}}>one line per call:</span>
          {['provider', 'model', 'feature', 'dollars', 'tokens'].map((f, k) => <Pill key={f} color={C.sheet} size={19} delay={cue(0) + k * 4}><span style={{fontFamily: MONO}}>{f}</span></Pill>)}
          <Pill color={C.pinkSoft} size={19} delay={cue(0) + 26}>never a prompt · never an answer</Pill>
        </div>
      </Beat>
      <Beat from={1} style={{left: 110, top: 320}}>
        <div style={{display: 'flex', gap: 10}}>
          <Pill color={C.teal} size={18} delay={cue(1)}>reported by provider</Pill>
          <Pill color={C.sun} size={18} delay={cue(1) + 5}>priced from a table</Pill>
          <Pill color={C.sheet} size={18} delay={cue(1) + 10}>unknown → empty, not guessed</Pill>
        </div>
      </Beat>
      <Card delay={cue(2)} color={C.sky} style={{position: 'absolute', left: 110, top: 400, width: 1700}} float={false}>
        <div style={{padding: '14px 24px'}}>
          <div style={{display: 'grid', gridTemplateColumns: '360px 1fr', font: '700 20px Andika', color: C.inkSoft, paddingBottom: 8, borderBottom: `2px solid ${C.desk}`}}>
            <div>lane</div><div>what leaves the Mac</div>
          </div>
          {rows.map(([lane, what, c], k) => {
            const a = fade(frame, cue(c) + (k % 2) * 8, cue(c) + (k % 2) * 8 + 12);
            return (
              <div key={lane} style={{display: 'grid', gridTemplateColumns: '360px 1fr', font: '400 24px/1.6 Andika', opacity: a, transform: `translateX(${(1 - a) * -20}px)`}}>
                <div style={{fontWeight: 700}}>{lane}</div><div>{what}</div>
              </div>
            );
          })}
        </div>
      </Card>
    </>
  );
}

// ---------- 10. run it ----------

function RunScene() {
  const cue = useCue();
  return (
    <>
      <Beat from={0} style={{left: 110, top: 250}}>
        <Code at={cue(0)} w={860} size={21} title="switches (shipped defaults)" lines={[
          'EVE_TELEGRAM=1          # off by default',
          'EVE_MINIAPP=1           # off by default',
          'EVE_MEMORY=1            # off by default',
          'EVE_COMPUTER=holo       # default: codex',
          'EVE_ROUTER_MODEL=jev    # default: off',
          'EVE_BROWSER_OWN=1       # default: 0',
          'EVE_WEB_READER=auto     # on with FIRECRAWL_API_KEY',
        ]} />
      </Beat>
      <Beat from={3} style={{left: 1040, top: 250, width: 780}}>
        <div style={{font: '400 28px Gochi Hand', color: C.inkSoft, marginBottom: 12}}>read next</div>
        <div style={{display: 'flex', flexDirection: 'column', gap: 12}}>
          {['docs/stackchan/routing.md', 'docs/holo-computer-use.md', 'docs/memory-bus.md · memory.py', 'docs/verification.md', 'verification/Eve/*.lean'].map((p, k) => (
            <Pill key={p} color={k === 4 ? C.sun : C.sheet} size={21} delay={cue(3) + k * 5}><span style={{fontFamily: MONO}}>{p}</span></Pill>
          ))}
        </div>
      </Beat>
    </>
  );
}

// ---------- captions and chrome ----------

function Captions() {
  const frame = useCurrentFrame();
  let text = '';
  let o = 0;
  for (const s of timeline.sections) {
    if (frame < s.from || frame >= s.from + s.duration) continue;
    for (const c of s.cues) {
      const a = s.from + c.from;
      const b = a + c.duration;
      if (frame >= a - 3 && frame < b + 6) {
        text = c.text;
        o = Math.min(fade(frame, a - 3, a + 5), 1 - fade(frame, b, b + 6));
      }
    }
  }
  return (
    <div style={{position: 'absolute', left: 0, right: 0, bottom: 34, display: 'flex', justifyContent: 'center', zIndex: 40, pointerEvents: 'none'}}>
      {text && (
        <div style={{maxWidth: 1560, background: 'rgba(31,58,120,0.92)', color: '#FFFDF8', font: '400 33px/1.32 Andika', padding: '14px 30px', borderRadius: 12, textAlign: 'center', opacity: o}}>
          {text}
        </div>
      )}
    </div>
  );
}

function Progress() {
  const frame = useCurrentFrame();
  const n = timeline.sections.length;
  return (
    <div style={{position: 'absolute', left: 0, right: 0, bottom: 0, height: 10, display: 'flex', gap: 4, zIndex: 41}}>
      {timeline.sections.map((s, k) => {
        const p = interpolate(frame, [s.from, s.from + s.duration], [0, 1], {extrapolateLeft: 'clamp', extrapolateRight: 'clamp'});
        return (
          <div key={s.id} style={{flex: s.duration, background: C.desk, position: 'relative'}}>
            <div style={{position: 'absolute', inset: 0, width: `${p * 100}%`, background: ACCENTS[k % n]}} />
          </div>
        );
      })}
    </div>
  );
}

const SCENES: Record<string, React.FC> = {
  what: WhatScene,
  hub: HubScene,
  path: PathScene,
  holo: HoloScene,
  brain: BrainScene,
  memory: MemoryScene,
  watch: WatchScene,
  proofs: ProofScene,
  privacy: PrivacyScene,
  run: RunScene,
};

export function EveEngineering() {
  return (
    <AbsoluteFill style={{background: C.paper}}>
      <style dangerouslySetInnerHTML={{__html: fontFace}} />
      {timeline.sections.map((s, k) => {
        const meta = script.sections.find((m) => m.id === s.id)!;
        const Scene = SCENES[s.id];
        return (
          <Sequence key={s.id} from={s.from} durationInFrames={s.duration} name={meta.kicker}>
            <SectionCtx.Provider value={s}>
              <SceneFrame accent={ACCENTS[k]} duration={s.duration}>
                <Header kicker={meta.kicker} title={meta.title} />
                <Scene />
              </SceneFrame>
            </SectionCtx.Provider>
          </Sequence>
        );
      })}
      {timeline.sections.slice(1).map((s, k) => (
        <Sequence key={`wipe-${s.id}`} from={s.from - 16} durationInFrames={32}><Wipe color={ACCENTS[k + 1]} /></Sequence>
      ))}
      <Captions />
      <Progress />
    </AbsoluteFill>
  );
}
