// "Sources" under an eve reply (sources.ts): the memory lines it leaned on.
// At rest a small muted line, "Sources · 3"; a tap opens the list, each
// source as one line with its kind ("Fact", "Remembered", "Said") and, when
// known, when it is from. Open or closed is kept per message, so a remount
// (the list re-keys turns) does not fold it again.

import { useState } from "react";
import { KIND_LABEL, sanitizeSources, type Source } from "./sources";

const open = new Set<string>();

export function SourceList({
  messageId,
  items,
  defaultOpen = false,
}: {
  messageId: string;
  items: Source[];
  defaultOpen?: boolean;
}) {
  const clean = sanitizeSources(items);
  const [expanded, setExpanded] = useState(() => defaultOpen || open.has(messageId));
  if (clean.length === 0) return null;
  const listId = `eve-sources-${messageId.replace(/[^A-Za-z0-9_-]/g, "_")}`;
  const toggle = () => {
    const next = !expanded;
    if (next) open.add(messageId);
    else open.delete(messageId);
    setExpanded(next);
  };
  return (
    <div className="eve-sources">
      <button
        type="button"
        className="eve-sources-toggle"
        aria-expanded={expanded}
        aria-controls={listId}
        onClick={toggle}
      >
        <span>Sources</span>
        <span aria-hidden="true"> · </span>
        <span>{clean.length}</span>
        <svg className={expanded ? "eve-sources-chevron is-open" : "eve-sources-chevron"} viewBox="0 0 12 12" aria-hidden="true">
          <path d="M4.2 2.6 7.6 6 4.2 9.4" />
        </svg>
      </button>
      {expanded && (
        <ul id={listId} className="eve-sources-list">
          {clean.map((s, i) => (
            <li key={i} className="eve-source">
              <span className="eve-source-meta">
                <span className={`eve-source-kind eve-source-kind--${s.kind}`}>{KIND_LABEL[s.kind]}</span>
                {s.when && <span className="eve-source-when">{s.when}</span>}
              </span>
              <span className="eve-source-text">{s.text}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
