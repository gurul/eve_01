// The page: the screens before the chat (flow.ts decides which), then the chat itself (Shell.tsx).

import { useEffect, useMemo, useState } from "react";
import type { Bridge } from "../nativeBridge";
import type { Where } from "./api";
import { Flow, type Phase, type Spent } from "./flow";
import { Pair, Restarting, SignIn, Waiting } from "./Gate";
import type { WebHost } from "./host";
import { Setup } from "./Setup";
import { Shell } from "./Shell";
import type { Transport } from "./transport";
import type { VoiceController } from "./voice";

export interface RootProps {
  where: Where;
  /** The fragment's secret, spent once (main.tsx); every call gets the same outcome. */
  spent(): Promise<Spent>;
  bridge: Bridge;
  host: WebHost;
  transport: Transport;
  voice: VoiceController;
  /** The host's 401s reach the flow through this (main.tsx wires it before the first request). */
  onAuthLost(listener: () => void): () => void;
  /** A new secret arrived in the fragment of this open page (spent() now spends it). */
  onSecret(listener: () => void): () => void;
}

export function Root({ where, spent, bridge, host, transport, voice, onAuthLost, onSecret }: RootProps) {
  const [phase, setPhase] = useState<Phase>({ name: "boot" });
  const flow = useMemo(() => new Flow({ where, onPhase: setPhase }), [where]);

  // The tab says where the page is: "Set up eve" while setting up, "Pair with eve" before pairing, else "eve".
  useEffect(() => {
    document.title = phase.name === "setup" ? "Set up eve" : phase.name === "pair" ? "Pair with eve" : "eve";
  }, [phase.name]);

  useEffect(() => {
    void flow.boot(spent());
    const offAuth = onAuthLost(() => flow.authLost());
    const offSecret = onSecret(() => void flow.boot(spent()));
    return () => {
      offAuth();
      offSecret();
      flow.dispose();
    };
  }, [flow, spent, onAuthLost, onSecret]);

  switch (phase.name) {
    case "boot":
      return <Waiting message="Connecting to eve…" />;
    case "signin":
      return <SignIn note={phase.note} onRetry={() => void flow.decide()} />;
    case "pair":
      return <Pair note={phase.note} onPaired={() => void flow.decide("", true)} />;
    case "setup":
      return <Setup state={phase.state} onChanged={() => void flow.decide()} />;
    case "restarting":
      return <Restarting late={phase.late} />;
    case "unreachable":
      return <Waiting message={phase.message} onRetry={() => void flow.decide()} />;
    case "chat":
      return (
        <Shell
          where={where}
          bridge={bridge}
          host={host}
          transport={transport}
          voice={voice}
          onAuthLost={() => flow.authLost()}
          onSignedOut={() => flow.signedOut()}
        />
      );
  }
}
