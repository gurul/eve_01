// A json-render card: the same stable wrapper as GeneratedCard (partial
// output, completion, a text view, input reset), over json-render's renderer
// and eve's catalog. See specProgram.ts for what counts as drawable.
import { setByPath } from "@json-render/core";
import { JSONUIProvider, Renderer } from "@json-render/react";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { CardHostContext, registry, type CardHost } from "./jsonRegistry";
import { inspectSpec, selectSpec, type SpecProgram } from "./specProgram";
import { UiStateCache } from "./uiProgram";

// What the owner typed or picked, by state path. Page-local: never on disk or in a log.
const forms = new UiStateCache();

export function JsonCard({ stateKey, code, streaming, onAsk, onOpen }: {
  stateKey: string;
  code: string;
  streaming: boolean;
  /** A tap that continues the conversation: the message, and form values when the card sent some. */
  onAsk: (message: string, data?: unknown) => void;
  onOpen: (url: string) => void;
}) {
  const current = useMemo(() => inspectSpec(code, streaming), [code, streaming]);
  const lastGood = useRef<SpecProgram | undefined>(undefined);
  const selected = selectSpec(current, lastGood.current, streaming);
  useEffect(() => { if (selected) lastGood.current = selected; }, [selected]);
  const [revision, setRevision] = useState(0);
  const [textMode, setTextMode] = useState(false);
  const textId = useId();

  // The block's own state, with the owner's edits on top. Keyed by content so
  // a streaming chunk that leaves the state alone hands back the same object.
  const seed = JSON.stringify((selected?.spec as { state?: unknown } | null | undefined)?.state ?? {});
  const initialState = useMemo(() => {
    const state = JSON.parse(seed) as Record<string, unknown>;
    for (const [path, value] of Object.entries(forms.get(stateKey) ?? {})) setByPath(state, path, value);
    return state;
    // revision: Reset inputs clears the edits, then rebuilds from the block
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seed, stateKey, revision]);
  const onStateChange = useCallback((changes: Array<{ path: string; value: unknown }>) => {
    const kept = { ...(forms.get(stateKey) ?? {}) };
    for (const { path, value } of changes) kept[path] = value;
    forms.set(stateKey, kept);
  }, [stateKey]);

  const handlers = useMemo(() => ({
    ask: (params: Record<string, unknown>) => {
      const message = typeof params?.["message"] === "string" ? params["message"] : "";
      if (message) onAsk(message, params["data"]);
    },
    open_url: (params: Record<string, unknown>) => {
      if (typeof params?.["url"] === "string") onOpen(params["url"]);
    },
  }), [onAsk, onOpen]);
  const host = useMemo<CardHost>(() => ({ ask: (text) => onAsk(text), open: onOpen, streaming }), [onAsk, onOpen, streaming]);

  const failed = !streaming && !selected;
  const recovered = current.text || lastGood.current?.text || "";
  const showText = failed || textMode;

  return (
    <section className="eve-generated-card" aria-label="Eve card" aria-busy={streaming} data-engine="json-render">
      <div hidden={showText} className="eve-generated-card__visual">
        {selected?.spec && <CardHostContext.Provider value={host}>
          <JSONUIProvider key={revision} registry={registry} initialState={initialState} handlers={handlers} onStateChange={onStateChange}>
            <Renderer spec={selected.spec} registry={registry} loading={streaming} />
          </JSONUIProvider>
        </CardHostContext.Provider>}
      </div>
      {streaming && !selected && <p className="eve-generated-card__status" role="status">Building your card…</p>}
      {failed && <p className="eve-generated-card__status" role="status">
        {recovered ? "This card couldn’t be displayed. Recovered text may be incomplete." : "This card couldn’t be displayed. Ask Eve to send it as text."}
      </p>}
      {showText && recovered && <div id={textId} className="eve-generated-card__text">
        <p className="eve-generated-card__caption">Text from this reply{current.hasControls ? " · live values aren’t included" : ""}</p>
        <div>{recovered}</div>
      </div>}
      <div className="eve-generated-card__actions">
        {!streaming && !failed && recovered && <button
          type="button"
          className="eve-generated-card__toggle"
          aria-expanded={textMode}
          aria-controls={textMode ? textId : undefined}
          onClick={() => setTextMode((shown) => !shown)}
        >{textMode ? "Show card" : "Show text"}</button>}
        {!streaming && !failed && selected?.hasControls && <button
          type="button"
          className="eve-generated-card__toggle"
          onClick={() => {
            forms.delete(stateKey);
            setRevision((v) => v + 1);
          }}
        >Reset inputs</button>}
      </div>
    </section>
  );
}
