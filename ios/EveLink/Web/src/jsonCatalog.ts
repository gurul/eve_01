// The json-render catalog for eve's cards: the only components and actions a
// json-render block in a reply may use. The page's registry (jsonRegistry.tsx)
// draws each one; scripts/gen-json-prompt.mjs turns this file into the
// reference the brain is given; scripts/validate-spec.mjs checks a block
// against it. No React here, so Node scripts can import it as it is (keep the
// syntax erasable: Node strips the types and runs the rest).
//
// Trial, 2026-11-16: the engine is chosen by the bridge (EVE_CARD_ENGINE);
// OpenUI Lang is the default and this catalog is the other choice. See
// docs/app-chat.md, "The card engine".
import { defineCatalog } from "@json-render/core";
import { schema } from "@json-render/react/schema";
import { z } from "zod";

const text = z.string();
const maybeText = z.string().nullish();
const cell = z.union([z.string(), z.number()]);
/** A prop a form control writes to: `{ "$bindState": "/form/name" }`, or a fixed value. */
const boundText = z.union([z.string(), z.record(z.string(), z.unknown())]).nullish();
const boundList = z.union([z.array(z.string()), z.record(z.string(), z.unknown())]).nullish();

export const catalog = defineCatalog(schema, {
  components: {
    Card: {
      props: z.object({ title: maybeText, subtitle: maybeText }),
      slots: ["default"],
      description:
        "The root of every block: one card with an optional title and subtitle. Its children are the content, top to bottom.",
    },
    Stack: {
      props: z.object({ direction: z.enum(["column", "row"]).nullish() }),
      slots: ["default"],
      description: "Groups a few children, in a column (default) or a row. Use it for one tab's or one section's content.",
    },
    Text: {
      props: z.object({ text }),
      description: "A short paragraph. Markdown is allowed.",
    },
    Callout: {
      props: z.object({
        variant: z.enum(["info", "warning", "success", "neutral"]).nullish(),
        title: text,
        description: maybeText,
      }),
      description: "A banner for something the owner must not miss: a warning or a deadline.",
    },
    Table: {
      props: z.object({ columns: z.array(z.string()), rows: z.array(z.array(cell)) }),
      description: "A table. columns are the headings; each row has one cell per column, in the same order.",
    },
    Facts: {
      props: z.object({ rows: z.array(z.object({ label: text, value: text })) }),
      description: "Label and value pairs, one per line: totals, a summary, the details of one thing.",
    },
    List: {
      props: z.object({
        variant: z.enum(["number", "image"]).nullish(),
        items: z.array(z.object({
          title: text,
          subtitle: maybeText,
          image: z.object({ src: text, alt: text }).nullish(),
          url: maybeText,
        })),
      }),
      description:
        "A list of options or results. variant \"number\" (default) numbers them; \"image\" shows each item's image. An item with a url opens it when tapped.",
    },
    Steps: {
      props: z.object({ items: z.array(z.object({ title: text, details: maybeText })) }),
      description: "A step-by-step guide, numbered in order.",
    },
    BarChart: {
      props: z.object({
        labels: z.array(z.string()),
        series: z.array(z.object({ name: text, values: z.array(z.number()) })),
        variant: z.enum(["grouped", "stacked"]).nullish(),
        xLabel: maybeText,
        yLabel: maybeText,
      }),
      description: "Vertical bars comparing a few categories. Each series has one value per label.",
    },
    LineChart: {
      props: z.object({
        labels: z.array(z.string()),
        series: z.array(z.object({ name: text, values: z.array(z.number()) })),
        xLabel: maybeText,
        yLabel: maybeText,
      }),
      description: "Lines showing a trend over time. Each series has one value per label.",
    },
    PieChart: {
      props: z.object({
        labels: z.array(z.string()),
        values: z.array(z.number()),
        variant: z.enum(["pie", "donut"]).nullish(),
      }),
      description: "Parts of one whole: one value per label.",
    },
    Image: {
      props: z.object({ src: text, alt: text }),
      description: "One picture, from a real image URL.",
    },
    Tags: {
      props: z.object({ tags: z.array(z.string()) }),
      description: "A row of short labels.",
    },
    Buttons: {
      props: z.object({ direction: z.enum(["row", "column"]).nullish() }),
      slots: ["default"],
      description: "A group of Button children, side by side (default) or stacked.",
    },
    Button: {
      props: z.object({ label: text, variant: z.enum(["primary", "secondary"]).nullish() }),
      events: ["press"],
      description: "A button the owner taps. Bind its press event to an action with on.press.",
    },
    FollowUps: {
      props: z.object({ items: z.array(z.string()) }),
      description:
        "Likely next questions, at the end of a card. A tap sends that item's text to you as the owner's next message.",
    },
    Tabs: {
      props: z.object({ tabs: z.array(z.string()) }),
      slots: ["default"],
      description: "Tabbed content. tabs are the labels; the children are the tabs' contents, one child per label in the same order.",
    },
    Accordion: {
      props: z.object({ sections: z.array(z.string()) }),
      slots: ["default"],
      description:
        "Collapsible sections. sections are the titles; the children are the sections' contents, one child per title in the same order.",
    },
    TextInput: {
      props: z.object({ label: text, placeholder: maybeText, value: boundText }),
      description: "A one-line text field. Bind value to state with $bindState.",
    },
    Select: {
      props: z.object({ label: text, options: z.array(z.string()), value: boundText }),
      description: "A pick-one menu. Bind value to state with $bindState.",
    },
    Chips: {
      props: z.object({ label: maybeText, options: z.array(z.string()), value: boundList }),
      description: "Pick-several chips. Bind value (a list of the chosen options) to state with $bindState.",
    },
  },
  actions: {
    ask: {
      params: z.object({ message: z.string(), data: z.unknown().optional() }),
      description:
        "Send message to you as the owner's next message, as if they typed it. Params: { message: string, data?: any }. data is optional: form values to send along, such as { \"$state\": \"/form\" }.",
    },
    open_url: {
      params: z.object({ url: z.string() }),
      description: "Open a link in the owner's browser. Params: { url: string }.",
    },
  },
});

/** The root component of every card. */
export const ROOT = "Card";
/** Components that hold what the owner typed or picked. */
export const CONTROLS = ["TextInput", "Select", "Chips"];
/** The fence a reply puts a json-render block in. */
export const SPEC_FENCE = "```json-render";
