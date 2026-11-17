# eve's chat page and card catalog

Two things live here:

- **eve lite's web app**: the chat as a web page, drawn with OpenUI and json-render in React
  ([below](#eve-lites-web-app-srcweb)).
- **The card catalog** (`src/jsonCatalog.ts`): the one list of card components and actions. The reference
  the brain reads and the native iPhone app's Swift catalog are both generated from it.

The iPhone app does not use this page any more. Its chat is native SwiftUI
(`../Packages`, [docs/app-chat.md](../../../docs/app-chat.md#how-it-is-built)), and `sh ios/EveLink/build.sh`
builds nothing here.

Use Node 22.18 or later and pnpm (the json-render scripts run their TypeScript sources as they are).

- `pnpm gen-swift-catalog` writes `../Packages/EveCards/Sources/EveCardCore/Resources/card-catalog.json`
  from the catalog; `pnpm check-swift-catalog` fails when that file is stale.
- `pnpm gen-card-fixtures` writes `../Packages/EveCards/Tests/EveCardCoreTests/Fixtures/card-parity.json`:
  55 cards, whole and damaged, with what the web reader makes of each. The Swift tests must read every one
  the same way. `pnpm check-card-fixtures` fails when it is stale.
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
- `pnpm gen-json-prompt` writes `bridge/src/eve_bridge/json_render_prompt.txt`:
  the json-render reference for app turns while the card engine is json-render
  (`EVE_CARD_ENGINE=json-render` on the Mac). Its component and action lists
  come from the catalog in `src/jsonCatalog.ts`; `pnpm check-json-prompt` exits
  1 when the committed file is stale. `pnpm validate-spec [file]` checks a
  json-render block as the app does. See `docs/app-chat.md`, "The card engine".
- `pnpm preview-cards` serves `preview.html`, a development page that draws a
  json-render block as the app does and lists what each tap sends. It is not
  part of either build.
- `pnpm build:web` writes eve lite's web app to
  `bridge/src/eve_bridge/web_lite/`, from `web.html` and `vite.web.config.ts`
  (below).
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
| `src/replyParts.ts` | Splits a reply into markdown, OpenUI Lang and json-render parts |
| `src/GeneratedCard.tsx`, `src/uiProgram.ts` | Validated streaming cards, readable static text, per-card input reset and bounded page-local state |
| `src/jsonCatalog.ts`, `src/jsonRegistry.tsx` | The json-render engine: the components and actions a block may use, and how each is drawn (with OpenUI's own components) |
| `src/specProgram.ts`, `src/JsonCard.tsx` | A json-render block to a checked spec, and its card: streaming, readable text, input reset |
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
entry around the chat page (`App.tsx`), which the iPhone app also ran until 2026-11-17.

- **The page.** `App.tsx`, `nativeBridge.ts`, `llm.ts`, `storage.ts` and `sync.ts` were written for
  the app's web view and keep its bridge. `createBridge(webPost)` stands in for the app's native code: `src/web/host.ts`
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
