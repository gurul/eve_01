# Telegram Bot API: what eve could use

A reference for the Telegram door ([telegram.md](telegram.md)). It lists the
Bot API features that fit a personal assistant bot. For each one it gives the
exact methods and fields, the limits that bite, and one line on eve.

Everything here was read from the official Telegram docs on 2026-09-23, not
from memory. The current version is **Bot API 10.3, released August 24, 2026**
([changelog](https://core.telegram.org/bots/api-changelog)).

Sources, cited per section:

- Bot API reference: https://core.telegram.org/bots/api
- Bot features: https://core.telegram.org/bots/features
- Mini Apps: https://core.telegram.org/bots/webapps
- Working with bots (MTProto): https://core.telegram.org/api/bots/
- Changelog: https://core.telegram.org/bots/api-changelog
- Rate limits: https://core.telegram.org/bots/faq

Local copies of these pages live in [../reference/telegram/](../reference/telegram/).

## What eve uses today

`BotApi` in `bridge/src/eve_bridge/telegram.py` calls these methods and
nothing else:

| Method | What for |
|---|---|
| `getMe` | the token check (`telegram-check`) |
| `getUpdates` | the long poll, `timeout` 50 s, `allowed_updates: ["message"]` |
| `sendMessage` | every line, `parse_mode: "HTML"`, split at 4096; an optional one-time reply keyboard |
| `setMessageReaction` | receipts on the owner's message: 👍 typed or sent, ✍ saved to the vault, 🏆 starred, 👀 an image on its way |
| `sendPhoto` | screenshots, robot photos, browser pictures |
| `sendDocument` | `send_file`, up to 50 MB |
| `sendChatAction` | `typing` once, at the start of an eve turn |

One fact shapes every idea below. `allowed_updates` is `["message"]`, so
Telegram never delivers a `callback_query`, a `message_reaction` or a
`stopped_message_generation` update to eve. `accept()` also drops anything
that is not a `message`. Any button that calls back needs both changed first.

## 1. Keyboards

Source: https://core.telegram.org/bots/api (InlineKeyboardMarkup,
InlineKeyboardButton, CallbackQuery, answerCallbackQuery, ReplyKeyboardMarkup,
KeyboardButton, ReplyKeyboardRemove, ForceReply) and
https://core.telegram.org/bots/features#keyboards.

Telegram has two kinds of keyboard. A **reply keyboard** replaces the phone's
letter keys, and a tap sends the button's text as the user's own message. An
**inline keyboard** sits under one bot message, and a tap sends nothing to the
chat. The bot gets a callback instead.

### Inline keyboard and the callback flow

- Send `reply_markup: {"inline_keyboard": [[button, ...], ...]}` on
  `sendMessage` (or any send method). Rows are arrays of buttons.
- An `InlineKeyboardButton` has `text` and exactly one action field:
  `callback_data`, `url`, `web_app`, `login_url`, `switch_inline_query`,
  `switch_inline_query_current_chat`, `switch_inline_query_chosen_chat`,
  `copy_text`, `callback_game`, `pay`, or (10.3) `disabled`.
- Optional look: `style` is `"danger"` (red), `"success"` (green) or
  `"primary"` (blue). `icon_custom_emoji_id` needs Fragment usernames or a
  Premium owner.
- A tap on a `callback_data` button arrives as an `Update.callback_query`: a
  `CallbackQuery` with `id`, `from`, `message` (the bot's message, as
  `MaybeInaccessibleMessage`), `chat_instance`, and `data`.
- The bot then calls `answerCallbackQuery(callback_query_id, text?,
  show_alert?, url?, cache_time?)`. `text` is 0-200 characters, shown as a
  toast, or as an alert with `show_alert`.
- To change the message after a tap, use `editMessageText` or
  `editMessageReplyMarkup` with `chat_id` and `message_id`.
- 10.3 added `force_reply` to `InlineKeyboardMarkup`: the reply interface opens
  with the keyboard. It can't be changed when the keyboard is edited.

Limits and gotchas:

- `callback_data` is **1-64 bytes**. Store the real payload on the Mac and put
  a short key in the button.
- **Always call `answerCallbackQuery`.** The docs: "Telegram clients will
  display a progress bar until you call answerCallbackQuery. It is, therefore,
  necessary to react by calling answerCallbackQuery even if no notification to
  the user is needed."
- `CallbackQuery.data` can hold data that no button on that message still has
  ("the message originated the query can contain no callback buttons with this
  data"). Treat every callback as untrusted input. Check `from.id` against the
  owner ids, as `accept()` does for messages.
- A button stays on the message after the Mac restarts. A tap on it must be
  answered with "expired", never acted on.
- Only messages with no markup or an inline keyboard can be edited: "it is
  currently only possible to edit messages without reply_markup or with inline
  keyboards." A message sent with a reply keyboard can't be edited.
- `copy_text` copies 1-256 characters to the clipboard.

### Reply keyboard

- `ReplyKeyboardMarkup` fields: `keyboard` (rows of `KeyboardButton`, or plain
  strings), `is_persistent`, `resize_keyboard`, `one_time_keyboard`,
  `input_field_placeholder` (1-64 characters), `selective`, and (10.3)
  `force_reply`.
- `one_time_keyboard` hides the keyboard after a tap, but it "will still be
  available": the user can open it again from the input field.
- `is_persistent` keeps it shown when the letter keys are hidden. Default false.
- `resize_keyboard` fits the height to the rows. Default false, which makes it
  as tall as the phone's own keyboard.
- A `KeyboardButton` can also ask for things, private chats only:
  `request_contact`, `request_location`, `request_users`, `request_chat`,
  `request_poll`, `request_managed_bot`, or open a `web_app`.
- A custom keyboard stays up "until a new keyboard is sent by a bot", unless it
  is one-time.

### ReplyKeyboardRemove and ForceReply

- `ReplyKeyboardRemove`: `{"remove_keyboard": true, "selective"?}`. Takes the
  custom keyboard away for good.
- `ForceReply`: `{"force_reply": true, "input_field_placeholder"?,
  "selective"?}`. Opens the reply box on the bot's message, as if the user
  tapped Reply. The owner's answer then carries `reply_to_message`, so it can't
  be confused with an unrelated text.

For eve: **inline keyboards are in use** (2026-09-23). `BotApi.send_inline`
sends yes/no prompts, AskUserQuestion options and the pickers ("claude on",
"new claude", codex folders) with `callback_data` keys of the form
`<generation>.<n>`; the meaning stays on the Mac (`TelegramInlet._taps`).
`getUpdates` asks for `callback_query`, `accept_tap` checks the owner, and
every tap gets `answerCallbackQuery`. The reply keyboard
(`send_message(buttons=...)`) is now only the fallback for a picker whose
inline keyboard Telegram refuses.
ForceReply is not in use. It would tie a task's answer to its question.

## 2. Reactions

Source: https://core.telegram.org/bots/api (setMessageReaction, ReactionType,
ReactionTypeEmoji, MessageReactionUpdated, getUpdates).

- `setMessageReaction(chat_id, message_id, reaction?, is_big?)`. `reaction` is
  an array of `ReactionType`. Pass an empty array to clear.
- `ReactionTypeEmoji` is `{"type": "emoji", "emoji": "..."}`.
- `is_big: true` plays the big animation.
- The owner's own reactions come in as `Update.message_reaction`, a
  `MessageReactionUpdated` with `chat`, `message_id`, `user`, `date`,
  `old_reaction` and `new_reaction`.

The allowed emoji set is exactly these 73 (copied from the `ReactionTypeEmoji`
field description):

❤ 👍 👎 🔥 🥰 👏 😁 🤔 🤯 😱 🤬 😢 🎉 🤩 🤮 💩 🙏 👌 🕊 🤡 🥱 🥴 😍 🐳 ❤‍🔥 🌚 🌭 💯
🤣 ⚡ 🍌 🏆 💔 🤨 😐 🍓 🍾 💋 🖕 😈 😴 😭 🤓 👻 👨‍💻 👀 🎃 🙈 😇 😨 🤝 ✍ 🤗 🫡
🎅 🎄 ☃ 💅 🤪 🗿 🆒 💘 🙉 🦄 😘 💊 🙊 😎 👾 🤷‍♂ 🤷 🤷‍♀ 😡

Limits and gotchas:

- A bot sets **at most one reaction per message** ("as non-premium users, bots
  can set up to one reaction per message"). A new call replaces the old one.
  That makes a reaction a small status light: 👀 then 👍.
- A tick mark and a cross mark are not in the set. Pick from the list above; a
  common checkmark emoji is rejected.
- Bots can't use paid reactions. A custom emoji reaction needs to be already on
  the message or allowed by chat admins.
- `message_reaction` updates are not sent unless you ask for them by name:
  "must explicitly specify "message_reaction" in the list of allowed_updates".
  The same line says "The bot must be an administrator in the chat". The docs
  don't say how that applies to a private chat, so test it before relying on it.
- Reactions set by bots never produce an update.

For eve: **in use** as receipts (proposal 7, done 2026-09-23). `react()`
puts 👍 on the owner's message when a line reached the Claude terminal
(`TYPED_REACTION`) or steered a running Codex turn, ✍ when a capture went into
the vault (`VAULT_REACTION`), 🏆 when a fact was starred (`STAR_REACTION`), and
👀 while a relayed image downloads (`SEEN_REACTION`), replaced by 👍 once it
was delivered or taken off (`react(..., "")`, an empty list) when it was not.
Every emoji is on the list above, and a test checks them against the local
copy of the docs. `TelegramInlet._receipt` sends the line the reaction stands
for when the reaction fails. A text-brain round of only `capture_note` /
`remember` calls, all ok, ends with the reaction and no second model call.

## 3. Menu button and commands

Source: https://core.telegram.org/bots/api (setMyCommands, BotCommand,
BotCommandScope, setChatMenuButton, MenuButton) and
https://core.telegram.org/bots/features#commands.

- `setMyCommands(commands, scope?, language_code?)`. At most 100 commands.
- `BotCommand`: `command` is 1-32 characters, **lowercase English letters,
  digits and underscores only**. `description` is 1-256 characters. 10.2 added
  `is_ephemeral`, for group commands only the sender sees.
- There are 7 scopes: `BotCommandScopeDefault`, `AllPrivateChats`,
  `AllGroupChats`, `AllChatAdministrators`, `Chat`, `ChatAdministrators`,
  `ChatMember`. `BotCommandScopeChat` with the owner's chat id shows a list to
  that chat only.
- Related: `getMyCommands`, `deleteMyCommands`.
- `setChatMenuButton(chat_id?, menu_button?)` sets the button beside the input
  field. `MenuButton` is `MenuButtonCommands` (opens the command list, the
  default), `MenuButtonWebApp` (`text` plus `web_app`, opens a Mini App) or
  `MenuButtonDefault`.

Limits and gotchas:

- A command can't contain a space. The code word "claude on" can't be a
  command. It has to be `/claude_on`, or `/claude` with an argument.
- "Bot API updates will not contain any information about the scope of a
  command." Always check who sent it, as `accept()` already does.
- Telegram asks every bot to support `/start` and `/help`, and `/settings` if
  it has any.

For eve: **not in use.** eve answers `/start`, `/stop`, `/stealth`,
`/wake`, `/claude on`, `/codex` by matching text. One `setMyCommands` call,
scoped to the owner's chat, would put the code words in the `/` menu:
`/stop`, `/screenshot`, `/rundown`, `/stealth`, `/wake`, `/claude_on`,
`/claude_off`, `/new_claude`, `/codex`, `/mute`, `/unmute`.

## 4. Streaming responses

Source: https://core.telegram.org/bots/api (sendMessageDraft,
sendRichMessageDraft, MessageGenerationStopped, editMessageText,
sendChatAction) and https://core.telegram.org/bots/features#streaming-replies.

### sendMessageDraft (present in the current API)

Added in 9.3, opened to all bots in 9.5, given `can_stop` and `keep_on_stop` in
10.3.

- `sendMessageDraft(chat_id, draft_id, text?, parse_mode?, entities?,
  message_thread_id?, can_stop?, keep_on_stop?)`. Returns True.
- `draft_id` must be non-zero. Updates with the same `draft_id` animate; a new
  id replaces the draft without animation.
- `text` is 0-4096 characters. **Empty text shows a "Thinking…" placeholder.**
- `can_stop: true` shows a Stop button. A tap sends
  `Update.stopped_message_generation` (`MessageGenerationStopped`: `chat`,
  `message_thread_id`, `draft_id`).
- `keep_on_stop: true` keeps the draft on screen after Stop, for a short time.
- `sendRichMessageDraft` does the same with an `InputRichMessage`.

Limits and gotchas:

- **Private chats only** (`chat_id` is "the target private chat").
- A draft is **not a message**. It is "a temporary 30-second preview". The bot
  must finish with `sendMessage` (or `sendRichMessage`) or the text is gone.
- To get Stop taps, add `stopped_message_generation` to `allowed_updates`.

### Editing a sent message

The older way to stream: send a message, then `editMessageText` it as text
arrives.

- `editMessageText(chat_id, message_id, text, parse_mode?, entities?,
  link_preview_options?, reply_markup?, rich_message?)`. `text` is 1-4096
  characters.
- The result is a real message that stays. It can carry an inline keyboard.
- Only a message with no markup or with an inline keyboard can be edited.
- The 48-hour edit limit applies to business messages not sent by the bot. The
  docs give no edit window for the bot's own messages in its own chat.
- The docs give no separate rate limit for edits. The FAQ says: "In a single
  chat, avoid sending more than one message per second ... eventually you'll
  begin receiving 429 errors." Pace edits the same way. A 429 reply carries
  `parameters.retry_after`, the seconds to wait.

### Chat actions

- `sendChatAction(chat_id, action)`. `action` is one of `typing`,
  `upload_photo`, `record_video`, `upload_video`, `record_voice`,
  `upload_voice`, `upload_document`, `choose_sticker`, `find_location`,
  `record_video_note`, `upload_video_note`.
- It lasts **5 seconds or less**, and it clears when the bot's next message
  arrives. Work that takes longer has to send it again every ~4 s.

### Rich messages (10.1+)

- `sendRichMessage(chat_id, rich_message, reply_markup?, ...)`.
  `InputRichMessage` takes exactly one of `html`, `markdown` or `blocks`.
- Rich Markdown "follows GitHub Flavored Markdown where possible": headings,
  tables, task lists, code, LaTeX, collapsible details, media.
- `editMessageText` takes `rich_message` too.

For eve: **only `typing`, sent once per eve turn.** Nothing streams. The
Claude relay batches lines for 1.2 s and sends whole messages, split at 4096
characters (cut only past 16000). `sendMessageDraft` would show Claude's reply as it is written, then
leave one final message. Rich Markdown could carry Claude's GFM almost as
written.

## 5. Mini Apps (Web Apps)

Source: https://core.telegram.org/bots/webapps and
https://core.telegram.org/bots/features#mini-apps.

A Mini App is a web page that opens inside Telegram. It loads
`https://telegram.org/js/telegram-web-app.js` and gets `window.Telegram.WebApp`.

### Launch points

The docs name seven: the main Mini App from the bot's profile, a keyboard
button, an inline button, the menu button, inline mode, a direct link, and the
attachment menu. A chat join request is an eighth path (10.1).

- **Keyboard button** (`KeyboardButton.web_app`): the page sends data back with
  `Telegram.WebApp.sendData`, which arrives as a `web_app_data` service message.
- **Inline button** (`InlineKeyboardButton.web_app`): the page gets `query_id`,
  and the bot answers with `answerWebAppQuery`. Private chats only.
- **Menu button**: `setChatMenuButton` with `MenuButtonWebApp`, or BotFather.
- **Main Mini App**: set in BotFather; opens from `https://t.me/<bot>?startapp`.
- **Direct link**: `https://t.me/<bot>/<appname>?startapp=...&mode=compact`.
- **Attachment menu**: only for major advertisers, or on the test server.

### Validating initData

The page must send `Telegram.WebApp.initData` (a query string) to the backend,
which checks it:

```
data_check_string = all fields except hash, sorted by key, "key=value", joined by "\n"
secret_key = HMAC_SHA256(<bot_token>, "WebAppData")     # "WebAppData" is the key
valid      = hex(HMAC_SHA256(data_check_string, secret_key)) == hash
```

Also check `auth_date`, so old data can't be replayed. A third party without
the token can check the Ed25519 `signature` field instead, with Telegram's
public key and a data-check-string that starts `<bot_id>:WebAppData`.

### Device access

- `LocationManager` (8.0+): `init`, `getLocation`, `openSettings`. Returns
  latitude, longitude, altitude, course, speed and their accuracies.
- `Accelerometer`, `Gyroscope`, `DeviceOrientation` (8.0+): start and stop,
  with a refresh rate.
- `BiometricManager`: ask for Face ID or a fingerprint, and store a token.
- `CloudStorage` (on Telegram's servers), `DeviceStorage` (9.0+, local, up to
  5 MB per user), `SecureStorage` (9.0+, Keychain or Keystore, up to 10 items).
- `HapticFeedback`, full-screen mode, home screen shortcuts, QR scanning,
  file downloads, and more.

Limits and gotchas:

- The page needs a public **HTTPS** URL. eve has no public URL by design:
  "no port to open, no webhook". A Mini App breaks that rule unless the page is
  static and hosted elsewhere, and it still needs a way to reach the Mac.
- Since July 20, 2026, Mini App methods only work from the Mini App's own
  origin, unless the bot opts out in BotFather (10.2).
- Never trust `initDataUnsafe`. Validate `initData` on the server.

For eve: **not in use.** A Mini App could be a desk dashboard: sessions,
task state, a live robot view, and a joystick that turns the head by tilting
the phone (`DeviceOrientation`). It is the largest item here, because of the
HTTPS rule.

## 6. Other features, in short

### Inline mode

Source: https://core.telegram.org/bots/api#inline-mode,
https://core.telegram.org/bots/features#inline-requests.

Users type `@botname query` in any chat. The bot gets `inline_query` and
answers with `answerInlineQuery` (at most 50 results, `cache_time` default
300 s, `is_personal`). Must be enabled in BotFather.

For eve: not in use. It could paste a vault note or a fact into another chat.
That breaks the "owner's private chat only" rule, so it is low priority.

### Guest mode (10.0)

Source: https://core.telegram.org/bots/features#guest-bots.

A mentioned bot gets one `guest_message` update and replies once with
`answerGuestQuery`, without joining the chat.

For eve: not in use, and it is against the private-chat rule.

### Polls and quizzes

Source: https://core.telegram.org/bots/api#sendpoll.

`sendPoll(question 1-300 chars, options 1-12, type "regular" | "quiz",
is_anonymous, allows_multiple_answers, correct_option_ids, open_period
5-2628000 s, close_date, ...)`. 10.0 lowered the minimum options to 1 and added
media.

For eve: not in use. A non-anonymous poll could pick among several choices,
but an inline keyboard does that with less machinery.

### Checklists

Source: https://core.telegram.org/bots/api#sendchecklist.

`sendChecklist` and `editMessageChecklist` send a list of 1-30 tasks. **Both
require `business_connection_id`**: a bot sends a checklist only on behalf of a
connected business account, not in its own chat.

For eve: not in use, and not usable for vault todos in eve's own chat. Use
an inline keyboard of toggles, or a rich message task list, instead.

### Live location

Source: https://core.telegram.org/bots/api#sendlocation.

`sendLocation` with `live_period` 60-86400 s (or `0x7FFFFFFF` for no end),
`heading`, `proximity_alert_radius`. Update it with `editMessageLiveLocation`,
end it with `stopMessageLiveLocation`. A `KeyboardButton` with
`request_location` asks the owner for theirs.

For eve: not in use. "Where am I" or a reminder by place would need the
owner's location, from `request_location`.

### Games

Source: https://core.telegram.org/bots/api#games.

`sendGame` with a game made in BotFather. A `callback_game` button must be the
first button in the first row.

For eve: not in use, no need.

### Payments, Stars, paid media

Source: https://core.telegram.org/bots/api#payments,
https://core.telegram.org/bots/features#payments.

`sendInvoice` with `currency: "XTR"` and an empty `provider_token` takes
Telegram Stars; `prices` must have exactly one item. Digital goods must be sold
in Stars. `sendPaidMedia` locks media behind 1-25000 Stars.

For eve: not in use, no need.

### Group moderation

Source: https://core.telegram.org/bots/api (banChatMember, restrictChatMember,
deleteMessage).

The bot must be an admin with the right permission. `deleteMessage` works on
messages under 48 hours old; a bot can delete its own messages and incoming
messages in private chats.

For eve: not in use. `deleteMessage` could tidy eve's own chat, such as a
permission prompt once it is answered.

### Business account delegation

Source: https://core.telegram.org/bots/features#business-bots.

With Secretary Mode, a user connects the bot to their account. The bot gets
`business_connection`, `business_message` and more, and acts under the rights
in `BusinessBotRights` (for example `can_reply`, in chats with incoming
messages in the last 24 hours). 10.0 removed the need for Premium.

For eve: not in use. It would let eve read or answer the owner's own
Telegram chats. That is a big trust step, and it would need its own consent
rules.

### Bot-to-bot messaging (10.0)

Source: https://core.telegram.org/bots/features#bot-to-bot-communication.

Enable Bot-to-Bot Communication Mode in BotFather. In private chats a bot sends
to another bot by `@username`; both must have the mode on. Telegram requires
loop safeguards: dedupe, rate limits, a maximum depth.

For eve: not in use. `accept()` drops `is_bot` senders on purpose.

### Topics in private chats

Source: https://core.telegram.org/bots/features#topics-in-private-chats,
https://core.telegram.org/bots/api#createforumtopic.

Turned on in BotFather (`getMe` shows `has_topics_enabled`). `createForumTopic`
works "in a forum supergroup chat or a private chat with a user". Send methods
take `message_thread_id`.

For eve: not in use. One topic per Claude session, per Codex chat, and one
for eve would keep the conversations apart.

### Ephemeral messages (10.2, 10.3)

Visible to one user in a group. For eve: no use, eve only talks in a
private chat.

## 7. How the Claude relay could improve

What the relay does now (`telegram.py` and `daemon.py`):

- `_claude_on` lists sessions from `daemon.state.sessions`, newest first, and
  shows a **reply keyboard** of "claude on <name>" buttons when there are
  several.
- Plain text goes through `_type_to_claude` to `Daemon._type_into_terminal`,
  which raises the session's terminal, waits 0.3 s, and sends the text as
  **System Events keystrokes plus Return**. Success gets a 👍 reaction.
- `_on_assistant_text` (the JSONL tailer) calls `relay_text`. It drops text from
  sessions other than the pinned one, cuts only past 16000 characters (a
  runaway), and queues it; `_say` splits it into 4096-character messages.
  `relay_line` and `_flush_relay` batch 1.2 s of lines into one message.
- `relay_tool_call` forwards only `AskUserQuestion`, with "Reply with the
  option's number."
- `relay_notification` sends "Claude is waiting on you", unless a question went
  out in the last 20 s.
- `decide_permission` sends the command and "yes / no?". The owner's **next
  message** is the answer, read by `consent.decision`. It waits 240 s, then
  leaves the decision to the dialog on the Mac.

### Weaknesses found

1. **Keystrokes go to whatever is frontmost.** `_type_into_terminal` types into
   the focused window after 0.3 s. If focus moves in that time (a
   notification, the owner at the Mac, another app), the text lands somewhere
   else, and eve still reports success with 👍. The success check is only
   "osascript did not fail".
2. **Dead sessions stay in the picker.** A session leaves `state.sessions` only
   on the `SessionEnd` hook. A terminal that is killed or crashes never sends it,
   so it stays in the "claude on" list. Joining it types into whatever window
   is frontmost (weakness 1). *Fixed 2026-09-23:* `claude_live.py` drops a
   session with no running `claude` process in its folder.
3. **A pending yes/no eats the next message.** While `decide_permission` waits,
   any plain text is taken as the answer. A message meant for Claude is not
   typed. `consent.decision` finds no yes or no, the prompt goes back to the Mac
   where nobody is, and the owner's message is lost. *Fixed 2026-09-23:* the
   prompt has Allow/Deny buttons, only a tap or a clear yes/no answers it, and
   other text goes to Claude while it waits. Without buttons (Telegram refused
   them) the old rule holds.
4. **One question at a time.** A second permission while one is pending returns
   `None` at once and goes to the Mac dialog. With the owner away, that session
   stalls until the hook times out.
5. **No word when a prompt times out.** After 240 s the phone is not told that
   the dialog went back to the Mac. *Fixed 2026-09-23:* the prompt is edited to
   say the dialog on the Mac decides, and its buttons go.
6. **Silent drops.** Text from a session other than the pinned one is dropped
   with no sign on the phone.
7. **Long replies are cut.** Anything past 1500 characters became "the rest is
   in the terminal". *Fixed 2026-09-24:* the cap is 16000, and a long reply
   arrives whole, split into 4096-character messages.
8. **No sign of work.** During a relayed turn there is no typing indicator.
   The 👍 means "typed", not "Claude is on it" or "Claude is done".
   *Fixed 2026-09-23:* "typing…" is repeated from the typed line until Claude
   answers, asks, waits or ends its turn.
   *Then 2026-09-23:* a "Thinking…" draft replaces it for the whole turn,
   shown again after each of Claude's messages.
9. **Picker names can collide.** Two sessions in different folders with the
   same name show the same button, and `names.index` picks the first.
10. **Codex progress is one message per event** ("Codex progress"), which can
    hit the one-message-per-second guidance on a busy task. *Fixed 2026-09-23:*
    one progress message per task or Codex turn, edited at most once a second.

### Ideas, ranked by value to the owner

| # | Idea | Bot API feature | Concrete change |
|---|---|---|---|
| 1 | Yes/No buttons on permission prompts | inline keyboard, `callback_query`, `answerCallbackQuery`, `editMessageReplyMarkup` | `decide_permission` sends "Allow" (`style: success`) and "Deny" (`style: danger`) buttons with a short key. The answer comes from the tap, not the next message, which fixes weakness 3. After the tap, edit the message to "Allowed" or "Denied" and remove the buttons. On timeout, edit it to "Left to the Mac" (weakness 5). Needs `callback_query` in `allowed_updates` and an owner check on `from.id`. Several pending prompts can each have their own buttons (weakness 4). |
| 2 | Drop dead sessions from the picker | none (Mac side) | Before showing the picker, check each session's process or transcript age and hide stale ones. Refuse to type when the session's terminal can't be found. Fixes weakness 2. |
| 3 | AskUserQuestion options as buttons | inline keyboard, `callback_query` | Parse the "(1. …" options in the hint into one button each. A tap types the number, as the reply does today. The owner doesn't need to read and type a digit. |
| 4 | Stream Claude's reply | `sendMessageDraft`, then `sendMessage` | While Claude writes, update one draft (empty text shows "Thinking…"). When the turn ends (Stop hook), send the final message. This replaces the 1.2 s batching. The final send is required, because a draft lasts only 30 s. |
| 5 | Typing while Claude works | `sendChatAction` `typing`, repeated every ~4 s | Start after a line is typed in, stop at the Stop hook. Answers "is it doing anything?" (weakness 8). |
| 6 | Code words in the `/` menu | `setMyCommands` with `BotCommandScopeChat` | `/claude_on`, `/claude_off`, `/new_claude`, `/codex`, `/stop`. Needs the dispatcher to accept the underscore forms. |
| 7 | Reaction as a status light | `setMessageReaction` | 👀 when the line is typed, 👍 when Claude's turn ends, 🤔 while a question waits. One reaction per message, so each call replaces the last. |
| 8 | Pinned status message | `sendMessage`, `pinChatMessage`, `editMessageText` | One pinned line: "Relay: eve (joined 14:02) · Codex: off · Stealth: off". Edited in place when anything changes, with inline buttons "Switch" and "Off". |
| 9 | Reply to route | `Message.reply_to_message`, `sendMessage` result | Keep a map from each relayed message id to its session. A reply to a message from session A types into A, whoever is pinned. Also lets relayed text from other sessions show (weakness 6), each under its folder name. `send_message` has to return the sent message ids first. |
| 10 | Pickers as inline keyboards | inline keyboard, `editMessageText` | The "claude on" picker and the "new claude" tree become one message whose buttons change at each step, with Back and Cancel. Button keys carry the full path, so same-named folders are distinct (weakness 9). |
| 11 | One topic per session | topics in private chats, `message_thread_id` | Each Claude session gets its own topic. Text in a topic goes to that session. Needs BotFather to turn topics on. |
| 12 | Long replies whole | `sendRichMessage` with `markdown`, or `sendDocument` | Send Claude's GFM as a rich message (collapsible details for long parts), or the full text as a `.md` file, instead of cutting at 1500 (weakness 7). |
| 13 | Codex progress in one message | `editMessageText` | One "Codex progress" message, edited as events arrive, paced to under one edit per second (weakness 10). |

Weakness 1 (keystrokes into the frontmost window) has no Telegram fix. It needs
a way to write to the session that doesn't depend on focus, such as the
terminal's own scripting interface or a pty the daemon owns.

## 8. How every eve feature could use Telegram's UI better

Each row: what eve does today, the Telegram feature, and what the owner would
see.

| Feature | Today | Telegram feature | What the owner would see |
|---|---|---|---|
| stop | text "stop" or "/stop" | `setMyCommands`; inline "Stop" button on "On it"; `can_stop` on a draft | A Stop button under the task's "On it" message. `/stop` in the menu. |
| stealth / wake | text code words | `setMyCommands`; pinned status | `/stealth` and `/wake` in the menu. "Stealth: on" in the pinned status. |
| screenshot | text match, `sendPhoto` | `upload_photo` chat action; inline "Again" and "As file" buttons | "sending photo…" while it captures; a button to retake, and one to get it full size with `sendDocument`. |
| rundown | text match, one message | `sendRichMessage` (headings, lists); `sendMessageDraft` while it builds; `url` buttons | A structured brief with sections for mail, calendar and todos, shown building up, with buttons that open each item. |
| codex on / off / use | `folder_menu` is a plain text list of names | inline keyboard of folders; `editMessageText` | A list of folder buttons. One tap starts the chat; the menu becomes "Codex chat in eve". |
| new claude tree | reply keyboard per step | inline keyboard edited in place | One message that walks personal/work, then folder, then opens; Back and Cancel at each step. |
| claude on picker | reply keyboard | inline keyboard; pinned status | Buttons for live sessions only, with folder and age. The pinned line then shows which one is joined. |
| start_task | "On it", then a result message | `editMessageText` for progress; `reply_parameters` to the request; inline Stop and Steer | One message that updates ("opening Chrome…", "searching…") and ends as the result, as a reply to the request. |
| steer_task | the model reads the next message | `ForceReply` from a "Steer" button, with `input_field_placeholder` | Tap Steer, the reply box opens with "New instruction…". |
| task questions (ask_user) | the next message is the answer | `ForceReply`; inline buttons when the question has choices; edit on timeout | The reply box opens on the question. On timeout it reads "No answer, taken as no". |
| Codex app-access prompts | typed "yes" / "allow for task" / "always allow" / "no" | inline keyboard | Four buttons, with "No" in red. |
| Composio yes/no (`APP_ASKS_TITLE`) | "yes / no?" and the next message | inline keyboard, `style` | "Allow" and "Deny" buttons under the action. After a tap, the message reads what was decided. |
| Jev risk ask | yes/no in the chat, with Jev's reason | inline keyboard | Same buttons, Jev's reason above them. |
| take_photo | `sendPhoto` | `upload_photo` action; inline "Another" button | "sending photo…", then a photo with a button for one more. |
| look / find / move_head | text in, text out | inline D-pad (◀ ▲ ▼ ▶ ●) on a photo, `editMessageMedia` | A remote camera: tap an arrow, the head moves, the same photo message updates. |
| look_around | a text summary | `sendMediaGroup` | An album, one photo per direction, with the summary as the caption. |
| go_explore | "ok" | `editMessageText` progress; album at the end | A progress line while it explores, then the photos. |
| take_notes | start/stop by text | pinned status; `sendDocument` on stop | "Notes: on" in the pinned line; the notes file arrives when it stops. |
| second brain capture | a reply line | `setMessageReaction` ✍ | A ✍ on the owner's own message instead of a reply. |
| vault todos | text | inline keyboard toggles; rich message task list | A todo list with a button per item to mark it done. (Telegram checklists need a business connection, so they are not an option.) |
| remember (star a fact) | a reply line | `setMessageReaction` 🏆 | A 🏆 on the owner's message. |
| set_sound | text | `setMyCommands` `/mute` `/unmute`; pinned status | Menu commands, and "Sound: off" in the pinned line. |
| web_search | an answer in text | `url` buttons; `link_preview_options` | The answer with a button per source. |
| send_file / list_files | a text list, then a file | inline keyboard of files; `upload_document` action | Tap a file in the list to get it. |
| think_hard | the answer when done | `sendMessageDraft` with empty text | "Thinking…" while the slow brain works. |
| eve turns | `typing` once | `typing` every ~4 s | "typing…" for the whole turn, not just the first 5 s. |
| task result / failure | titled message | `reply_parameters`; inline "Retry" and "Screenshot" | The result as a reply to the request, with a Retry button on a failure. |
| restart notice | "…was stopped. Send it again." | inline "Run again" button | One tap re-runs the stopped task. Needs the goal kept across the restart. |
| images from the owner | download, then answer | `setMessageReaction` 👀 | 👀 on the image while it downloads and is read. |
| status overall | none | pinned message, edited in place | One line at the top of the chat: relay, Codex, stealth, sound, notes, running task. |
| everything, later | none | Mini App from the menu button | A dashboard with sessions, tasks, the camera and a tilt-to-look joystick. Needs HTTPS hosting. |

## 9. All proposals, ranked by value for effort

Value is to the owner. Effort is the change to eve. The first four share one
piece of plumbing: `callback_query` in `allowed_updates`, a dispatch path with
an owner check, `answerCallbackQuery` on every tap, and short keys stored on
the Mac. Once that exists, each later button costs little.

| Rank | Proposal | Area | Value | Effort |
|---|---|---|---|---|
| 1 | `setMyCommands` for the code words | all | medium | low |
| 2 | Drop dead sessions from the picker | relay | high | low |
| 3 | Inline Allow/Deny on permission prompts | relay | high | medium |
| 4 | Inline Allow/Deny on Composio, Jev and Codex app prompts | apps, tasks | high | low after 3 |
| 5 | AskUserQuestion options as buttons | relay | high | low after 3 |
| 6 | Repeated `typing` for relay turns, eve turns and think_hard | relay, chat | medium | low |
| 7 | Reactions as receipts (✍ vault, 🏆 star, 👀 image, 👀→👍 relay). *Done 2026-09-23.* | chat, relay | medium | low |
| 8 | Stop button on a running task | tasks | high | low after 3 |
| 9 | "Thinking…" draft for relayed Claude turns (`sendMessageDraft`) | relay | medium | medium |
| 10 | Task progress in one edited message, result as a reply | tasks, Codex | high | medium |
| 11 | Inline pickers: claude on, new claude, codex folders, files | relay, Codex | medium | medium |
| 12 | Pinned status message | all | medium | medium |
| 13 | ForceReply for task questions and Steer | tasks | medium | low |
| 14 | Reply-to routing between sessions | relay | medium | medium |
| 15 | Rundown and long replies as rich messages | rundown, relay | medium | medium |
| 16 | "Run again" button on restart notices | tasks | low | medium |
| 17 | Album for look_around and explore | robot | low | low |
| 18 | Robot D-pad remote camera | robot | medium (fun) | medium |
| 19 | One topic per session | relay | medium | high |
| 20 | Mini App dashboard | all | high | high |

Proposal 9, as built (owner, 2026-09-23). Only the empty "Thinking…"
placeholder is drafted, never Claude's words. The transcript tailer hands over
whole text blocks, not tokens, so there is nothing to stream inside a block.
Each block already goes as a real message within 1.2 s, and a draft vanishes
when the bot sends a message, so drafting a block first would only show it
twice. A draft that no `sendMessage` followed would also lose the words. What a
draft does add is a bubble that lasts the whole turn: it is sent every 20 s
(a draft lives 30 s) and again after each relayed message, and it ends at the
Stop hook, a question, a wait or `claude off`. There is no `can_stop` button:
stopping a Claude turn would need an Esc keystroke into the frontmost window
(weakness 1). The first refusal turns drafts off for the process and the
relay's "typing…" takes over. `EVE_TELEGRAM_DRAFTS=0` turns it off.

Typing into the frontmost window (weakness 1) is not on this list. It is a Mac
problem, not a Telegram one, but it is the most serious weakness in the relay.

## Next candidates for eve

1. **`setMyCommands`** for the code words, scoped to the owner's chat. One call
   at startup. *Done 2026-09-23.*
2. **Callback plumbing plus inline yes/no** for permission prompts, Composio,
   Jev and Codex app prompts. Stops a pending prompt eating the next message.
   *Done 2026-09-23.*
3. **AskUserQuestion options and pickers as inline buttons.** *Done
   2026-09-23.* The pickers moved to inline buttons because they can't be
   mistaken for a chat message and are taken away once used. A tap feeds the
   same code path as typing the button's words.
4. **`sendMessageDraft`** for relayed Claude output and think_hard, with a
   repeated `typing` action between drafts. *Done 2026-09-23 for the relay,
   as a "Thinking…" placeholder only* (see section 9). think_hard keeps
   "typing…".
5. **Task progress in one edited message**, with a Stop button. *Done
   2026-09-23.* The result is a new message replying to the request, and a
   task's free question uses ForceReply.
6. **A pinned status message**, edited in place.

## What the docs did not confirm

- Whether `message_reaction` updates reach a bot in its **private chat**. The
  docs only say the bot must be an admin and must ask for them by name.
- A rate limit for **edits**, or for `sendMessageDraft` calls. The only number
  is the FAQ's one message per second per chat.
- A maximum length for a **button's text**. eve's 64 (`MAX_BUTTON_CHARS`) is
  its own choice.
- **Checklists** in a bot's own chat. The docs allow them only through a
  business connection.
- An **edit window** for the bot's own messages. The 48-hour rule is stated
  only for business messages not sent by the bot, and for `deleteMessage`.

## Known limits

Found by the review of the Telegram batches on 2026-09-23 and left as they
are for now: each is cosmetic, bounded, or needs a larger change than a fix.

- **"Thinking…" can outlive the turn by up to 30 s.** A draft already on its
  way (a refresh, or the re-show after a relayed message) can land after the
  turn's last message, and nothing can take a draft back. A turn stopped with
  Esc at the Mac sends no Stop hook, so its bubble is refreshed until the
  5-minute cap. With two owner chats, the bubble follows the chat that
  started it. With drafts refused, "typing…" after the re-show can run to its
  cap instead of stopping at Claude's first words.
- **"stop" during a relayed Claude turn** answers "Nothing is running." and the
  bubble comes back: the relay cannot stop Claude from the chat.
- **A relayed slash command that starts no turn** (`/clear`) leaves
  "Thinking…" up to its cap, since no Stop hook follows.
- **Option buttons after a question answered at the Mac** stay live until you
  type, the turn ends or the relay moves; a late tap types its number into
  Claude's prompt.
- **A buttons send that fails partway** (a timeout after Telegram delivered
  it, or a later piece refused) is resent plain, so it can arrive twice. The
  fallback keeps the question from being lost.
- **A task's "Should I go ahead…?" is strict while a relay is on.** A
  qualified reply ("yes, but the cheaper one") goes to the relay, and the
  question waits until its timeout; nothing tells you it is still open.
- **The 👀 is set before an image's download starts**, which adds one
  reaction round-trip to every relayed image.
- **Session liveness is read by folder.** A live `claude` in a folder, or in
  one above it, keeps every session under it in the picker; the hooks carry
  no process id to tell sessions apart.
