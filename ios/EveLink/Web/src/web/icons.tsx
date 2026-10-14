// The shell's few glyphs, drawn inline (no icon font, nothing fetched). Each takes the text color.

const common = {
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.8,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  "aria-hidden": true,
};

/** The iOS app's menu glyph: two lines, the upper one shorter. */
export function MenuIcon() {
  return (
    <svg {...common} className="eve-icon">
      <path d="M5 9h8M5 15h13" />
    </svg>
  );
}

export function WaveIcon() {
  return (
    <svg {...common} className="eve-icon">
      <path d="M4 10v4M8 7v10M12 4v16M16 8v8M20 11v2" />
    </svg>
  );
}

export function PlusIcon() {
  return (
    <svg {...common} className="eve-icon">
      <path d="M12 5v14M5 12h14" />
    </svg>
  );
}

export function ArrowUpIcon() {
  return (
    <svg {...common} className="eve-icon" strokeWidth={2.2}>
      <path d="M12 19V5M6 11l6-6 6 6" />
    </svg>
  );
}

export function StopIcon() {
  return (
    <svg viewBox="0 0 24 24" className="eve-icon" aria-hidden>
      <rect x="7" y="7" width="10" height="10" rx="2" fill="currentColor" />
    </svg>
  );
}

export function CloseIcon() {
  return (
    <svg {...common} className="eve-icon" strokeWidth={2}>
      <path d="M6 6l12 12M18 6L6 18" />
    </svg>
  );
}

export function MicIcon() {
  return (
    <svg {...common} className="eve-icon">
      <rect x="9" y="3" width="6" height="11" rx="3" />
      <path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21" />
    </svg>
  );
}

export function SparkIcon() {
  return (
    <svg {...common} className="eve-icon">
      <path d="M12 3.5l1.9 5 5 1.9-5 1.9-1.9 5-1.9-5-5-1.9 5-1.9z" />
    </svg>
  );
}

export function CheckIcon() {
  return (
    <svg {...common} className="eve-icon" strokeWidth={2.2}>
      <path d="M5 12.5l4.5 4.5L19 7.5" />
    </svg>
  );
}

export function FileIcon() {
  return (
    <svg {...common} className="eve-icon">
      <path d="M7 3h7l5 5v13H7z" />
      <path d="M14 3v5h5" />
    </svg>
  );
}
