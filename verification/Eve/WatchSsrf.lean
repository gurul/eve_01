/-
  Where the watcher's requests connect (bridge/src/eve_bridge/watch.py). Line numbers are those of watch.py
  as reviewed (1792 lines, before the fix). The property the module docstring states ("URLs to private,
  loopback or link-local addresses are refused, redirects included") and `render`'s ("a page cannot point the
  browser at the LAN"):

    every connection the watcher opens, by urllib or by the headless browser, goes to a public address.

  How the code checks it. urllib (`http_request`, :644-674): `check_url` (:598-610) resolves the host with
  `_public` (:580-595, `socket.getaddrinfo`) and refuses a non-public answer; then `opener.open` connects, and
  http.client resolves the host AGAIN (`socket.create_connection`). A redirect is checked the same way, by
  `_GuardedRedirects` (:613-621), with its own two lookups. The browser (`render`, :742-792): the first URL
  passes `check_url`; each request the page makes passes `guard` (:757-765), whose `_public` answer is cached
  per host, and Chromium then resolves the host itself. But `page.route` is not called for a redirect's next
  hop (nor for a WebSocket or a popup's requests, which the replay test also shows), so such a hop is
  connected with no check at all.

  Abstraction. DNS is the attacker's: each lookup of a name may answer a public (`pub`) or a private (`priv`)
  address, the check's lookup and the connect's lookup independently (a rebinding name with TTL 0). An IP
  literal answers the same to both, which is a special case. Events: `fetch c k` is one urllib request or
  redirect hop whose check's lookup answers `c` and whose connect's answers `k`; `browse c k` a browser request
  `guard` sees; `unguarded k` a browser connection `guard` never sees (a redirect hop, a WebSocket, a popup),
  whose connect answers `k`. The ghost `bad` records a connection to a private address.

  The fix: urllib connects through `_connect_public`, which resolves once and refuses unless every answer is
  public, then connects to one of those very addresses (the check and the connect are one lookup). The browser
  goes through a guard that sees every connection (a local proxy that makes the same check on the address it
  connects to; `--proxy-bypass-list=<-loopback>` so loopback is not exempt), so `unguarded` becomes `browse`.
  Both fixes are in watch.py (2026-09-25): `_connect_public` behind `_PinnedHTTP(S)Handler` for urllib, and
  `_GuardProxy` for the browser (Chromium launched with it as its proxy and `<-loopback>` as the bypass list,
  in a child process with a deadline). The Python replays: test_watch_security.py's rebinding test, and the
  live browser test, whose negative control (the same page with the guard letting everything through) reaches
  the LAN by beacon, fetch, iframe, popup, redirect and WebSocket.
  Bound: none. The step lemma is checked on both states and all 10 events; `fixed_invariant` then holds for
  every trace of any length, by induction.

  Since this model (re-verification, 2026-09-25): "public" also excludes every address inside a prefix on one of
  this Mac's own interfaces (`_local_networks`, from ifconfig; a home network's IPv6 devices have global
  addresses), and no global IPv6 address at all when the interfaces cannot be read. The proxy carries one request
  per plain-http connection (a kept-alive connection used to carry the next request to the first server). A
  render past its deadline is killed with every process under it: Chromium runs in a process group of its own.
  The Chrome-like read (`tls_request`, curl_cffi, 2026-09-26) is a `fetch` of the fixed kind: `_pin` resolves
  once and refuses unless every answer is public, libcurl is pinned to that address (CURLOPT_RESOLVE, so it makes
  no lookup of its own), its reported primary IP must equal the pin, and eve follows each redirect itself as a
  new hop through the same check. So the model's `Fix.step` for `fetch` covers it unchanged.
-/
namespace Eve.WatchSsrf

inductive Addr where
  | pub
  | priv
  deriving DecidableEq, Repr

inductive Event where
  | fetch (check connect : Addr)
  | browse (check connect : Addr)
  | unguarded (connect : Addr)
  deriving DecidableEq, Repr

structure S where
  bad : Bool := false       -- ghost: a connection reached a private address
  deriving DecidableEq, Repr

def Spec (s : S) : Bool := !s.bad

/-- A connection to `a`. -/
def conn (s : S) (a : Addr) : S := { s with bad := s.bad || a == .priv }

/-! ## The current code -/

def Cur.step (s : S) : Event → S
  -- check_url's lookup refuses a private answer (:649-651, :618-620); else the connect's own lookup is used
  | .fetch c k => if c == .priv then s else conn s k
  -- guard's lookup (:763-765), then Chromium's own
  | .browse c k => if c == .priv then s else conn s k
  -- page.route is never called: nothing is checked
  | .unguarded k => conn s k

def Cur.run (evs : List Event) : S := evs.foldl Cur.step {}

/-- DNS rebinding: the name answers a public address to check_url and 127.0.0.1 to the connect. The Python
replay (`test_dns_rebinding_between_the_check_and_the_connect_is_refused`) reaches a loopback server this way. -/
theorem current_violates : Spec (Cur.run [.fetch .pub .priv]) = false := by
  decide

/-- A public page that redirects the headless browser to a LAN address: no lookup is checked at all
(`test_the_render_guard_covers_redirects_websockets_and_popups`). -/
theorem current_violates_browser_redirect : Spec (Cur.run [.browse .pub .pub, .unguarded .priv]) = false := by
  decide

/-! ## The fixed code -/

def Fix.step (s : S) : Event → S
  -- _connect_public: the address connected to is the address checked
  | .fetch _ k => if k == .priv then s else conn s k
  -- the proxy checks the address it connects to, for every connection the browser makes
  | .browse _ k => if k == .priv then s else conn s k
  | .unguarded k => if k == .priv then s else conn s k

def Fix.run (evs : List Event) : S := evs.foldl Fix.step {}

theorem inv_step : ∀ (s : S) (e : Event), Spec s = true → Spec (Fix.step s e) = true := by
  intro s e
  rcases s with ⟨b⟩
  cases e with
  | fetch c k => cases b <;> cases c <;> cases k <;> decide
  | browse c k => cases b <;> cases c <;> cases k <;> decide
  | unguarded k => cases b <;> cases k <;> decide

theorem inv_run (evs : List Event) : ∀ s : S, Spec s = true → Spec (evs.foldl Fix.step s) = true := by
  induction evs with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (inv_step s e h)

/-- For every trace of urllib requests, redirects and browser connections, whatever the attacker's DNS
answers to each lookup: no connection reaches a private address. -/
theorem fixed_invariant (evs : List Event) : Spec (Fix.run evs) = true :=
  inv_run evs {} (by decide)

end Eve.WatchSsrf
