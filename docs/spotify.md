# Spotify

eve controls your Spotify from Telegram, the chat window, a Mini App call,
the Voice PE and the robot's voice. It has the same controls as
[spotKnob](https://github.com/gurul/spotify-knob), the round Spotify dial:
now playing, play and pause, next and previous, volume, and moving the music
to another Spotify Connect device. eve adds two things a dial doesn't need:

- **Playing something by name.** "Play Daft Punk on Spotify" searches
  Spotify and plays the top result.
- **Waking the Mac's Spotify.** If no Spotify device is online when you ask to
  play, eve opens Spotify on this Mac in the background. It waits up to 12 s
  for Spotify to come online, then plays there.

eve talks to the Spotify Web API directly, as spotKnob does.
Controlling playback needs **Spotify Premium**. A free account can read what is
playing, but cannot pause, skip or set the volume.

The code is `bridge/src/eve_bridge/spotify.py`.

## Setup: log in once

You need a Spotify app from the
[Spotify Developer Dashboard](https://developer.spotify.com/dashboard). If you
already set up spotKnob, **use its app**: eve uses the same redirect URI.

1. In the app's settings, enable **Web API**. Set the Redirect URI to exactly:

   ```
   http://127.0.0.1:8888/callback
   ```

2. On the Mac that runs eve:

   ```bash
   eve-bridge spotify login --client-id YOUR_CLIENT_ID
   ```

   A browser opens on Spotify's consent page. Accept it, and the terminal says
   `Logged in.`

3. Restart the daemon so the chat, the voice and the Mini App can use
   Spotify:

   ```bash
   launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon
   ```

The login uses the authorization-code flow with **PKCE**, so eve doesn't
need the app's client secret. If you want to use a secret anyway, pipe it in
with `--secret-stdin`. The login asks for three scopes:
`user-read-playback-state`, `user-modify-playback-state` and
`user-read-currently-playing`.

The login is stored in `~/.config/eve-bridge/spotify.json` (mode 600).
The file holds the client id and the refresh token and is never in the repo.
Spotify rotates the refresh token, and eve saves the new one each time.
`eve-bridge spotify logout` deletes the file.

### Why the login runs on the Mac

Spotify accepts plain HTTP only for a loopback redirect such as
`127.0.0.1`. The login runs a small server on that address, receives the one
redirect, and closes. Refreshing the access token later doesn't use the
redirect, so the daemon never needs a browser again.

## What you can say

A message that contains **only** a music command is handled by code, with no
model turn. eve replies with one line, such as "Paused." or "Skipped. Now
One More Time by Daft Punk."

| You say | What happens |
|---|---|
| "pause the music", "stop spotify", "music off" | Pause |
| "play music", "resume spotify", "unpause the music" | Resume. If nothing is active, it starts on a device that is online. |
| "next song", "skip this song", "skip this" | Next track, and the line names it |
| "previous track", "last song" | Previous track |
| "music volume 40", "spotify volume to 45%", "set the volume of the music to 30" | That volume |
| "turn the music up", "music quieter" | Up or down 10% |
| "what's playing", "what song is this", "now playing" | The track, the artist and the device |
| "play the music on the kitchen speaker", "move spotify to my phone" | Move playback to that device |
| "play Daft Punk on Spotify", "play Blinding Lights on Spotify" | Search, then play the top track |

A single word such as "pause", "skip" or "volume 40" is **not** handled as a
music command. It could mean a task, or the Mac's own volume, so it goes to
the model. A sentence with anything more also goes to the model, for example
"pause the music in ten minutes" or "put on something chill". The model has
two tools:

- `spotify_now_playing`: the track, artists, album, playing or paused, the
  volume, the device, and the Connect devices that are online.
- `spotify_control`: `play` (resume, or play something by name, with a kind:
  track, artist, album or playlist), `pause`, `next`, `previous`, `volume`
  (a level, or a change such as +10), and `transfer` (to a device by name).

The robot's live voice gets the same two tools, except during a lesson.

### Naming a device

eve finds a device by its full Spotify name ("Kitchen Speaker"), then by a
word of its name ("kitchen"), then by its kind: "mac", "computer", "phone",
"speaker", "tv" or "tablet". Spotify marks some devices as restricted, and the
Web API refuses to control them. eve lists these devices but doesn't select
them. The Mini App shows them with the label **no remote control**.

## Spotify mode on the Voice PE

Say "spotify mode" (or tap the Mini App's toggle) and the Voice PE works like
spotKnob: its ring turns green and its dial becomes the Spotify volume. 1
click plays or pauses, 2 clicks skip, 3 clicks go back, and a hold opens a
device picker on the dial. Holding for 3 s leaves the mode. See
[Spotify mode](voice-pe.md#spotify-mode).

| You say | What happens |
|---|---|
| "spotify mode", "spotify mode on", "enter spotify mode" | Spotify mode on |
| "spotify mode off", "exit spotify mode", "turn off spotify mode" | Spotify mode off |

The model has these as `mode_on` and `mode_off` in `spotify_control`.

## The Mini App's Music card

When Spotify is logged in, the Mini App's home page has a **Music** card,
just under Lights. It shows:

- The album art, track, artist and album.
- ⏮ ▶/⏸ ⏭ buttons.
- A **Volume** slider. It sends the value when you let go.
- A **Playing on** list of your Connect devices. Pick a device to move the
  music there.
- A key of each device's colour, as the Voice PE's ring shows it in the
  picker.
- **Voice PE Spotify mode**: on or off. It appears only when a Voice PE is
  connected as eve's controller.

The card calls `/api/spotify`, which only opens with the owner's signed
`initData`. See [the Mini App](stackchan/miniapp.md#music).

## From the terminal

```bash
eve-bridge spotify status             # what is playing, and every device
eve-bridge spotify devices
eve-bridge spotify play               # resume
eve-bridge spotify play daft punk --kind artist --device kitchen
eve-bridge spotify pause | next | previous
eve-bridge spotify volume 40
eve-bridge spotify transfer kitchen speaker
eve-bridge spotify logout
```

## Turning it off

Set `EVE_SPOTIFY=0` in `~/.config/eve-bridge/env`. With no login,
Spotify is off too, and eve gives the model no Spotify tools.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| "Spotify refused the login (invalid_grant)" | The refresh token was revoked, for example from your Spotify account's Apps page. Run `spotify login` again. |
| "Controlling playback needs Spotify Premium." | The account is on the free plan. |
| "No Spotify device is online." | No phone, computer or speaker has Spotify open, and this Mac has no Spotify app to open. |
| "Spotify says slow down" | You hit the rate limit. eve sends one volume write for each request, but a script that loops can still hit it. |
| `Port 8888 is busy` at login | Something else is listening on 127.0.0.1:8888. spotKnob's `get_refresh_token.py` uses the same port. |
| `INVALID_CLIENT: Invalid redirect URI` in the browser | The app's Redirect URI is not exactly `http://127.0.0.1:8888/callback`. |
