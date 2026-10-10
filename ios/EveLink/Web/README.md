# Eve Link chat web layer

The message list of the Eve Link chat, rendered by OpenUI inside the app's
WKWebView. Everything the owner touches around it (composer, navigation,
haptics, network) is native.

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
| `src/App.tsx` | AgentInterface with only the message list; native send/scroll/busy glue |
| `src/theme.ts`, `src/native.css` | iOS system colors and font, no zoom/callouts/tap highlight |

`pnpm-workspace.yaml` denies `@openuidev/lang-core`'s postinstall (it only
sends install telemetry).
