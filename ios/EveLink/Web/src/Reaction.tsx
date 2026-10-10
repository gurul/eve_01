// The small badge on the corner of an owner bubble that wears eve's
// reaction (reactions.ts). It pops in once when it appears or changes.

import { sanitizeReaction } from "./reactions";

export function ReactionBadge({ emoji }: { emoji: string }) {
  const clean = sanitizeReaction(emoji);
  if (!clean) return null;
  // keyed by the emoji, so a replaced reaction (👀 then 👍) animates in again
  return (
    <span key={clean} className="eve-reaction" role="img" aria-label={`eve reacted ${clean}`}>
      {clean}
    </span>
  );
}
