// Card preview: paste a json-render block and see it drawn as the app draws
// it (JsonCard, the same registry, theme and CSS), with what each tap would
// send. A development page only: `pnpm preview-cards`, then open the address
// it prints. Not part of the app's bundle or eve lite's.
import "@openuidev/react-ui/styles/index.css";
import "../native.css";

import { ThemeProvider } from "@openuidev/react-ui";
import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { JsonCard } from "../JsonCard";
import { inspectSpec } from "../specProgram";
import { darkTheme, lightTheme } from "../theme";

const line = (path: string, value: unknown) => JSON.stringify({ op: "add", path, value });
const el = (key: string, type: string, props: unknown, children: string[] = [], more: object = {}) =>
  line(`/elements/${key}`, { type, props, children, ...more });
const ask = (message: string, data?: unknown) => ({ on: { press: { action: "ask", params: data ? { message, data } : { message } } } });

// One block with every component in the catalog.
const SHOWCASE = [
  line("/root", "card"),
  el("card", "Card", { title: "Train or plane", subtitle: "Seattle to Portland, door to door" },
    ["note", "table", "chart", "facts", "tabs", "tags", "form", "choose", "next"]),
  el("note", "Callout", { variant: "warning", title: "Fares rise on Thursday", description: "Book before then to keep the **$89** price." }),
  el("table", "Table", { columns: ["Compare", "Train", "Plane"], rows: [["Time", "4 h 10 min", "3 h 30 min"], ["Cost", "$89", "$140"], ["Luggage", "Two bags", "One carry-on"]] }),
  el("chart", "BarChart", { labels: ["Train", "Plane", "Car"], series: [{ name: "Minutes", values: [250, 210, 200] }, { name: "Dollars", values: [89, 140, 60] }], xLabel: "Way to go", yLabel: "Minutes or dollars" }),
  el("facts", "Facts", { rows: [{ label: "Cheapest", value: "Car, $60" }, { label: "Fastest", value: "Car, 3 h 20 min" }] }),
  el("tabs", "Tabs", { tabs: ["Steps", "Trend", "Share", "Stops"] }, ["steps", "trend", "share", "stops"]),
  el("steps", "Steps", { items: [{ title: "Book early", details: "Fares rise in the last week." }, { title: "Pick a window seat", details: "The Puget Sound side has the view." }] }),
  el("trend", "LineChart", { labels: ["Mon", "Tue", "Wed", "Thu", "Fri"], series: [{ name: "Train fare ($)", values: [79, 79, 89, 104, 119] }], xLabel: "Day booked", yLabel: "$" }),
  el("share", "PieChart", { labels: ["Train", "Plane", "Car"], values: [30, 25, 45], variant: "donut" }),
  el("stops", "List", { variant: "number", items: [{ title: "Tacoma", subtitle: "45 min in" }, { title: "Olympia", subtitle: "1 h 20 min in", url: "https://example.com/olympia" }] }),
  el("tags", "Tags", { tags: ["Friday", "One way", "Two adults"] }),
  el("form", "Stack", {}, ["when", "seat", "extras"]),
  el("when", "TextInput", { label: "Leave after", placeholder: "9:00 am", value: { $bindState: "/form/after" } }),
  el("seat", "Select", { label: "Seat", options: ["Window", "Aisle"], value: { $bindState: "/form/seat" } }),
  el("extras", "Chips", { label: "Extras", options: ["Bike", "Quiet car", "Wi-Fi"], value: { $bindState: "/form/extras" } }),
  line("/state/form", { after: "", seat: "Window", extras: ["Wi-Fi"] }),
  el("choose", "Buttons", { direction: "row" }, ["book", "site"]),
  el("book", "Button", { label: "Find trains" }, [], ask("Find trains for Friday", { $state: "/form" })),
  el("site", "Button", { label: "Open timetable", variant: "secondary" }, [], { on: { press: { action: "open_url", params: { url: "https://example.com/timetable" } } } }),
  el("next", "FollowUps", { items: ["Which is better for the climate?", "What about the bus?"] }),
].join("\n");

function Preview() {
  const [code, setCode] = useState(SHOWCASE);
  const [log, setLog] = useState<string[]>([]);
  const [dark, setDark] = useState(() => matchMedia("(prefers-color-scheme: dark)").matches);
  const [typed, setTyped] = useState<string | null>(null);        // "Stream it": the block arriving a bit at a time
  useEffect(() => {
    if (typed === null || typed.length >= code.length) return;
    const t = setTimeout(() => setTyped(code.slice(0, typed.length + 40)), 30);
    return () => clearTimeout(t);
  }, [typed, code]);
  useEffect(() => { document.documentElement.style.colorScheme = dark ? "dark" : "light"; }, [dark]);
  const streaming = typed !== null && typed.length < code.length;
  const shown = typed === null ? code : typed;
  const result = inspectSpec(code);
  const say = (text: string) => setLog((l) => [text, ...l].slice(0, 20));
  return (
    <ThemeProvider mode={dark ? "dark" : "light"} lightTheme={lightTheme} darkTheme={darkTheme}>
      {/* native.css pins the page for the app's own scroller, so this page scrolls inside main */}
      <main style={{ maxWidth: 430, margin: "0 auto", padding: 16, background: dark ? "#000" : "#fff", height: "100dvh", overflowY: "auto", boxSizing: "border-box" }}>
        <p style={{ display: "flex", gap: 12, flexWrap: "wrap" }}>
          <button type="button" onClick={() => setDark((d) => !d)}>{dark ? "Light" : "Dark"}</button>
          <button type="button" onClick={() => setTyped("")}>Stream it</button>
          <span role="status" id="verdict">
            {result.complete ? `valid${result.adopted.length ? `, shown last: ${result.adopted.join(", ")}` : ""}` : `not a whole card: ${result.issues.join("; ")}`}
          </span>
        </p>
        <JsonCard
          key={typed === null ? "whole" : "typed"}
          stateKey="preview"
          code={shown}
          streaming={streaming}
          onAsk={(message, data) => say(`ask: ${message}${data === undefined ? "" : ` ${JSON.stringify(data)}`}`)}
          onOpen={(url) => say(`open: ${url}`)}
        />
        <h2 style={{ fontSize: 15 }}>What a tap sends</h2>
        <ol id="log" style={{ fontSize: 13 }}>{log.map((entry, i) => <li key={i}>{entry}</li>)}</ol>
        <h2 style={{ fontSize: 15 }}>The block</h2>
        <textarea
          aria-label="json-render block"
          value={code}
          onChange={(e) => { setTyped(null); setCode(e.target.value); }}
          spellCheck={false}
          style={{ width: "100%", height: 320, fontFamily: "ui-monospace, monospace", fontSize: 11 }}
        />
      </main>
    </ThemeProvider>
  );
}

createRoot(document.getElementById("root")!).render(<StrictMode><Preview /></StrictMode>);
