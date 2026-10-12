# eve todo

## Composer: show what eve will do before you send (from Shapeshift)

Owner, 2026-10-12, after looking at [Shapeshift](https://lab.anishfn.ink/r/shapeshift.json) (shadcn registry item,
`bunx --bun shadcn@latest add https://lab.anishfn.ink/r/shapeshift.json`): an input that works out which "shape" a
sentence means while you type and opens that component over the field, already filled in ("split 2400 between 3"
opens a bill split; eight starter shapes: event, message, list, timer, split, poll, color, sum).

Assessed 2026-10-12, not installed:
- Safe: React + `motion` only, no network, no eval, no storage; touches `navigator.clipboard` and builds a Google
  Calendar template link.
- Does not drop in: Eve Link's composer is native SwiftUI (`ChatComposer` in `ios/EveLink/App/ChatScreen.swift`); the
  web view only draws messages. `ios/EveLink/Web` has no shadcn setup (no `components.json`, Tailwind or `@/lib/utils`)
  and uses pnpm, not bun.
- Its matcher is keywords/regex on the phone; eve already routes intent with a model (blind evals at 95%+). Two
  routers that disagree would confuse.

The idea to build: a native preview in `ChatComposer`. As you type "remind me at 9 to call mom", show the card eve is
about to make (reminder, event, timer, message) before you send, using eve's own routing, not a second matcher. Needs
a blind intent eval like any routing change (AGENTS.md rule 6).
