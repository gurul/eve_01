// OpenUI theme mapped onto iOS system colors and the system font, so the
// message list looks like the rest of Eve Link in light and dark.

import { createTheme, defaultDarkTheme, defaultLightTheme, type Theme } from "@openuidev/react-ui";

const SYSTEM_FONT = '-apple-system, system-ui, "SF Pro Text", "Helvetica Neue", sans-serif';
const SYSTEM_MONO = 'ui-monospace, "SF Mono", SFMono-Regular, Menlo, monospace';

/** OpenUI bakes "Inter" into its compound typography tokens; swap every one. */
function systemTypography(base: Theme): Theme {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(base)) {
    if (typeof value !== "string") continue;
    if (value.includes('"Inter"')) out[key] = value.replace(/"Inter", sans-serif/g, SYSTEM_FONT);
    else if (value.includes("SFMono-Regular")) out[key] = value.replace(/"SFMono-Regular", Menlo, monospace/g, SYSTEM_MONO);
  }
  return {
    ...out,
    fontBody: SYSTEM_FONT,
    fontHeading: SYSTEM_FONT,
    fontLabel: SYSTEM_FONT,
    fontNumbers: SYSTEM_FONT,
    fontCode: SYSTEM_MONO,
    fontWeightRegular: "400",
    fontWeightMedium: "500",
  } as Theme;
}

// iOS semantic colors (UIColor.systemBlue, .label, .secondaryLabel, ...).
const light = {
  blue: "#007AFF",
  green: "#34C759",
  red: "#FF3B30",
  orange: "#FF9500",
  label: "#000000",
  secondaryLabel: "rgba(60, 60, 67, 0.6)",
  tertiaryLabel: "rgba(60, 60, 67, 0.3)",
  separator: "rgba(60, 60, 67, 0.29)",
  fill: "rgba(120, 120, 128, 0.2)",
  secondaryFill: "rgba(120, 120, 128, 0.16)",
  tertiaryFill: "rgba(118, 118, 128, 0.12)",
  card: "#FFFFFF",
  grouped: "#F2F2F7",
};

const dark = {
  blue: "#0A84FF",
  green: "#30D158",
  red: "#FF453A",
  orange: "#FF9F0A",
  label: "#FFFFFF",
  secondaryLabel: "rgba(235, 235, 245, 0.6)",
  tertiaryLabel: "rgba(235, 235, 245, 0.3)",
  separator: "rgba(84, 84, 88, 0.65)",
  fill: "rgba(120, 120, 128, 0.36)",
  secondaryFill: "rgba(120, 120, 128, 0.32)",
  tertiaryFill: "rgba(118, 118, 128, 0.24)",
  card: "#1C1C1E",
  grouped: "#000000",
};

function colors(c: typeof light): Theme {
  return {
    // The native view paints the page background. foreground is not a page
    // color in OpenUI: it is an opaque surface (chart tooltips, switch and
    // slider thumbs, table scroll buttons, chart dot rings), so it is a card.
    background: "transparent",
    foreground: c.card,
    popoverBackground: c.card,
    sunkLight: c.tertiaryFill,
    sunk: c.tertiaryFill,
    sunkDeep: c.secondaryFill,
    elevatedLight: c.card,
    elevated: c.card,
    elevatedStrong: c.secondaryFill,
    elevatedIntense: c.fill,
    highlightSubtle: c.tertiaryFill,
    highlight: c.tertiaryFill,
    highlightStrong: c.secondaryFill,
    highlightIntense: c.fill,
    textNeutralPrimary: c.label,
    textNeutralSecondary: c.secondaryLabel,
    textNeutralTertiary: c.tertiaryLabel,
    textNeutralLink: c.blue,
    textBrand: c.blue,
    textAccentPrimary: "#FFFFFF",
    textSuccessPrimary: c.green,
    textAlertPrimary: c.orange,
    textDangerPrimary: c.red,
    textInfoPrimary: c.blue,
    interactiveAccentDefault: c.blue,
    interactiveAccentHover: c.blue,
    interactiveAccentPressed: c.blue,
    borderDefault: c.separator,
    borderInteractive: c.separator,
    borderInteractiveSelected: c.blue,
    borderAccentSelected: c.blue,
    // Owner bubbles in iMessage blue, white text.
    chatUserResponseBg: c.blue,
    chatUserResponseText: "#FFFFFF",
  } as Theme;
}

export const lightTheme: Theme = createTheme({ ...systemTypography(defaultLightTheme), ...colors(light) });
export const darkTheme: Theme = createTheme({ ...systemTypography(defaultDarkTheme), ...colors(dark) });
