# Eve Link chat web layer

The message list of the Eve Link chat, rendered by OpenUI inside the app's
WKWebView. Everything the owner touches around it (composer, navigation,
haptics, network) is native.

The same page also runs in a browser as eve lite's web app, with a shell of its
own in place of the native code: see [eve lite's web app](#eve-lites-web-app-srcweb).

Use Node 22.12 or later and pnpm. `sh ios/EveLink/build.sh` from the repository
root installs the locked web dependencies and rebuilds the chat before building
the app. When building directly in Xcode, first run `pnpm install --frozen-lockfile`
and `pnpm build` in this directory. Generated bundle changes stay out of commits.

- `pnpm build` writes `../App/ChatWeb/` (index.html, chat.js, chat.css). The
  app serves those files from its bundle on `eveapp://chat/index.html`.
  The script is a classic deferred IIFE (no module/CORS loads on the custom
  scheme). A CSP meta tag sets `connect-src 'none'`, so the page cannot reach
  the network itself.
- `pnpm gen-prompt` writes `bridge/src/eve_bridge/openui_prompt.txt`:
  the openui-lang reference for app turns. It is the library's own prompt,
  trimmed to the components a phone chat uses (`CHAT_COMPONENTS`), with four
  small examples that are parsed before anything is written.
  `pnpm check-prompt` exits 1 when the committed file is stale. When to use
  OpenUI is decided by the bridge (`link_chat.APP_RULE`). See
  `docs/app-chat.md`, "When a reply uses OpenUI".
- `pnpm validate [file]` (or `node scripts/validate.mjs`, from any folder)
  parses a program with the app's parser and library, and exits 1 on a parse
  error, an unknown component, an undefined name, or a cut-off program.
  `--batch` reads a JSON array of programs.
- `pnpm build:web` writes eve lite's web app to
  `bridge/src/eve_bridge/web_lite/`, from `web.html` and `vite.web.config.ts`
  (below). It never touches `../App/ChatWeb/`.
- `pnpm test` (vitest), `pnpm exec tsc --noEmit`.

Source map:

| File | What it does |
|---|---|
| `src/nativeBridge.ts` | `window.eveNative` and posts to `webkit.messageHandlers.eve` |
| `src/llm.ts` | ChatLLM: a turn goes through native; reply parsed by `openAIAdapter()` |
| `src/storage.ts` | One-thread ChatStorage (history) and the visible-page long poll |
| `src/sync.ts` | Transcript lines to messages; the ledger that drops poll copies of a streamed turn |
| `src/reactions.ts`, `src/Reaction.tsx` | eve's reaction on an owner bubble (Telegram's receipts) and its badge |
| `src/sources.ts`, `src/SourceList.tsx` | "Sources · N" under a reply: the memory lines it leaned on, kept by message id |
| `src/replyParts.ts` | Splits a reply into markdown and OpenUI Lang parts |
| `src/GeneratedCard.tsx`, `src/uiProgram.ts` | Validated streaming cards, readable static text, per-card input reset and bounded page-local state |
| `src/cardActions.ts` | Existing conversation/link action routing, with busy and duplicate-send protection |
| `src/AssistantMessage.tsx` | Renders those parts (MarkDownRenderer / react-lang Renderer), and eve's own pictures and files above them |
| `src/attachments.ts` | Attachments: sanitizing, the owner's binary parts, eve's media kept by message id |
| `src/AttachmentList.tsx` | Thumbnails and chips for a message's attachments; a tap asks native to open one full size |
| `src/history.ts`, `src/HorizontalHistory.tsx` | Horizontal History: topic cards over the chat, the star, the jump back (docs/app-chat.md) |
| `src/App.tsx` | AgentInterface with only the message list; native send/scroll/busy glue |
| `src/theme.ts`, `src/native.css` | iOS system colors and font, no zoom/callouts/tap highlight |

`pnpm-workspace.yaml` denies `@openuidev/lang-core`'s postinstall (it only
sends install telemetry).

## eve lite's web app (`src/web`)

[eve lite](../../../docs/lite.md) has no iPhone app: the Mac serves this chat as a web page
(`bridge/src/eve_bridge/lite_web.py`), to its own browser at `http://localhost:48780` and to
phones and laptops on the same Wi-Fi over HTTPS. `web.html` and `src/web/` are a second Vite
entry around the iOS page.

- **Shared, unchanged.** `App.tsx`, `nativeBridge.ts`, `llm.ts`, `storage.ts` and `sync.ts` are the
  iOS page's own, and so are `vite.config.ts` and `../App/ChatWeb/`; `pnpm build` and the app are
  as they were. `createBridge(webPost)` stands in for the app's native code: `src/web/host.ts`
  answers the page's bridge messages over WebRTC or HTTPS, and the shell draws the top bar, the
  composer, voice, settings, setup and pairing outside `App`.
- **`pnpm build:web`** (`vite.web.config.ts`) writes `bridge/src/eve_bridge/web_lite/`:
  `index.html` (renamed from `web.html`) and one script and one stylesheet,
  `assets/eve-<hash>.js` and `.css`, with no code splitting, so the Mac's small server answers two
  requests and not one per icon. It targets Safari 16.4, Chrome 111, Firefox 114 and Edge 111.
  The folder is git-ignored; `tools/release_lite.sh` builds it from the commit and packs it into
  the wheel, and the daemon serves it at `/` and `/assets/*` (or `EVE_WEB_DIR` instead).
- **No new npm dependency.** The pairing QR codes are drawn on the Mac (segno, as SVG data URIs).
- **The CSP**, in `web.html`'s meta tag and sent as a header by the Mac's doors:
  `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self' data:; media-src 'self' blob:; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'`,
  and the header adds `frame-ancestors 'none'`, which a meta tag cannot carry. Inline styles are
  for OpenUI's injected `<style>` elements, as in the iOS page. Unlike the iOS page's
  `connect-src 'none'`, this page fetches its own origin: it has no native code to do it.
- **No token in the page.** The device cookie (`eve_mac` on the Mac, `eve_dev` on a phone) is
  HttpOnly. A one-time secret arrives in the URL fragment, is cleared at once and posted once;
  nothing is kept in JavaScript or `localStorage` but the Think choice.

| File | What it does |
|---|---|
| `web.html`, `src/web/main.tsx` | The entry: the fragment's secret taken first, then `createBridge(webPost)`, `window.eveNative`, and the page in its shell |
| `src/web/boot.ts` | Which door served the page (`http:` the Mac's, `https:` the LAN door's), the move from `127.0.0.1` to `localhost`, a `crypto.randomUUID` stand-in |
| `src/web/pair.ts` | `#p=` (a phone's pairing) and `#login=` (the Mac's sign-in): read, cleared with `history.replaceState`, posted once |
| `src/web/flow.ts`, `src/web/Root.tsx`, `src/web/Gate.tsx`, `src/web/Setup.tsx` | What the page shows: sign in, pair, set up, restarting, unreachable, then the chat |
| `src/web/api.ts` | The Mac's JSON routes, each answer read field by field |
| `src/web/host.ts` | `webPost`: `turn`, `messages`, `cancel`, `openURL`, `openAttachment`, `haptic`, `ready`, as the app's native code answers them |
| `src/web/transport.ts`, `src/web/rtc.ts` | The way to the Mac: loopback on the Mac; on a phone, the WebRTC peer (no ICE server), with HTTPS after 4 s without it and WebRTC retried |
| `src/web/framing.ts`, `src/web/linkChannel.ts`, `src/web/sse.ts` | The `link` DataChannel's frames, its requests and cancels, and a turn carried on over HTTPS after the link dropped |
| `src/web/voice.ts`, `src/web/VoiceView.tsx` | Push-to-talk: the `call` channel and the microphone on the peer ([voice](../../../docs/voice-mode.md#eve-lite-push-to-talk-in-a-browser)) |
| `src/web/Shell.tsx`, `src/web/Composer.tsx`, `src/web/upload.ts`, `src/web/Lightbox.tsx` | The top bar, the composer and its Think toggle, attachments (HEIC and AVIF to JPEG), the photo viewer |
| `src/web/Settings.tsx` | Apps, Phone access, Devices, Trust eve on this device, OpenRouter, Help, signing out |
| `src/web/windows.ts`, `src/web/viewport.ts`, `src/web/prefs.ts`, `src/web/icons.tsx`, `src/web/web.css` | Links out of the page, the shell on a phone's keyboard, the Think choice, the glyphs, the styles |
| `src/web/__tests__/` | Vitest: the frames, the link channel, the fallback, the fragments, the host, uploads, voice, the flow, the API, boot, settings and the screens |

`pnpm test` runs `src/web/__tests__/` with the rest (vitest's `src/**/*.test.ts`). How the chat
reaches the Mac: [the chat in a browser](../../../docs/app-chat.md#eve-lite-the-same-chat-in-a-browser).

## Staying on the newest OpenUI

`sh tools/openui_update.sh` (from the repository root) checks npm for newer `@openuidev/react-lang`, `react-ui` and
`react-headless`. When there is one, it bumps the pins in a worktree of its own (`../eve-openui-update`, never the
live checkout), moves every `@openuidev` entry in `pnpm-workspace.yaml`'s `minimumReleaseAgeExclude` to npm's newest
(so a release younger than pnpm's minimum age still installs), runs `pnpm install`, the tests, the type check, `gen-prompt` + `check-prompt`, the app's logic tests,
the bridge's chat tests and an app build; all green and the iPhone reachable, it installs the app, fast-forwards the
live checkout and restarts the daemon, so the card rules (Mac) and the renderer (phone) change together. Anything
failing changes nothing live and eve says what failed. `--dry-run` only reports; `--schedule` runs it every night at
04:50 (LaunchAgent `com.github.eve-bridge.openui-update`, log `~/Library/Logs/eve-openui-update.log`);
`--unschedule` stops it. It never pushes.

