// meet_page.js — the script eve runs inside its Google Meet tab (meet.py evaluates it with Playwright).
//
// One call, one job: read the page, take at most the few safe steps toward being in the call as a silent
// listener, and hand back what it saw. meet.py polls it about once a second; the script is idempotent, so a
// step that did not land is simply taken again on the next poll.
//
// Listen only, by construction. The ONLY things this script ever clicks are named in SAFE below, and click()
// refuses anything else. It turns the microphone and the camera OFF (never on), presses Join only when both
// are already off, and when Meet says the account is already in the call on another device it presses
// "Join here too" — never "Switch here", which would take the call away from the owner's other device.
//
// Ported from OpenClaw's google-meet plugin (extensions/google-meet/src/transports/google-meet-page-scripts.ts
// and google-meet-caption-observer-source.ts, MIT, Copyright (c) 2026 OpenClaw Foundation): the button labels,
// the lobby, ended and sign-in wording, and the caption region selector. The caption bookkeeping is eve's
// own: the page only records what the caption region shows, and meet.py (CaptionMerger) turns those snapshots
// into lines, where it can be tested without a browser.
async (opts) => {
  const o = opts || {};
  const SAFE = {
    "mic-off": /^\s*turn off microphone\b/i,
    "camera-off": /^\s*turn off camera\b/i,
    "no-mic": /\b(continue|join|use) without (microphone|mic)\b|\bcontinue without (microphone and camera|camera and microphone)\b/i,
    "join": /^\s*(join now|ask to join)\b/i,
    "join-here-too": /^\s*join here too\b/i,
    "captions-on": /^\s*turn on captions\b/i,
    "leave-confirm": /^\s*leave meeting\b/i,
    "leave": /^\s*leave call\b/i,
  };
  const NEVER = /switch here|companion|turn on microphone|turn on camera|unmute|present now|remotely mute|someone else/i;

  const S = window.__eveMeet || (window.__eveMeet = {
    snaps: [], seq: 0, ids: new WeakMap(), nextId: 1, observer: null, timer: 0,
    captionTries: 0, captionTriedAt: 0, clicks: [], lastSig: "",
  });
  const text = (n) => ((n && (n.innerText || n.textContent)) || "").trim();
  const label = (b) => [b.getAttribute("aria-label"), b.getAttribute("data-tooltip"), text(b)]
    .filter(Boolean).join(" ");
  const buttons = () => [...document.querySelectorAll('button, [role="button"]')];
  const usable = (b) => !b.disabled && b.getAttribute("aria-disabled") !== "true";
  const find = (why) => buttons().find((b) => SAFE[why].test(label(b)) && !NEVER.test(label(b)) && usable(b));
  const has = (re) => buttons().some((b) => re.test(label(b)));
  const clicked = [];
  const click = (why) => {
    if (!o.act || !(why in SAFE)) return false;
    const b = find(why);
    if (!b) return false;
    b.click();
    clicked.push(why);
    S.clicks.push(why);
    return true;
  };

  // ---- quiet: the call is not played out loud on the Mac (the owner may be on another device, and the
  // robot's own microphone is in the same room). Captions come from Meet, not from this tab's playback.
  if (o.quiet) {
    for (const el of document.querySelectorAll("audio, video")) {
      if (!el.muted) el.muted = true;
    }
  }

  const host = location.hostname.toLowerCase();
  const body = text(document.body).toLowerCase();
  const inCall = has(/^\s*leave call\b/i);
  const micOn = has(/^\s*turn off microphone\b/i);
  const camOn = has(/^\s*turn off camera\b/i);
  const joinHereToo = Boolean(find("join-here-too"));
  const switchHere = has(/^\s*switch here\b/i);
  const signIn = !inCall && (host === "accounts.google.com" ||
    /use your google account|to continue to google meet|choose an account|sign in to (join|continue)/i.test(body));
  const lobby = !inCall && /asking to be let in|you.?ll join (the call )?when someone lets you in|waiting to be let in|someone will let you in soon|waiting for the host/i.test(body);
  const endedMatch = inCall ? null : body.match(/you can.?t join this (video )?call|denied your request to join|no one responded to your request|you.?ve been removed from the meeting|removed from the meeting|you were removed|you left the meeting|you.?ve left the meeting|the call has ended|call ended|meeting ended|this meeting has ended|check your meeting code|meeting code (is|was) invalid|invalid video call name/i);
  const ended = endedMatch ? endedMatch[0] : (!inCall && has(/^\s*(rejoin|return to home screen)\b/i) ? "left the call" : "");
  const permission = /permission needed|microphone (is )?blocked|camera (is )?blocked|allow meet to use your/i.test(body);

  // ---- leaving: asked for by meet.py, and nothing else happens on that pass
  if (o.leave) {
    if (!click("leave-confirm")) click("leave");
    return { left: true, inCall, ended, clicked, url: location.href };
  }

  // ---- toward the call: mic and camera off first, then Join, never in the same pass as turning one off
  let joinClicked = false;
  if (!inCall && !ended && !signIn) {
    click("no-mic");                       // "Do you want people to hear you?" → without the microphone
    if (o.guestName) {
      const name = [...document.querySelectorAll("input")].find((el) =>
        /your name/i.test(el.getAttribute("aria-label") || el.placeholder || ""));
      if (name && !name.value && o.act) {
        name.focus();
        name.value = o.guestName;
        name.dispatchEvent(new Event("input", { bubbles: true }));
        name.dispatchEvent(new Event("change", { bubbles: true }));
      }
    }
    const turnedOff = [micOn && click("mic-off"), camOn && click("camera-off")].some(Boolean);
    // Off means SEEN off: the preview's toggles render after the Join button, microphone on by default, so
    // "no toggle yet" is not "off". Only when Meet shows no toggle at all for a while (no device, no
    // permission) does meet.py let a pass join without one (noControlsOk); in the call it is muted again.
    const micOff = has(/^\s*turn on microphone\b/i) || (o.noControlsOk && !micOn);
    const camOff = has(/^\s*turn on camera\b/i) || (o.noControlsOk && !camOn);
    if (!turnedOff && micOff && camOff && !lobby) {
      joinClicked = joinHereToo ? click("join-here-too") : click("join");
    }
  }

  // ---- in the call: stay muted and dark, turn captions on, record what they show
  if (inCall) {
    if (micOn) click("mic-off");
    if (camOn) click("camera-off");
    const captionsOn = has(/^\s*turn off captions\b/i);
    if (o.captions && !captionsOn && S.captionTries < 5 && Date.now() - S.captionTriedAt > 3000) {
      S.captionTries += 1;
      S.captionTriedAt = Date.now();
      click("captions-on");
    }
  }

  const idOf = (el) => {
    let id = S.ids.get(el);
    if (!id) { id = S.nextId++; S.ids.set(el, id); }
    return id;
  };
  // The words of a caption block, without its buttons and icon ligatures ("arrow_downward").
  const linesOf = (el) => {
    const out = [];
    const walk = (n) => {
      for (const c of n.childNodes) {
        if (c.nodeType === 3) {
          const t = c.textContent.replace(/\s+/g, " ").trim();
          if (t) out.push(t);
        } else if (c.nodeType === 1) {
          const tag = c.tagName;
          if (tag === "BUTTON" || tag === "I" || tag === "SCRIPT" || tag === "STYLE" ||
              c.getAttribute("role") === "button") continue;
          walk(c);
        }
      }
    };
    walk(el);
    return out;
  };
  const regions = () => [...document.querySelectorAll('[role="region"][aria-label*="aption" i]')];
  // The raw words, spaces kept: Meet splits a caption into text nodes that carry their own spaces.
  const said = (el) => {
    let out = "";
    const walk = (n) => {
      for (const c of n.childNodes) {
        if (c.nodeType === 3) out += c.textContent;
        else if (c.nodeType === 1 && !["BUTTON", "I", "SCRIPT", "STYLE"].includes(c.tagName) &&
                 c.getAttribute("role") !== "button") walk(c);
      }
    };
    walk(el);
    return out.replace(/\s+/g, " ").trim();
  };
  // One speaker turn, as Meet draws it (live, 2026-09-25): a block of two parts, the name (one short line)
  // and the words (one element whose words arrive as many text nodes: joined, never split into lines).
  const shape = (el) => {
    const parts = [...el.children].filter((k) => said(k));
    if (parts.length < 2) return null;
    const name = linesOf(parts[0]);
    if (name.length !== 1 || name[0].length > 80) return null;
    return { speaker: name[0], words: parts.slice(1).map(said).join(" ").replace(/\s+/g, " ").trim() };
  };
  // The region's blocks: its children of that shape, or one wrapper level down when Meet nests them.
  const blocksOf = (region) => {
    const kids = [...region.children].filter((k) => said(k));
    if (kids.some((k) => shape(k))) return kids;
    const inner = kids.flatMap((k) => [...k.children]).filter((k) => said(k));
    return inner.some((k) => shape(k)) ? inner : kids;
  };
  const rowsNow = () => {
    const rows = [];
    for (const region of regions()) {
      for (const block of blocksOf(region)) {
        const s = shape(block);
        const self = Boolean(block.closest('[data-is-self="true"]') || block.querySelector('[data-is-self="true"]'));
        // a name with no words yet, or a stray control, is not a caption
        const words = s ? s.words : "";
        if (words.length < 2 || /^(turn on captions|turn off captions|captions)$/i.test(words)) continue;
        rows.push({ id: idOf(block), speaker: s.speaker, text: words, self });
      }
    }
    return rows;
  };
  const record = () => {
    S.timer = 0;
    const rows = rowsNow();
    const sig = JSON.stringify(rows.map((r) => [r.id, r.speaker, r.text]));
    if (sig === S.lastSig) return;
    S.lastSig = sig;
    S.seq += 1;
    S.snaps.push({ seq: S.seq, t: Date.now(), rows });
    if (S.snaps.length > 3000) S.snaps.splice(0, S.snaps.length - 3000);
  };
  if (inCall && o.captions && !S.observer) {
    S.observer = new MutationObserver(() => {
      if (!S.timer) S.timer = setTimeout(record, 150);
    });
    S.observer.observe(document.body, { childList: true, subtree: true, characterData: true });
  }
  if (inCall && o.captions) record();

  const since = Number(o.since) || 0;
  return {
    url: location.href, title: document.title, host,
    inCall, lobby, ended, signIn, permission,
    micOn, camOn, joinHereToo, switchHere, joinClicked, clicked,
    captionsOn: has(/^\s*turn off captions\b/i),
    captionRegion: regions().length > 0,
    snaps: S.snaps.filter((s) => s.seq > since),
    seq: S.seq,
  };
}
