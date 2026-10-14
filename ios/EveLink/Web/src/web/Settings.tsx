// Settings (docs/plans/eve-lite.md, M7): Apps, Phone access and Devices (the Mac's own browser only), Trust eve (a
// phone or laptop only), OpenRouter, Help, and signing this browser out. Everything here is the Mac's web door
// (api.ts); nothing is kept in the page.

import { useCallback, useEffect, useState, type ReactNode } from "react";
import {
  api,
  ApiError,
  connectLink,
  noCredit,
  readApps,
  readDevices,
  readPairWindow,
  readStatus,
  type AppsState,
  type Device,
  type PairWindow,
  type WebStatus,
  type Where,
} from "./api";
import { CheckIcon, CloseIcon } from "./icons";
import type { LinkStatus } from "./transport";
import { appleMobile, openPending } from "./windows";

export const ADD_CREDITS = "https://openrouter.ai/settings/credits";

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="eve-settings__section" aria-label={title}>
      <h2 className="eve-settings__heading">{title}</h2>
      <div className="eve-settings__card">{children}</div>
    </section>
  );
}

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : "Something went wrong. Try again.";
}

function ago(seconds: number | null, now = Date.now()): string {
  if (seconds === null) return "";
  const diff = Math.round(seconds - now / 1000);
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  const abs = Math.abs(diff);
  if (abs < 60) return "just now";
  if (abs < 3600) return rtf.format(Math.round(diff / 60), "minute");
  if (abs < 86_400) return rtf.format(Math.round(diff / 3600), "hour");
  if (abs < 30 * 86_400) return rtf.format(Math.round(diff / 86_400), "day");
  return new Date(seconds * 1000).toLocaleDateString();
}

/** Dollars as the page shows them; owing reads as $0.00, as the Mac reports it. */
export function money(usd: number): string {
  return `$${Math.max(0, usd).toFixed(2)}`;
}

// ---- Apps ----------------------------------------------------------------------------------------------------

function Apps({ where, onAuthLost }: { where: Where; onAuthLost(): void }) {
  const [apps, setApps] = useState<AppsState | null>(null);
  const [problem, setProblem] = useState("");
  const [opening, setOpening] = useState("");

  const load = useCallback(async () => {
    try {
      setApps(readApps(await api("GET", "/api/web/apps")));
      setProblem("");
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) onAuthLost();
      setProblem(errorText(e));
    }
  }, [onAuthLost]);

  useEffect(() => {
    void load();
    // Back from a provider's consent page: the list shows what is connected now.
    const onVisible = () => {
      if (document.visibilityState === "visible") void load();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, [load]);

  const connect = async (toolkit: string) => {
    // The tab opens inside the tap (a pop-up blocker allows that), then goes to the Connect Link once it is here.
    const tab = openPending();
    setOpening(toolkit);
    setProblem("");
    try {
      const url = connectLink(await api("POST", `/api/web/apps/${encodeURIComponent(toolkit)}/connect`, {}));
      if (!url) throw new Error("Composio didn't send a sign-in link. Try again.");
      tab.go(url);
    } catch (e) {
      tab.cancel();
      if (e instanceof ApiError && e.status === 401) onAuthLost();
      setProblem(errorText(e));
    } finally {
      setOpening("");
    }
  };

  if (!apps) {
    return (
      <Section title="Apps">
        <p className="eve-settings__text">{problem || "Loading…"}</p>
      </Section>
    );
  }
  if (!apps.composio) {
    return (
      <Section title="Apps">
        <p className="eve-settings__text">
          eve reaches your mail, calendar, files and work apps through Composio. Sign in once, then connect each app.
        </p>
        {where === "mac" ? (
          <button type="button" className="eve-button" onClick={() => window.location.assign("/onboard/composio/start")}>
            Sign in with Composio
          </button>
        ) : (
          <p className="eve-settings__text eve-settings__text--muted">
            Sign in to Composio on the Mac: open eve there, then Settings, Apps.
          </p>
        )}
        {problem && <p className="eve-settings__problem">{problem}</p>}
      </Section>
    );
  }
  return (
    <Section title="Apps">
      <p className="eve-settings__text eve-settings__text--muted">
        Connect an app, or just ask eve: it sends the link in the chat.
      </p>
      <ul className="eve-settings__list">
        {apps.apps.map((app) => (
          <li key={app.toolkit} className="eve-settings__row">
            <span className="eve-settings__label">{app.name}</span>
            {app.connected ? (
              <span className="eve-settings__ok">
                <CheckIcon /> Connected
              </span>
            ) : (
              <button
                type="button"
                className="eve-button eve-button--small"
                disabled={opening !== ""}
                onClick={() => void connect(app.toolkit)}
              >
                {opening === app.toolkit ? "Opening…" : "Connect"}
              </button>
            )}
          </li>
        ))}
      </ul>
      {problem && <p className="eve-settings__problem">{problem}</p>}
    </Section>
  );
}

// ---- Phone access (the Mac) ----------------------------------------------------------------------------------

/** Seconds left until `until` (ms), redrawn every second. */
function useCountdown(until: number | null): number {
  const [, tick] = useState(0);
  useEffect(() => {
    if (until === null) return;
    const timer = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(timer);
  }, [until]);
  return until === null ? 0 : Math.max(0, Math.ceil((until - Date.now()) / 1000));
}

function PhoneAccess({ status, reload, onAuthLost }: { status: WebStatus | null; reload(): void; onAuthLost(): void }) {
  const [win, setWin] = useState<PairWindow | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const left = useCountdown(win ? win.expiresAt : null);
  const expired = win !== null && left === 0;

  const open = async () => {
    setBusy(true);
    setProblem("");
    try {
      setWin(readPairWindow(await api("POST", "/api/web/pair/open", {})));
      reload(); // the LAN door is on now
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) onAuthLost();
      setProblem(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const setLan = async (on: boolean) => {
    setBusy(true);
    setProblem("");
    try {
      await api("POST", "/api/web/lan", { on });
      if (!on) setWin(null);
      reload();
    } catch (e) {
      setProblem(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Section title="Phone access">
      {win && !expired ? (
        <div className="eve-pair">
          <div className="eve-pair__codes">
            {win.links.map((link) => (
              <figure key={link.url} className="eve-pair__qr">
                {link.qr && <img src={link.qr} alt={`QR code for ${link.label}`} width={168} height={168} />}
                <figcaption>
                  <b>{link.label}</b>
                  <span className="eve-selectable eve-pair__url">{link.url.replace(/#.*$/, "")}</span>
                </figcaption>
              </figure>
            ))}
          </div>
          {win.code && (
            <p className="eve-pair__code" aria-label={`Code ${win.code.split("").join(" ")}`}>
              <span className="eve-selectable">{win.code}</span>
            </p>
          )}
          <p className="eve-settings__text eve-settings__text--muted">
            Expires in {Math.floor(left / 60)}:{String(left % 60).padStart(2, "0")}. On the phone, open the Camera,
            scan the code for it and tap the link. The browser warns once about eve's certificate: on an iPhone tap
            Show Details, then visit this website; in Chrome tap Advanced, then Proceed. On a laptop, type the address
            under a code and enter the six digits.
          </p>
          <button type="button" className="eve-button eve-button--plain" onClick={() => setWin(null)}>
            Done
          </button>
        </div>
      ) : (
        <>
          <p className="eve-settings__text">
            Use eve from a phone or laptop on this Wi-Fi. When you add one, macOS may ask to allow "python3.13" to find
            devices on local networks: that is eve, so click Allow. If the Mac's firewall is on, macOS also asks to
            accept incoming connections: click Allow again.
          </p>
          {expired && <p className="eve-settings__problem">That code expired. Make a new one.</p>}
          <button type="button" className="eve-button" disabled={busy} onClick={() => void open()}>
            {busy ? "Opening…" : "Add a phone"}
          </button>
        </>
      )}
      {status?.lan && (
        <div className="eve-settings__row eve-settings__row--top">
          <span className="eve-settings__label">
            Phones and laptops on this Wi-Fi
            <span className="eve-settings__sub">On. The Mac listens on its own network only.</span>
          </span>
          <button type="button" className="eve-button eve-button--small eve-button--plain" disabled={busy} onClick={() => void setLan(false)}>
            Turn off
          </button>
        </div>
      )}
      {status?.localNetwork === "blocked" && (
        <p className="eve-settings__problem">
          macOS is blocking eve's direct link, so phones chat over HTTPS and voice can't connect. Open System Settings,
          then Privacy &amp; Security, then Local Network, and turn on python3.13.
        </p>
      )}
      {problem && <p className="eve-settings__problem">{problem}</p>}
    </Section>
  );
}

// ---- Devices (the Mac) ---------------------------------------------------------------------------------------

function Devices({ onAuthLost }: { onAuthLost(): void }) {
  const [devices, setDevices] = useState<Device[] | null>(null);
  const [problem, setProblem] = useState("");
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      setDevices(readDevices(await api("GET", "/api/web/devices")));
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) onAuthLost();
      setProblem(errorText(e));
    }
  }, [onAuthLost]);

  useEffect(() => {
    void load();
  }, [load]);

  const others = (devices ?? []).filter((d) => !d.current);

  // One device, or every one but this browser (each by its id, so this browser is never signed out by it).
  const revoke = async (ids: string[]) => {
    if (ids.length > 1 && !window.confirm("Sign out every phone and laptop? Each one will need a new code.")) return;
    setBusy(true);
    setProblem("");
    try {
      for (const id of ids) await api("POST", "/api/web/devices/revoke", { id });
    } catch (e) {
      setProblem(errorText(e));
    } finally {
      await load();
      setBusy(false);
    }
  };

  return (
    <Section title="Devices">
      {devices === null ? (
        <p className="eve-settings__text">{problem || "Loading…"}</p>
      ) : devices.length === 0 ? (
        <p className="eve-settings__text eve-settings__text--muted">No phones or laptops yet.</p>
      ) : (
        <ul className="eve-settings__list">
          {devices.map((d) => (
            <li key={d.id} className="eve-settings__row">
              <span className="eve-settings__label">
                {d.label}
                {d.current && " (this browser)"}
                <span className="eve-settings__sub">
                  {[d.host, d.lastSeen !== null ? `seen ${ago(d.lastSeen)}` : ""].filter(Boolean).join(" · ")}
                </span>
              </span>
              {!d.current && (
                <button
                  type="button"
                  className="eve-button eve-button--small eve-button--plain"
                  disabled={busy}
                  onClick={() => void revoke([d.id])}
                >
                  Sign out
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      {others.length > 1 && (
        <button type="button" className="eve-button eve-button--plain" disabled={busy} onClick={() => void revoke(others.map((d) => d.id))}>
          Sign out every phone and laptop
        </button>
      )}
      {problem && devices !== null && <p className="eve-settings__problem">{problem}</p>}
    </Section>
  );
}

// ---- Trust eve (a phone or laptop) ---------------------------------------------------------------------------

function Trust() {
  const apple = appleMobile();
  return (
    <Section title="Trust eve on this device">
      <p className="eve-settings__text">
        Installing eve's own certificate ends the browser's warning for good. It is good only for addresses on your
        own network, so it can never vouch for any other site.
      </p>
      {apple ? (
        <>
          <a className="eve-button" href="/api/web/eve.mobileconfig">
            Download eve's profile
          </a>
          <p className="eve-settings__text eve-settings__text--muted">
            Then open Settings, tap Profile Downloaded and Install. Last, open Settings, General, About, Certificate
            Trust Settings, and turn on eve lite local CA.
          </p>
        </>
      ) : (
        <>
          <a className="eve-button" href="/api/web/ca.crt">
            Download eve's certificate
          </a>
          <p className="eve-settings__text eve-settings__text--muted">
            Then install it as a CA certificate. On Android: Settings, Security, Encryption &amp; credentials, Install a
            certificate, CA certificate. On a computer, open it and trust it for websites.
          </p>
        </>
      )}
    </Section>
  );
}

// ---- OpenRouter ----------------------------------------------------------------------------------------------

/**
 * The key's credit in a sentence. With no figure (an OAuth key gets none), it says so only for an account known to be
 * empty; not known says nothing.
 */
function creditLine(status: WebStatus): string {
  if (status.credit !== null) {
    return noCredit(status) ? "It has no credit left." : `It has ${money(status.credit)} of credit.`;
  }
  return noCredit(status) ? "It has no credit yet." : "";
}

export function OpenRouter({ status, reload, where }: { status: WebStatus | null; reload(): void; where: Where }) {
  const rejected = status?.openrouterRejected === true;
  const empty = status !== null && !rejected && noCredit(status);
  const left = status ? creditLine(status) : "";

  // Add credits opens openrouter.ai in a new tab: coming back to this one checks again by itself.
  useEffect(() => {
    if (!empty) return;
    const onVisible = () => {
      if (document.visibilityState === "visible") reload();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, [empty, reload]);

  if (rejected) {
    // The OAuth key was turned down: no credit can fix it, only a new sign-in. That sign-in is on the Mac's own
    // page (/onboard/openrouter/start is local-only), so the button is on the Mac and the phone is pointed there.
    return (
      <Section title="OpenRouter">
        <p className="eve-settings__text">
          eve's OpenRouter key stopped working — it was removed or turned off on OpenRouter.
          {where === "mac" ? " Sign in again to make a new one." : " Sign in again on the Mac to make a new one."}
        </p>
        <div className="eve-settings__buttons">
          {where === "mac" && (
            <button type="button" className="eve-button" onClick={() => window.location.assign("/onboard/openrouter/start")}>
              Sign in with OpenRouter again
            </button>
          )}
          <button type="button" className="eve-button eve-button--plain" onClick={reload}>
            Check again
          </button>
        </div>
      </Section>
    );
  }

  return (
    <Section title="OpenRouter">
      <p className="eve-settings__text">
        eve's models, voice and search run on your OpenRouter key.{left && ` ${left}`}
      </p>
      <div className="eve-settings__buttons">
        <a
          className={empty ? "eve-button" : "eve-button eve-button--plain"}
          href={ADD_CREDITS}
          target="_blank"
          rel="noopener noreferrer"
        >
          Add credits
        </a>
        <button type="button" className="eve-button eve-button--plain" onClick={reload}>
          Check again
        </button>
      </div>
    </Section>
  );
}

// ---- Help ----------------------------------------------------------------------------------------------------

const ROUTE: Record<LinkStatus, string> = {
  loopback: "This Mac's own browser: chat goes straight to eve on this Mac.",
  direct: "Direct link: chat goes to the Mac over WebRTC on this Wi-Fi.",
  fallback: "Direct link blocked, using HTTPS. Chat works; voice needs the direct link.",
  connecting: "Connecting the direct link…",
};

function Help({ where, route }: { where: Where; route: LinkStatus }) {
  return (
    <Section title="Help">
      <p className="eve-settings__text">{ROUTE[route]}</p>
      {where === "lan" && route !== "direct" && (
        <p className="eve-settings__text eve-settings__text--muted">
          To fix the direct link, on the Mac open System Settings, then Privacy &amp; Security, then Local Network, and
          turn on python3.13 (that is eve).
        </p>
      )}
      <p className="eve-settings__text eve-settings__text--muted">
        eve works on the Mac's own Wi-Fi, while the Mac is awake. Guest and office networks that keep devices apart
        block it. If the Mac's address changes, an iPhone keeps working; an Android phone needs a new code from Add a
        phone.
      </p>
    </Section>
  );
}

// ---- the sheet -----------------------------------------------------------------------------------------------

export function Settings({
  where,
  route,
  onClose,
  onAuthLost,
  onSignedOut,
}: {
  where: Where;
  route: LinkStatus;
  onClose(): void;
  onAuthLost(): void;
  onSignedOut(): void;
}) {
  const [status, setStatus] = useState<WebStatus | null>(null);
  const [leaving, setLeaving] = useState(false);

  const reload = useCallback(() => {
    void api("GET", "/api/web/status").then(
      (raw) => setStatus(readStatus(raw)),
      (e: unknown) => {
        if (e instanceof ApiError && e.status === 401) onAuthLost();
      },
    );
  }, [onAuthLost]);

  useEffect(() => {
    reload();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [reload, onClose]);

  const signOut = async () => {
    const ask =
      where === "mac"
        ? "Sign this browser out of eve? Open Eve from Applications to sign in again."
        : "Unpair this device? You will need a new code from the Mac to use eve here again.";
    if (!window.confirm(ask)) return;
    setLeaving(true);
    try {
      await api("POST", "/api/web/logout", {});
    } catch {
      // signed out either way: the cookie is the Mac's to drop
    }
    onSignedOut();
  };

  return (
    <div className="eve-settings" role="dialog" aria-modal="true" aria-labelledby="eve-settings-title">
      <div className="eve-settings__panel">
        <header className="eve-settings__header">
          <h1 id="eve-settings-title">Settings</h1>
          <button type="button" className="eve-round" aria-label="Close settings" onClick={onClose} autoFocus>
            <CloseIcon />
          </button>
        </header>
        <div className="eve-settings__body">
          <Apps where={where} onAuthLost={onAuthLost} />
          {where === "mac" && <PhoneAccess status={status} reload={reload} onAuthLost={onAuthLost} />}
          {where === "mac" && <Devices onAuthLost={onAuthLost} />}
          {where === "lan" && <Trust />}
          <OpenRouter status={status} reload={reload} where={where} />
          <Help where={where} route={route} />
          <button type="button" className="eve-button eve-button--plain eve-button--danger" disabled={leaving} onClick={() => void signOut()}>
            {where === "mac" ? "Sign this browser out" : "Unpair this device"}
          </button>
        </div>
      </div>
    </div>
  );
}
