//! Port of `lite_web.py`: eve lite's two web doors, the Mac's own page on loopback ([`LocalServer`]) and the LAN
//! door phones and laptops use ([`LanServer`]) (docs/plans/eve-lite.md, M4).
//!
//! The owner, 2026-10-13: "make the eve app a web app so that it can just be accessed from same wifi through web
//! rtc". The web app (ios/EveLink/Web/src/web, built into `web_lite/`) is served by two listeners:
//!
//! * **LocalServer**: `http://localhost:48780` (else the first free of 48782-48789), on 127.0.0.1 and ::1 only,
//!   plain HTTP: localhost is a secure context, so the Mac's browser may use the microphone. Always on. It holds
//!   the setup (the two sign-ins), the Mac's own settings (adding a phone, the devices, the LAN switch) and the
//!   chat, which the Mac's page fetches over loopback.
//! * **LanServer**: `https://<LocalHostName>.local:48781` (else 48790-48799), on every interface, IPv4 and IPv6. On
//!   from the first "Add a phone" (remembered), off from Settings. A connection is judged before TLS: a peer that
//!   is not loopback, private, link-local, ULA or on one of the Mac's own links (lan_net.peer_allowed) is dropped
//!   unread. Then TLS with the Mac's own certificate (lite_tls), TLS 1.2 or later. It holds pairing, the chat (for a
//!   phone whose WebRTC link is down), WebRTC signaling and the CA downloads; never the setup or the Mac's
//!   settings.
//!
//! Every request is checked in this order, and refused at the first that fails:
//!
//! 1. `Host` is one of this door's names with its port (421): `localhost`, `127.0.0.1` and `[::1]` here; the
//!    certificate's names and the Mac's current LAN addresses there. A rebound DNS name stops here.
//! 2. The path is one of this door's routes (404), and the method one of the route's (405).
//! 3. A request that is not a GET carries an `Origin` equal to the page's own origin (403), no chunked body (411),
//!    and a JSON body, or raw bytes for an upload (415): a page elsewhere cannot send either without a preflight,
//!    which is never answered. A GET is not asked for `Origin`: browsers leave it off a same-origin GET.
//! 4. The device cookie (401): `eve_mac` here, `eve_dev` there (web_auth). A browser keeps a cookie per host and
//!    not per port, so each door reads only its own name. A cookie the devices no longer know is cleared.
//!
//! The routes (spec M4) are the constants below. The ones without a cookie: the page itself (`/`, `/assets/*`),
//! `/healthz`, the Mac's one-time login code (`POST /api/mac/login`, from `eve-bridge web open`), the two OAuth
//! callbacks (checked by their single-use `state` alone, since a SameSite=Strict cookie is not sent on the
//! provider's redirect back: onboarding), pairing (`POST /api/web/pair`, the open window's secret or 6-digit code)
//! and the CA downloads.
//!
//! * **The chat** goes to the unchanged LinkChat (link_chat). `/api/web/link/{turn,messages,upload,file/<id>}` are
//!   passed to `LinkChat::serve` on the real stream with `Origin` and the cookie removed and `Authorization: Bearer
//!   <a nonce of this process>`, the only token it is given to check; every other `/api/web/link/` path is a 404 at
//!   this door. The iOS app's rule stays: any Origin on `/api/link` is still refused there.
//! * **Sign-ins.** `/onboard/<provider>/start` sends the browser to OpenRouter or Composio with a fresh `state`; the
//!   callback finishes the exchange, writes the credential (the env file, or composio-connect.json), sends the
//!   browser back to the setup page, and asks the daemon to restart (`Parts::restart`) RESTART_DELAY_SECS later,
//!   once the page has loaded and can show that it is waiting. A callback that does not finish answers onboarding's
//!   status with a short page, and the setup page shows the same line (`error`).
//! * **Pairing.** `POST /api/web/pair/open` (the Mac's page) opens the LAN door, raises macOS's Local Network alert
//!   on purpose (lan_net), reissues the certificate when the Mac has a new name or address, and opens a 5-minute
//!   window: two links with the secret in the fragment (`.local` for Apple devices, the IP for the rest), each as a
//!   QR code drawn here (SVG data URIs, byte for byte segno's: the page needs no npm dependency) and a 6-digit code.
//!
//! Files: `web.json` (0600) keeps the ports each door last had (so the address and bookmarks stay put), the LAN
//! switch and the setup's Composio skip. Responses carry the spec's security headers, and the API's
//! `Cache-Control: no-store`. There is no access log: lines carry counts, ports and exception types only (AGENTS
//! rule 10), never a path, an address, a name, a cookie or a code.
//!
//! This is eve lite's only: `start` refuses outside EVE_PROFILE=lite, and nothing in full eve uses this crate.

pub mod lite_web;
mod qr;

pub use lite_web::*;
