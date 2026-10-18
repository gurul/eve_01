# Fire TV

eve controls a Fire TV smart TV from Telegram, the chat window, a Mini App
call, the Mini App's remote card, the Voice PE and the robot's voice. It can:

- Turn the TV on and off, and say what is on it.
- Set the volume, step it up or down, and mute or unmute.
- Play, pause, skip and seek in the app that is playing.
- Press the remote's d-pad: up, down, left, right, select, back, home and
  menu.
- Open an app by name: YouTube, Netflix, Prime Video, Disney+, Hulu, Live TV
  and others.
- Search YouTube on the TV, and type into the search box the TV has focused.
- Take a picture of the screen.

eve talks to the TV with **network ADB**, the Android debugging bridge that
Amazon documents for Fire TV. It needs no cloud, no hub and no app on the TV.
eve runs Google's `adb` command on the Mac. No Python ADB library is added.

The code is `bridge/src/eve_bridge/firetv.py`.

## Setup on the TV

1. Open **Settings > Device & Software (older menus: My Fire TV) > About**. On some TVs this menu is called
   **Device & Software**. Select the device name 7 times. The TV says you are
   now a developer.
2. Go back to **Settings > Device & Software > Developer Options** and turn on
   **ADB Debugging**.
3. Open **Settings > Display & Sounds > Power Controls** and turn on
   **Wake on Network**, so the TV stays reachable in standby. (Some models
   call it **Voice Commands When TV Screen is Off** or **Alexa Anytime**.)
   Verified 2026-10-04 on an Insignia Fire TV with it on: "turn on the TV" woke
   the TV from standby 37 s after it went to sleep. A wake after the chip
   suspends (about 6 minutes into standby) is not verified yet.

The first time the Mac connects, the TV shows **Allow USB debugging?** Tick
**Always allow from this computer**, then select **Allow**. If you don't allow
it, the TV refuses the connection.

## Setup on the Mac

1. Install Google's platform tools, which include `adb`:

   ```bash
   brew install --cask android-platform-tools
   ```

2. Pair with the TV. The TV must be on.

   ```bash
   eve-bridge firetv pair
   ```

   eve looks for the TV on the network with `adb mdns services`, connects,
   checks that the TV allows this Mac, and saves it. If eve finds no TV, or
   finds more than one ADB device, give the TV's address:

   ```bash
   eve-bridge firetv pair --host 192.0.2.10
   ```

3. Restart the daemon so the chat and the voice can use the TV:

   ```bash
   launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon
   ```

4. Reserve a DHCP lease for the TV in your router, so its address stays the
   same. If the address changes anyway, eve finds the TV again by its mDNS
   name (see [Troubleshooting](#troubleshooting)).

The pairing is stored in `~/.config/eve-bridge/firetv.json` (mode 600). It
holds the TV's address and port, its mDNS name (`adb-<serial>`) and its model.
The key that the TV allowed is `adb`'s own, in `~/.android/adbkey`.
`eve-bridge firetv forget` deletes the pairing file.

## What you can say

A message that contains **only** a TV command, and **names the TV** ("tv",
"the tv", "fire tv", "television", "telly"), is handled by code, with no model
turn. eve replies with one line, such as "TV on — YouTube." or "TV volume
30%."

| You say | What happens |
|---|---|
| "turn on the tv", "wake up the tv", "tv on" | Wake the TV |
| "turn off the tv", "tv off", "put the tv to sleep" | Put the TV in standby |
| "what's on the tv", "is the tv on", "tv status" | Power, the app in front, the volume |
| "tv volume 30", "set the volume on the tv to 30%" | That volume |
| "tv volume up", "tv louder", "turn the tv down" | Up or down 10% |
| "turn the tv up a bit", "tv quieter a little" | Up or down 5% |
| "mute the tv", "unmute the tv" | Mute or unmute |
| "pause the tv", "resume the tv", "play the tv" | Pause or play |
| "go home on the tv", "go to the home screen on the tv" | Home |
| "tv down 3 times", "press select on the tv", "tv back" | Press the key, up to 10 times |
| "open youtube on the tv", "open crunchyroll on the tv" | Open that app (any name) |
| "put netflix on the tv", "watch hulu on the tv" | Open that app (only an app eve knows) |
| "search lofi beats on youtube on the tv", "search youtube for cat videos on the tv" | Search YouTube on the TV |

These go to the model instead:

- A word with no "tv", such as "pause", "volume 40" or "turn it up". It could
  mean the music, the Mac or a task. Bare "stop" and "cancel" stop the running
  task.
- A sentence with anything more, such as "turn on the tv in ten minutes".
- "Play X on the tv" when X is not an app eve knows, such as "play daft punk
  on the tv".
- "Play music on the tv" and "put the music on the tv". These stay with
  Spotify, which moves the music to the TV's Spotify app as a Connect device.
  See [Spotify](spotify.md).
- Typing, next, previous, rewind and fast-forward.
- "What's on tv" with no "the" or "my". People usually ask that about TV
  listings, not about this TV.
- "Tv up" and "tv down" with no count. They probably mean the volume. "Tv down
  3 times" and "press up on the tv" press the d-pad.

The model has three tools:

- `firetv_status`: power (on, screensaver or standby), the app in front, the
  volume in percent and whether it is muted.
- `firetv_control`: `power_on` and `power_off`; the d-pad (`up`, `down`,
  `left`, `right`, `select`, `back`, `home`, `menu`, with a repeat of 1 to
  10); the media keys (`play_pause`, `play`, `pause`, `next`, `previous`,
  `rewind`, `fast_forward`); `volume` (a level or a change), `mute` and
  `unmute`; `open_app` by name; `youtube_search`; and `type_text` into the box
  the TV has focused.
- `firetv_screenshot`: a picture of the screen, sent to you. The model does not
  see the picture. It gets back what the TV reports at the same moment: power,
  the app in front and the volume. eve deletes the picture file after it is
  sent.

The robot's live voice gets `firetv_status` and `firetv_control`, but not the
screenshot: the picture goes to a chat, and a voice conversation has none. The
voice's instructions do not mention the screenshot.

When you ask for an app while another app is playing, eve opens it and says
what it interrupted ("Opened Netflix — YouTube was on."). Opening an app or
searching YouTube wakes the TV first if it is in standby.

### The apps eve knows by name

YouTube, Netflix, Prime Video, Disney+, Hulu, Peacock, Spotify, ESPN, NBC
Sports, NFL, NBA, Fox Sports, Amazon Music, Photos, Silk, Appstore, Settings
and Live TV. Each one is opened with its exact activity on the TV. "Open X on
the tv" also works for an app not in this list: eve looks for an installed
package whose name contains X, and asks you to choose if more than one
matches.

## Mini App remote

When the TV is paired, the Mini App's home page has a **TV** card under Music.
It is a remote for the phone:

- A status line at the top, such as "TV on — YouTube, volume 44%".
- A **power** button. It is red when the TV is on; a tap turns the TV off,
  and another tap turns it on.
- A row of app chips from the table above: YouTube, Netflix, Prime Video,
  Disney+, Hulu, Spotify and the others. Swipe the row for more. The app in
  front is highlighted. A tap opens the app.
- A round **d-pad** with **OK** in the centre.
- **Back**, **Home** and **Menu**.
- **Rewind**, **Play/Pause** and **Fast-forward**.
- **Volume −**, **Mute** and **Volume +** (10% a tap), and a **Volume**
  slider. The slider sends the volume when you pause while dragging and when
  you let go.

A button reacts at once with a light haptic tap. The TV takes about a second
for a key, so taps wait in line in the order you made them. Taps on the same
d-pad key while one is on its way go to the TV as one press with a count, in
one `input` call. After each tap the TV's state is read again, and the card
shows what the TV reports, not what was asked. If the TV refuses, or eve
can't reach it, the card shows eve's line in red with an error haptic. A
d-pad press that worked shows no line: the screen is the answer.

The card calls `/api/firetv` (`FireTV.panel`), which only opens with the
owner's signed `initData`. An app's token never reaches it. The card can't
type or search YouTube: those stay in the chat. Without a pairing, or with
`EVE_FIRETV=0`, the card does not appear. See
[the Mini App](stackchan/miniapp.md#tv-remote).

## From the terminal

```bash
eve-bridge firetv pair [--host 192.0.2.10] [--port 5555]
eve-bridge firetv status              # power, the app in front, the volume
eve-bridge firetv on | off
eve-bridge firetv key down --times 3  # up, down, left, right, select, back, home, menu,
                                           # play_pause, play, pause, next, previous,
                                           # rewind, fast_forward, stop
eve-bridge firetv volume 30 | up | down
eve-bridge firetv mute | unmute
eve-bridge firetv open prime video
eve-bridge firetv apps                # the apps eve knows that are on the TV
eve-bridge firetv youtube lofi beats
eve-bridge firetv type hello world
eve-bridge firetv screenshot [--out shot.png]
eve-bridge firetv forget
```

## How it works

eve connects to the TV's ADB on port 5555 and runs one `adb shell` command
for each action. Before the first command, and after a failure, eve runs
`adb connect` and then `adb get-state`, which must say `device`. `adb connect`
always exits with 0, so eve reads its words to know what happened.

Each command was chosen by what it costs on the TV. These times were measured
on the TV on 2026-10-04:

| What | Command on the TV | Time | Read back |
|---|---|---|---|
| State | `dumpsys power`, `dumpsys window` and the `dumpsys audio` music block, in one call | ~0.5 s | It is the read |
| Play, pause, next, ... | `cmd media_session dispatch play-pause` and others | ~75 ms | No |
| Volume | `cmd media_session volume --stream 3 --set N` | ~75 ms | Yes |
| Open an app | `am start -n package/activity` | ~120 ms | No |
| YouTube search | `am start` with a YouTube results link, pinned to the YouTube app | ~120 ms | No |
| Mute, unmute | `input keyevent 164` | ~1.2 s | Yes |
| D-pad, back, home | `input keyevent 20 20 23` | ~1.2 s | No |
| Power | `input keyevent 224` (wake) or `223` (sleep) | ~1.2 s | Yes, polled for 5 s |
| Type text | `input text` | ~1.2 s | No |
| Screenshot | `adb exec-out screencap -p` | ~2.3 s | It is the picture |

`input` costs about 1.2 s because it starts a Java VM on the TV each time. So
eve uses `cmd media_session` and `am start` wherever they can do the job,
and uses `input` only where nothing faster exists. Several presses of a key
go in one `input` call: "down three times" costs one VM start, not three.

Three choices that are not obvious:

- **Mute** uses `input keyevent 164`, the volume-mute key. `cmd media_session
  dispatch mute` sends the *microphone* mute key, which never reaches the
  speaker. Mute is a toggle, so eve reads the TV's mute state first and
  presses the key only when the state is different.
- **Volume** is set as a level, not with the volume keys. eve reads the TV's
  maximum (100 on the TV it was built on) and converts your percent to it.
- **Power** uses WAKEUP and SLEEP, which do nothing when the TV is already in
  that state. The POWER key is a toggle, and eve never sends it.

The state read says **on**, **screensaver** (the TV is on and showing its
screensaver) or **standby**. It also names the app in front: an app's name,
"home", "Settings", "screensaver" or "an HDMI input".

## Power

"Turn on the TV" sends WAKEUP, then reads the state until the TV says it is on.
"Turn off the TV" sends SLEEP, then reads until the TV says it is in standby.

The TV keeps answering ADB for about **6 minutes** after it is turned off. Then
its chip suspends. So:

- From the screensaver, and from a short standby (under about 6 minutes),
  "turn on the TV" works.
- After that, eve can't reach the TV. Whether a network packet wakes it from
  this deep standby is not verified, also with **Voice Commands When TV Screen
  is Off** turned on. eve says the TV is probably in deep
  standby, and to use the remote or ask Alexa.

## Limits

- **No HDMI input switching.** The TV's input switcher needs an
  Amazon-signed permission that the ADB shell does not have.
- **Search is YouTube only.** There is no Netflix or Prime Video search.
- **Typing is plain ASCII**: English letters, digits and punctuation, up to
  200 characters.
- **The d-pad, media keys, opening apps and typing are not read back.** eve
  says what it pressed or opened. When you need to see where it landed, the
  model sends you a screenshot. The model can't see the screen itself.
- **A command that timed out is not sent again** if sending it twice would
  change the result: typing, a run of d-pad keys, mute, play/pause, next,
  previous and the seeks. eve says it doesn't know if the command was done.
  A state read, power, a volume level and opening an app are sent again once.
- One key can be pressed at most 10 times in one command.
- **Projectivy Launcher counts as home.** When the Home button opens
  Projectivy instead of Amazon's home screen, eve still says the TV is on
  "home" (and "Projectivy settings" on its settings screens). Other
  third-party launchers are read out by their package name.

## Privacy

- The pairing file is mode 600 and lives only on the Mac. Nothing about your
  TV (its address, serial or name) is in the repo.
- The log keeps the action, whether it worked and how long it took. It never
  keeps the text you typed, a search or an app's words.
- What the TV reports (app names, the model, errors) is cleaned of control
  characters and shortened before it reaches a model or a log.
- Screenshots are saved under `~/.cache/eve-bridge/firetv/`.

## Settings

Set these in `~/.config/eve-bridge/env`:

| Setting | What it does |
|---|---|
| `EVE_FIRETV=0` | Turns the Fire TV off. With no pairing it is off too, and eve gives the model no TV tools. |
| `EVE_ADB=/path/to/adb` | Uses this `adb`. Without it, eve looks on the PATH, then in `/opt/homebrew/bin` and `/usr/local/bin` (the daemon runs under launchd with a short PATH). |

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| "adb isn't installed on this Mac." | Run `brew install --cask android-platform-tools`, or set `EVE_ADB`. |
| "The TV hasn't allowed this Mac yet." (unauthorized) | The TV did not accept the prompt. On the TV, accept **Allow USB debugging?** and tick **Always allow from this computer**, then try again. |
| "I can't reach the TV. It is probably in deep standby" (offline after standby) | The TV was off for more than about 6 minutes, and its chip suspended. Turn it on with the remote or Alexa. The next command connects again by itself. |
| "I can't reach the TV. Is it on, and on the home network?" | The TV is off, on another network, or its address changed. When a command fails, eve disconnects, looks the TV up by its mDNS name (`adb mdns services`), saves a new address if it finds one, and tries once more. If that fails, run `eve-bridge firetv pair` again. |
| "The Fire TV isn't set up on this Mac yet." | There is no pairing. Run `eve-bridge firetv pair` and restart the daemon. |
| `firetv pair` says "More than one ADB device is on the network." | Choose the TV with `--host`. |
| An app won't open | Amazon sometimes renames an app's activity. eve then asks the TV for the app's launcher activity and tries again. If that fails too, the app has nothing eve can open. |

eve restarts the Mac's ADB server (`adb kill-server`, then `adb start-server`) in one case only: after the
normal recovery fails, the TV's port (5555) still opens to a plain TCP connection. Then the TV is fine and the
server is stale. On 2026-10-08 it had lost its route after the Mac's network changed and said "No route to host"
for hours while the TV was on. eve restarts it at most once every 10 minutes (`ADB_RESTART_EVERY`) and logs
`firetv: the TV's port answers but adb cannot reach it; restarting the adb server`. Otherwise it leaves the server
alone, because another program on the Mac may be using it.
