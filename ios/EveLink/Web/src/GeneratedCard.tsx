// Stable wrapper through partial output, completion and text/card switches.
import { Renderer, type ActionEvent, type OpenUIError } from "@openuidev/react-lang";
import { openuiChatLibrary } from "@openuidev/react-ui/genui-lib";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { inspectUi, selectUi, UiStateCache, type UiProgram } from "./uiProgram";

const forms = new UiStateCache();

export function GeneratedCard({ stateKey, code, streaming, onAction }: {
  stateKey: string;
  code: string;
  streaming: boolean;
  onAction: (event: ActionEvent) => void;
}) {
  const current = useMemo(() => inspectUi(code), [code]);
  const lastGood = useRef<UiProgram | undefined>(undefined);
  const selected = selectUi(current, lastGood.current, streaming);
  useEffect(() => { if (selected) lastGood.current = selected; }, [selected]);
  // Hydrate once: passing the changing cache back into Renderer reapplies
  // defaults and can overwrite edits when a streaming chunk arrives.
  const [initialState, setInitialState] = useState(() => forms.get(stateKey));
  const [revision, setRevision] = useState(0);
  const [textMode, setTextMode] = useState(false);
  const [failedCode, setFailedCode] = useState<string | null>(null);
  const textId = useId();
  const onStateUpdate = useCallback((state: Record<string, unknown>) => forms.set(stateKey, state), [stateKey]);
  const onError = useCallback((errors: OpenUIError[]) => {
    // Error messages can include generated content: keep them out of logs.
    if (errors.length && selected) setFailedCode(selected.code);
  }, [selected]);
  const failed = !streaming && (!selected || failedCode === selected.code);
  const recovered = current.text || lastGood.current?.text || "";
  const showText = failed || textMode;

  return (
    <section className="eve-generated-card" aria-label="Eve card" aria-busy={streaming}>
      <div hidden={showText} className="eve-generated-card__visual">
        {selected && <Renderer
          key={revision}
          response={selected.code}
          library={openuiChatLibrary}
          isStreaming={streaming}
          onAction={onAction}
          onStateUpdate={onStateUpdate}
          initialState={initialState}
          onError={onError}
          publishObservability={false}
        />}
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
            setInitialState(undefined);
            setRevision((v) => v + 1);
          }}
        >Reset inputs</button>}
      </div>
    </section>
  );
}
