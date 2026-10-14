// The screens before the chat: this browser is not signed in (the Mac) or not paired (a phone or laptop), eve
// cannot be reached yet, or eve is restarting after a sign-in.

import { useRef, useState, type FormEvent } from "react";
import { MAC_SIGNED_OUT } from "./host";
import { PAIR_CODE, pairDevice } from "./pair";
import { appleMobile } from "./windows";

/**
 * What Eve.app runs, for a Mac without it (the app is optional, and the installer goes on when it can't be made).
 * eve lite keeps eve-bridge in its own venv and never puts it on PATH (install-lite.sh), so it is named by its full
 * path. It reads EVE_PROFILE=lite from eve's env file, as every eve-bridge command does.
 */
export const OPEN_COMMAND = "~/.eve-lite/venv/bin/eve-bridge web open";

export function SignIn({ note, onRetry }: { note: string; onRetry(): void }) {
  const line = useRef<HTMLElement>(null);
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(OPEN_COMMAND);
      setCopied(true);
    } catch {
      // no clipboard here: select the line instead, so Command-C copies it
      if (line.current) window.getSelection()?.selectAllChildren(line.current);
    }
  };

  return (
    <main className="eve-gate">
      <div className="eve-gate__card">
        <h1 className="eve-gate__title">Open eve from this Mac</h1>
        <p className="eve-gate__text">
          This browser isn't signed in to eve. Open <b>Eve</b> from Applications (or press ⌘ Space and type Eve), and
          eve opens signed in.
        </p>
        <p className="eve-gate__text">No Eve app? Paste this line into Terminal and press Return:</p>
        <div className="eve-command">
          <code ref={line} className="eve-selectable">
            {OPEN_COMMAND}
          </code>
          <button type="button" className="eve-button eve-button--small eve-button--plain" onClick={() => void copy()}>
            {copied ? "Copied" : "Copy"}
          </button>
        </div>
        {/* MAC_SIGNED_OUT only says again what this card says */}
        {note && note !== MAC_SIGNED_OUT && <p className="eve-settings__problem">{note}</p>}
        <button type="button" className="eve-button eve-button--plain" onClick={onRetry}>
          Try again
        </button>
      </div>
    </main>
  );
}

export function Pair({ note, onPaired }: { note: string; onPaired(): void }) {
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState(note);
  const apple = appleMobile();

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    if (!PAIR_CODE.test(code)) {
      setProblem("The code is six digits.");
      return;
    }
    setBusy(true);
    setProblem("");
    const outcome = await pairDevice({ code });
    setBusy(false);
    if (outcome.ok) onPaired();
    else setProblem(outcome.message);
  };

  return (
    <main className="eve-gate">
      <form className="eve-gate__card" onSubmit={(e) => void submit(e)}>
        <h1 className="eve-gate__title">Pair with eve</h1>
        <p className="eve-gate__text">
          On the Mac, open eve, then Settings, Phone access, Add a phone. Scan its code with this phone's camera, or
          type the six digits here.
        </p>
        <input
          className="eve-code"
          inputMode="numeric"
          autoComplete="one-time-code"
          pattern="[0-9]*"
          maxLength={6}
          aria-label="Six-digit code"
          placeholder="000000"
          value={code}
          onChange={(e) => setCode(e.target.value.replace(/\D/g, "").slice(0, 6))}
        />
        <button type="submit" className="eve-button" disabled={busy || code.length !== 6}>
          {busy ? "Pairing…" : "Pair"}
        </button>
        {problem && (
          <p className="eve-settings__problem" role="alert">
            {problem}
          </p>
        )}
        <p className="eve-gate__foot">
          Tired of the certificate warning?{" "}
          <a href={apple ? "/api/web/eve.mobileconfig" : "/api/web/ca.crt"}>
            {apple ? "Download eve's profile" : "Download eve's certificate"}
          </a>{" "}
          and trust it on this device.
        </p>
      </form>
    </main>
  );
}

export function Waiting({ message, onRetry }: { message: string; onRetry?: () => void }) {
  return (
    <main className="eve-gate">
      <div className="eve-gate__card eve-gate__card--quiet">
        <span className="eve-spinner eve-spinner--large" aria-hidden />
        <p className="eve-gate__text" role="status">
          {message}
        </p>
        {onRetry && (
          <button type="button" className="eve-button eve-button--plain" onClick={onRetry}>
            Try again
          </button>
        )}
      </div>
    </main>
  );
}

export function Restarting({ late }: { late: boolean }) {
  return (
    <main className="eve-gate">
      <div className="eve-gate__card eve-gate__card--quiet">
        <h1 className="eve-gate__title">Restarting eve</h1>
        <div className="eve-progress" role="progressbar" aria-label="Restarting eve">
          <span />
        </div>
        <p className="eve-gate__text" role="status">
          {late
            ? "This is taking longer than usual. eve comes back by itself; check that the Mac is awake."
            : "eve is starting again with what you just connected. About 5 seconds."}
        </p>
      </div>
    </main>
  );
}
