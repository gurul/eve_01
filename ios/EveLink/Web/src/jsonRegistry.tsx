// How each catalog component (jsonCatalog.ts) is drawn. json-render decides
// what is on the card; the look is OpenUI's own React components, the same
// ones an openui-lang card uses, so a card reads the same whichever engine
// wrote it. Every component takes whatever the model sent and draws what it
// can: a missing list is an empty list, never a throw.
import {
  Accordion, AccordionContent, AccordionItem, AccordionTrigger,
  BarChartCondensed, Button as UiButton, Buttons as UiButtons, Callout as UiCallout, Card as UiCard, CardHeader,
  Chips as UiChips, EntityList, FollowUpBlock, FollowUpItem, ImageBlock, Input, LineChartCondensed, ListBlock,
  ListItem, MarkDownRenderer, PieChart as UiPieChart, ScrollableTable, Select as UiSelect, SelectContent,
  SelectItem, SelectTrigger, SelectValue, Steps as UiSteps, StepsItem, TableBody, TableCell, TableHead,
  TableHeader, TableRow, Tabs as UiTabs, TabsContent, TabsList, TabsTrigger, Tag, TagBlock, TextContentWrapper,
} from "@openuidev/react-ui";
import { defineRegistry, useBoundProp } from "@json-render/react";
import { Children, createContext, useContext, useId, useState, type ReactNode } from "react";
import { catalog } from "./jsonCatalog.ts";

/** What a card can ask of the page around it. */
export interface CardHost {
  /** Send `text` to eve as the owner's next message. */
  ask: (text: string) => void;
  open: (url: string) => void;
  /** True while the block is still arriving: controls wait. */
  streaming: boolean;
}
export const CardHostContext = createContext<CardHost>({ ask: () => {}, open: () => {}, streaming: false });

const list = <T,>(v: T[] | null | undefined): T[] => (Array.isArray(v) ? v : []);
const str = (v: unknown): string => (typeof v === "string" || typeof v === "number" ? String(v) : "");

function chartRows(labels: unknown, series: unknown): Record<string, string | number>[] {
  const named = list(series as { name?: unknown; values?: unknown }[]).filter((s) => s && str(s.name));
  return list(labels as unknown[]).map((label, i) => {
    const row: Record<string, string | number> = { category: str(label) };
    for (const s of named) {
      const v = list(s.values as unknown[])[i];
      row[str(s.name)] = typeof v === "number" ? v : 0;
    }
    return row;
  });
}

function Field({ label, children }: { label: string; children: (id: string) => ReactNode }) {
  const id = useId();
  return (
    <div className="eve-spec-field">
      {label && <label className="eve-spec-field__label" htmlFor={id}>{label}</label>}
      {children(id)}
    </div>
  );
}

function PanelTabs({ names, children }: { names: string[]; children: ReactNode }) {
  const panels = Children.toArray(children);
  const [active, setActive] = useState("0");
  if (!panels.length) return null;
  return (
    <UiTabs value={active} onValueChange={setActive}>
      <TabsList>
        {panels.map((_, i) => <TabsTrigger key={i} value={String(i)} text={names[i] || `Tab ${i + 1}`} />)}
      </TabsList>
      {panels.map((panel, i) => <TabsContent key={i} value={String(i)}>{panel}</TabsContent>)}
    </UiTabs>
  );
}

function PanelAccordion({ names, children }: { names: string[]; children: ReactNode }) {
  const panels = Children.toArray(children);
  const [open, setOpen] = useState("0");
  if (!panels.length) return null;
  return (
    <Accordion type="single" collapsible value={open} onValueChange={setOpen}>
      {panels.map((panel, i) => (
        <AccordionItem key={i} value={String(i)}>
          <AccordionTrigger text={names[i] || `Section ${i + 1}`} />
          <AccordionContent>{panel}</AccordionContent>
        </AccordionItem>
      ))}
    </Accordion>
  );
}

function LinkList({ variant, items }: {
  variant: "number" | "image";
  items: { title?: unknown; subtitle?: unknown; image?: { src?: unknown; alt?: unknown } | null; url?: unknown }[];
}) {
  const host = useContext(CardHostContext);
  const hasSubtitle = items.some((item) => !!str(item?.subtitle));
  return (
    <ListBlock variant={variant}>
      {items.filter(Boolean).map((item, i) => {
        const url = str(item.url);
        const src = str(item.image?.src);
        return (
          <ListItem
            key={i}
            title={str(item.title)}
            subtitle={str(item.subtitle) || undefined}
            listHasSubtitle={hasSubtitle}
            image={variant === "image" && src ? { src, alt: str(item.image?.alt) } : undefined}
            onClick={url ? () => host.open(url) : undefined}
          />
        );
      })}
    </ListBlock>
  );
}

function AskList({ items }: { items: string[] }) {
  const host = useContext(CardHostContext);
  return (
    <FollowUpBlock>
      {items.map((text, i) => <FollowUpItem key={i} text={text} onClick={() => host.ask(text)} />)}
    </FollowUpBlock>
  );
}

function PressButton({ label, variant, press }: { label: string; variant: "primary" | "secondary"; press: () => void }) {
  const host = useContext(CardHostContext);
  return <UiButton variant={variant} size="medium" disabled={host.streaming} onClick={press}>{label}</UiButton>;
}

export const { registry } = defineRegistry(catalog, {
  components: {
    Card: ({ props, children }) => (
      <UiCard variant="card" width="full" style={{ display: "flex", flexDirection: "column", gap: "var(--openui-space-m)", minWidth: 0 }}>
        {(props.title || props.subtitle) && <CardHeader title={props.title ?? undefined} subtitle={props.subtitle ?? undefined} />}
        {children}
      </UiCard>
    ),
    Stack: ({ props, children }) => (
      <div style={{ display: "flex", flexDirection: props.direction === "row" ? "row" : "column", flexWrap: "wrap", gap: "var(--openui-space-m)", minWidth: 0 }}>
        {children}
      </div>
    ),
    Text: ({ props }) => <TextContentWrapper textMarkdown={str(props.text)} />,
    Callout: ({ props }) => (
      <UiCallout
        variant={props.variant ?? "info"}
        title={str(props.title)}
        description={props.description ? <MarkDownRenderer textMarkdown={str(props.description)} /> : undefined}
      />
    ),
    Table: ({ props }) => {
      const columns = list(props.columns);
      if (!columns.length) return null;
      return (
        <ScrollableTable>
          <TableHeader>
            <TableRow>{columns.map((c, i) => <TableHead key={i}>{str(c)}</TableHead>)}</TableRow>
          </TableHeader>
          <TableBody>
            {list(props.rows).map((row, r) => (
              <TableRow key={r}>{columns.map((_, c) => <TableCell key={c}>{str(list(row)[c])}</TableCell>)}</TableRow>
            ))}
          </TableBody>
        </ScrollableTable>
      );
    },
    Facts: ({ props }) => (
      <EntityList
        size="default"
        rows={list(props.rows).filter(Boolean).map((r) => ({ left: str(r.label), right: str(r.value), rightVariant: "text" as const }))}
      />
    ),
    List: ({ props }) => <LinkList variant={props.variant === "image" ? "image" : "number"} items={list(props.items)} />,
    Steps: ({ props }) => (
      <UiSteps>
        {list(props.items).filter(Boolean).map((item, i) => (
          <StepsItem key={i} number={i + 1} title={str(item.title)} details={<MarkDownRenderer textMarkdown={str(item.details)} />} />
        ))}
      </UiSteps>
    ),
    BarChart: ({ props }) => {
      const data = chartRows(props.labels, props.series);
      if (!data.length) return null;
      return <BarChartCondensed data={data} categoryKey="category" variant={props.variant ?? undefined}
        xAxisLabel={props.xLabel ?? undefined} yAxisLabel={props.yLabel ?? undefined} isAnimationActive={false} />;
    },
    LineChart: ({ props }) => {
      const data = chartRows(props.labels, props.series);
      if (!data.length) return null;
      return <LineChartCondensed data={data} categoryKey="category"
        xAxisLabel={props.xLabel ?? undefined} yAxisLabel={props.yLabel ?? undefined} isAnimationActive={false} />;
    },
    PieChart: ({ props }) => {
      const values = list(props.values);
      const data = list(props.labels).map((label, i) => ({ category: str(label), value: typeof values[i] === "number" ? values[i] : 0 }));
      if (!data.length) return null;
      return <UiPieChart data={data} categoryKey="category" dataKey="value" variant={props.variant ?? "pie"} appearance="circular" isAnimationActive={false} />;
    },
    Image: ({ props }) => (str(props.src) ? <ImageBlock src={str(props.src)} alt={str(props.alt)} /> : null),
    Tags: ({ props }) => <TagBlock>{list(props.tags).map((tag, i) => <Tag key={i} text={str(tag)} />)}</TagBlock>,
    Buttons: ({ props, children }) => (
      <UiButtons variant={props.direction === "column" ? "vertical" : "horizontal"}>
        {children as never}
      </UiButtons>
    ),
    Button: ({ props, emit }) => (
      <PressButton label={str(props.label)} variant={props.variant === "secondary" ? "secondary" : "primary"} press={() => emit("press")} />
    ),
    FollowUps: ({ props }) => <AskList items={list(props.items).map(str).filter(Boolean)} />,
    Tabs: ({ props, children }) => <PanelTabs names={list(props.tabs).map(str)}>{children}</PanelTabs>,
    Accordion: ({ props, children }) => <PanelAccordion names={list(props.sections).map(str)}>{children}</PanelAccordion>,
    TextInput: ({ props, bindings }) => {
      const [value, setValue] = useBoundProp<string>(typeof props.value === "string" ? props.value : undefined, bindings?.value);
      return (
        <Field label={str(props.label)}>
          {(id) => <Input id={id} type="text" placeholder={props.placeholder ?? ""} value={value ?? ""}
            onChange={(e: React.ChangeEvent<HTMLInputElement>) => setValue(e.target.value)} />}
        </Field>
      );
    },
    Select: ({ props, bindings }) => {
      const [value, setValue] = useBoundProp<string>(typeof props.value === "string" ? props.value : undefined, bindings?.value);
      return (
        <Field label={str(props.label)}>
          {() => (
            <UiSelect value={value ?? ""} onValueChange={setValue} size="md">
              <SelectTrigger><SelectValue placeholder="Select…" /></SelectTrigger>
              <SelectContent>
                {list(props.options).map(str).filter(Boolean).map((o) => <SelectItem key={o} value={o}>{o}</SelectItem>)}
              </SelectContent>
            </UiSelect>
          )}
        </Field>
      );
    },
    Chips: ({ props, bindings }) => {
      const [value, setValue] = useBoundProp<string[]>(Array.isArray(props.value) ? props.value : undefined, bindings?.value);
      const chosen = list(value);
      return (
        <Field label={str(props.label)}>
          {() => (
            <UiChips
              type="multiple"
              items={list(props.options).map(str).filter(Boolean).map((o) => ({ value: o, label: o }))}
              selected={chosen}
              onToggle={(o) => setValue(chosen.includes(o) ? chosen.filter((c) => c !== o) : [...chosen, o])}
            />
          )}
        </Field>
      );
    },
  },
  // The page's own handlers do the work (JsonCard.tsx passes them to the
  // provider, bound to one card); these keep the registry whole on its own.
  actions: { ask: async () => {}, open_url: async () => {} },
});
