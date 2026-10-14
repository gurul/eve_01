// "Set up eve" on the Mac's own browser (docs/plans/eve-lite.md, user flow A): two browser approvals and nothing
// else. 1. OpenRouter: OAuth PKCE mints eve's one model key. 2. Composio, or Skip for now. Each sign-in leaves this
// page for the provider's and comes back to it; eve then restarts in about 5 s (the Root shows that).

import { useEffect, useState, type ReactNode } from "react";
import { api, noCredit, type OnboardState } from "./api";
import { CheckIcon } from "./icons";
import { ADD_CREDITS, money } from "./Settings";

function Step({ n, title, done, children }: { n: number; title: string; done: boolean; children: ReactNode }) {
  return (
    <li className={done ? "eve-step eve-step--done" : "eve-step"}>
      <span className="eve-step__n" aria-hidden>
        {done ? <CheckIcon /> : n}
      </span>
      <div className="eve-step__body">
        <h2 className="eve-step__title">{title}</h2>
        {children}
      </div>
    </li>
  );
}

export function Setup({ state, onChanged }: { state: OnboardState; onChanged(): void }) {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const credit = state.credit;
  const rejected = state.openrouterRejected;
  // OpenRouter often gives the key no figure (an OAuth key has no limit); funded alone then says it has none. A key
  // that was turned down (rejected) is not "empty": no credit can fix it, only a new sign-in.
  const empty = state.openrouter && !rejected && noCredit(state);
  const appName = window.location.host;

  // Add credits opens openrouter.ai in a new tab: coming back to this one checks again by itself.
  useEffect(() => {
    if (!empty) return;
    const onVisible = () => {
      if (document.visibilityState === "visible") onChanged();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, [empty, onChanged]);

  const skip = async () => {
    setBusy(true);
    setProblem("");
    try {
      await api("POST", "/onboard/composio/skip", {});
      onChanged();
    } catch (e) {
      setProblem(e instanceof Error ? e.message : "That didn't work. Try again.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <main className="eve-gate">
      <div className="eve-gate__card">
        <h1 className="eve-gate__title">Set up eve</h1>
        <p className="eve-gate__text">Two sign-ins in your browser, and eve is ready to chat.</p>
        <ol className="eve-steps">
          <Step n={1} title="OpenRouter" done={state.openrouter && !rejected}>
            {rejected ? (
              <>
                <p className="eve-step__text">
                  eve's OpenRouter key stopped working — it was removed or turned off on OpenRouter. Sign in again to
                  make a new one.
                </p>
                <button
                  type="button"
                  className="eve-button"
                  onClick={() => window.location.assign("/onboard/openrouter/start")}
                >
                  Sign in with OpenRouter again
                </button>
              </>
            ) : state.openrouter ? (
              <>
                <p className="eve-step__text">
                  OpenRouter connected{credit !== null ? ` · ${money(credit)} credit` : empty ? " · no credit yet" : ""}
                </p>
                {empty && (
                  <>
                    <p className="eve-step__text">
                      eve needs credit on OpenRouter to answer. Add some, then come back to this page.
                    </p>
                    <div className="eve-settings__buttons">
                      <a className="eve-button" href={ADD_CREDITS} target="_blank" rel="noopener noreferrer">
                        Add credits
                      </a>
                      <button type="button" className="eve-button eve-button--plain" onClick={onChanged}>
                        Check again
                      </button>
                    </div>
                  </>
                )}
              </>
            ) : (
              <>
                <p className="eve-step__text">
                  eve thinks, talks and searches with models on OpenRouter, on a key made for it. Sign in or sign up,
                  then click Authorize. OpenRouter names the app "{appName}": that is eve on this Mac.
                </p>
                <button type="button" className="eve-button" onClick={() => window.location.assign("/onboard/openrouter/start")}>
                  Sign in with OpenRouter
                </button>
              </>
            )}
          </Step>
          <Step n={2} title="Composio (optional)" done={state.composio}>
            {state.composio ? (
              <p className="eve-step__text">Composio connected</p>
            ) : (
              <>
                <p className="eve-step__text">
                  eve reaches your mail, calendar, files and work apps through Composio. You can also do this later, in
                  Settings, Apps.
                </p>
                <div className="eve-settings__buttons">
                  <button
                    type="button"
                    className="eve-button"
                    disabled={!state.openrouter || busy}
                    onClick={() => window.location.assign("/onboard/composio/start")}
                  >
                    Sign in with Composio
                  </button>
                  <button
                    type="button"
                    className="eve-button eve-button--plain"
                    disabled={!state.openrouter || busy}
                    onClick={() => void skip()}
                  >
                    Skip for now
                  </button>
                </div>
              </>
            )}
          </Step>
        </ol>
        {(state.error || problem) && <p className="eve-settings__problem">{problem || state.error}</p>}
        <p className="eve-gate__foot">
          If macOS said Background Items Added for "python3.13", that is eve: it starts when you log in.
        </p>
      </div>
    </main>
  );
}
