# The weekly refresh

Two things in a personal eve lapse every 7 days, and both fail quietly:

- **Eve Link.** A free Apple ID signs the app for 7 days. After that the app
  does not open, and the home-screen widget goes with it.
- **The Google sign-in**, while your OAuth app is still in "Testing"
  ([connected accounts](apps.md#set-it-up)). Google drops the sign-in after 7
  days. Calendar, Gmail, Drive and Contacts stop, and the widget comes back
  empty. The daemon log says `Google refused the refresh (invalid_grant)`.

## Refresh both

With the phone unlocked, run this on the Mac:

```bash
sh ios/EveLink/build.sh --install
bridge/.venv/bin/eve-bridge apps login google
```

eve reads the Google sign-in on every call, so a sign-in again works at once,
with no restart (checked 2026-10-26: the calendar came back a minute later).
Only a first-ever sign-in needs `launchctl kickstart -k
gui/$(id -u)/com.github.eve-bridge.daemon`.

## On the widget

The home-screen widget says so before either one lapses, and when Google has
already refused the sign-in: a row with a key at the top of **Needs you**
("Sign in to Google", "Google sign-in ends tomorrow 11:40", "Eve Link stops
opening in 3 h"). It shows from 36 hours before a lapse
(`link_today.SIGNIN_WARN`) and goes once you sign in again or reinstall.

`build.sh --install` removes the cached Eve Link signing profiles before it
builds. Xcode reuses a profile until it expires. Without this step, an install
on day 6 used the old profile and stopped opening the next morning.

The sign-in waits for Google on a local port for 5 minutes. Keep the command
running until the browser shows "eve: signed in". If you stop it first, the
browser page stays blank.

## The reminder

```bash
bridge/.venv/bin/eve-bridge refresh status       # when each one lapses
bridge/.venv/bin/eve-bridge refresh schedule     # remind every Friday at 09:00
bridge/.venv/bin/eve-bridge refresh remind       # send the reminder now
bridge/.venv/bin/eve-bridge refresh unschedule
```

The reminder goes to your phone through [ntfy](notifications.md) and to the
Mac as a notification. It gives both dates and the steps above.

- **App date.** Read from the signing profile of the last build. That build is
  the one `build.sh --install` put on the phone.
- **Google date.** The time of your last `apps login google` plus 7 days. The
  sign-in stamps `signed_in_at` in `google-token.json`.

The LaunchAgent (`com.github.eve-bridge.refresh-reminder`) runs the venv's
Python, the same as the daemon. launchd may refuse to run a `/bin/sh` script
from `~/Documents` ("Operation not permitted"). Its log is at
`~/Library/Logs/eve-refresh-reminder.log`.

Publish the Google app out of "Testing" and the Google half no longer lapses.
The app half lapses as long as it is signed with a free Apple ID.
