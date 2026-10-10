# Notifications

On 2026-10-08 the owner asked to "turn on notis for eve". Telegram is now
app-only (`EVE_TELEGRAM=app`). The chat's brain still runs for Eve Link,
calls, the watcher and the chief, but nothing is sent to Telegram. So nothing
made the phone buzz when eve said something the owner did not ask for.

Apple's push service is not available: Eve Link is signed with a free personal
team. So eve posts to an [ntfy](https://ntfy.sh) topic, and the free ntfy app
on the iPhone shows the notification. eve already uses ntfy.sh to post its
address for the app (the rendezvous, [action-button.md](action-button.md)).

## What notifies

eve sends a notification for each message Telegram would have pushed to the
phone:

- a watch alert (a price, a quote, tickets)
- a chief nudge or a question from a chief job
- a task or a Codex turn that finishes
- a Claude relay reply while `claude on` is set
- a reply to an app turn when the app stopped reading (you closed it before the
  reply came)
- a picture or a file eve sends ("eve sent a picture", or its caption).
  The picture itself shows in the Eve Link chat as eve's message
  ([app-chat.md](app-chat.md), "eve's pictures and files"). One asked for
  from the app is not pushed: it is already on the screen.

These do **not** notify:

- A reply to an Eve Link app turn while the app reads it. It goes to the app
  and does not go through Telegram.
- Edits, "typing…", drafts, reactions, the answer to a button tap, and the `/`
  menu.
- A progress message (the one with a **Stop** button). It says that work
  started. The message that says the work finished notifies.

Tap a notification to open Eve Link's Chat (`evelink://chat`).

### Bursts

A message waits 3 seconds for others. Then one notification goes out with the
newest message, and "(+N more)" when there were more. While messages keep
coming, eve posts at most once every 20 seconds and folds the rest into the
next one. The text is plain: eve removes Telegram's formatting, markdown and
OpenUI programs, and cuts the text at 300 characters.

## Setting it up

1. Pair Eve Link with the Mac first ([action-button.md](action-button.md#setting-it-up)).
   The topic comes from the link token.
2. Get the topic. In Eve Link, open **Settings > Notifications** and tap
   **Copy**. Or, on the Mac, run:

   ```sh
   eve-bridge notify topic
   ```

   It prints the topic, its `https://ntfy.sh/<topic>` address and the steps.
3. Install **ntfy** from the App Store. In Eve Link, **Open ntfy** opens it,
   or opens its App Store page when it is not installed.
4. In ntfy, tap **+**, paste the topic, keep the default server
   (`ntfy.sh`), and tap **Subscribe**. Allow notifications when iOS asks.
5. Check it from the Mac:

   ```sh
   eve-bridge notify test
   ```

   The phone shows "Notifications from eve work."

If you send `/stick new` (a new link token), the topic changes. Subscribe to the
new topic and unsubscribe from the old one.

## Privacy

ntfy.sh is a public server. Anyone who knows the topic can read what eve posts
on it. The topic is `eve-` and 32 hex characters of
HMAC-SHA256(link token, `eve notify`). Nobody can find the topic without the
link token, and the topic does not show the token. The rendezvous uses the same
method with a different label, so the two topics are different.

By default the notification shows the start of eve's message. To keep eve's
words off ntfy.sh, set `EVE_NOTIFY_PREVIEW=0`: each notification then says
"eve has a new message" (or "eve has N new messages"), and you open the app
to read it.

eve never writes the topic or the message text to its log. A failed post logs
one warning line with the error type only.

## Settings

Set these in `~/.config/eve-bridge/env` and restart the daemon.

| Variable | Default | What it does |
|---|---|---|
| `EVE_NOTIFY` | on | `0` turns notifications off. They run only with `EVE_TELEGRAM=app`; with Telegram on, Telegram notifies. Nothing posts until Eve Link is paired. |
| `EVE_NOTIFY_PREVIEW` | on | `0` sends "eve has a new message" instead of the words. |
| `EVE_NOTIFY_URL` | `https://ntfy.sh` | Another ntfy server. Subscribe to it in the ntfy app with that server instead of the default. |

## How it is built

- `bridge/src/eve_bridge/notify.py`: the topic, the plain text, the
  `Notifier` (bursts and the 20-second gap) and the post.
- `telegram.MutedBotApi`: in app-only mode, each `sendMessage`, `sendPhoto` and
  `sendDocument` goes to `Notifier.offer_send`. That call returns at once; the
  post runs on a worker thread from a task. A failure never reaches the send.
- `eve-bridge notify topic|test` (`cli.py`).
- Eve Link: `NotifyTopic` in `ios/EveLink/Shared/Rendezvous.swift` computes
  the same topic with CryptoKit. The Settings sheet shows it with **Copy** and
  **Open ntfy**. `LinkRoute.chat` opens the Chat.

## Tests

- `bridge/tests/test_notify.py`: the topic (a fixed vector), the switches, the
  plain text, one coalesced post for a burst, no post for edits, typing, drafts,
  reactions, the menu or a progress card, the 20-second gap, preview off, no
  link token, failures that never raise and never log the topic or the text,
  an app turn's reply that the app reads (no post) and one it does not read
  (a post), a watch alert, and the CLI. Fake posters only; no network.
- `ios/EveLink/Tests/run.sh`: the Swift topic equals the Python vector, it
  differs from the rendezvous topic, and `evelink://chat` routes to the Chat.
