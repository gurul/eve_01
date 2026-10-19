# System design

eve runs on a Mac. Eve Link and the optional desk devices are interfaces to that
host; the iPhone does not run the brain. The diagrams show logical components and
their main connections, rather than every module or optional integration.

## Full eve

```mermaid
flowchart TB
    subgraph surfaces["Owner interfaces"]
        phone["Eve Link on iPhone<br/>Chat, photos, voice, Apps and Work"]
        mini["Optional Mini App"]
        telegram["Optional Telegram door"]
        desktop["Mac app, widget and CLI"]
        desk["Optional Voice PE and StackChan"]
    end

    tunnel["Cloudflare tunnel<br/>HTTPS and secure WebSocket"]

    subgraph host["Mac host: Python asyncio daemon"]
        lifecycle["daemon.py<br/>Startup and component coordination"]
        api["miniapp.py, stick_link.py, link_chat.py<br/>Authenticated phone and web endpoints"]
        calls["phone_call.py, desk_call.py<br/>Push-to-talk transcription and reply audio"]
        ipc["Local JSON IPC<br/>CLI, hooks and native UI"]
        body["USB serial and controller links<br/>Face, head, camera, buttons and audio"]
        wake["Local wake-word detection<br/>Separate live voice backend"]
        brain["telegram.py: TelegramInlet<br/>Shared chat and push-to-talk brain"]
        routing["Code shortcuts and tool routing<br/>App commands, tool selection and search"]
        gate["Code-enforced tool policies<br/>tool_gate.py, consent.py and executor approvals"]
        registry["app_registry.py and app modules<br/>Enabled tools, controls and phone sheets"]
        accounts["Connected-account adapters<br/>Google REST, GitHub and Maps MCP, Composio"]
        tasks["Computer-task routing<br/>Browser lanes and Codex Computer Use"]
        jobs["Watches, chief and Updates<br/>Background checks, jobs and suggestions"]
        memory["Memory and local stores<br/>Chat log, opt-in transcripts, records,<br/>profile, dream, mem0 and notes vault"]
        coding["Claude Code hooks and work sessions<br/>Session updates, questions and replies"]
        notify["notify.py<br/>Phone notifications"]
    end

    subgraph services["External services and devices"]
        models["OpenRouter<br/>Shared model API gateway"]
        live["OpenAI live voice<br/>Optional wake-word voice path"]
        apps["Lights, Spotify, Fire TV,<br/>OBSBOT, Pika and custom apps"]
        connected["Google, GitHub, Maps,<br/>Slack, Notion, Linear and other accounts"]
        computer["Browser and Mac applications"]
        search["Search and web-reading providers"]
        ntfy["ntfy service and phone app"]
    end

    phone <-->|"Paired link"| tunnel
    mini <--> tunnel
    tunnel <--> api
    api <-->|"Chat and uploads"| brain
    api <-->|"Voice transport"| calls
    calls <-->|"Transcribed turns and replies"| brain
    telegram <-->|"Bot API: outbound HTTPS polling"| brain
    desktop <--> ipc
    ipc <--> brain
    desk <-->|"USB serial"| body
    body <--> calls
    body <--> wake
    wake <--> live
    wake -->|"Tool requests"| gate
    brain <--> models
    brain --> routing
    routing --> gate
    routing --> search
    gate <-->|"Required approval: exact act and owner yes"| brain
    gate --> registry
    gate --> accounts
    gate --> tasks
    registry <--> apps
    accounts <--> connected
    tasks <--> computer
    brain <--> memory
    wake <--> memory
    jobs <--> brain
    jobs <--> memory
    coding <--> ipc
    brain --> notify
    notify --> ntfy
```

`daemon.py` builds and connects the host components. Telegram transport is optional:
`EVE_TELEGRAM=app` retains `TelegramInlet` for Eve Link, calls and background jobs
without sending messages to Telegram. Voice PE presses record the Mac microphone;
Eve Link records the phone microphone. Wake-word voice uses a separate live backend.

The tool-policy box represents several code checks, including connected-account
policies and final approvals inside individual executors. Reads and routine
reversible work can proceed; consequential actions require the owner's approval.
When a turn has read outside content, the approval question must show the complete
act. A model's proposed action does not grant consent. Computer tasks also relay
their own permissions, progress and cancellation through eve.

OpenRouter supplies the shared model calls. The separate live voice connection and
Codex's own sign-in or optional API billing are distinct paths. Device adapters use
their own protocols: LAN, BLE, USB, service APIs or MCP. Pika can stream video from
the Mac directly to the phone over WebRTC, with the tunnel as fallback; that media
path is omitted above for clarity. Storage is on the Mac, but model requests and
enabled external integrations send the relevant inputs to their providers.

## eve lite

eve lite is an alternative deployment selected by `EVE_PROFILE=lite`. It starts
`lite.py` instead of the full daemon and reuses the shared chat, call, memory and
tool-policy components. It does not start the full deployment's device links,
desktop automation or coding sessions.

```mermaid
flowchart LR
    mac["Mac browser"]
    phone["Paired phone or laptop<br/>Same Wi-Fi"]

    subgraph host["Mac host: eve lite"]
        local["lite_web.py<br/>Local HTTP page"]
        lan["lite_web.py and lite_tls.py<br/>Authenticated LAN HTTPS page"]
        rtc["lite_rtc.py<br/>WebRTC chat DataChannel and voice audio"]
        brain["lite.py<br/>Shared TelegramInlet and PhoneCalls"]
        policy["Code-enforced tool policies"]
        connect["composio_connect.py<br/>Connected-account tools"]
        stores["Local chat log and opt-in memory<br/>Watches and nightly dream"]
    end

    models["OpenRouter"]
    apps["Composio and connected services"]

    mac <--> local
    local <--> brain
    phone <-->|"Page, pairing and signaling"| lan
    phone <-->|"Chat and push-to-talk"| rtc
    rtc <--> brain
    brain <--> models
    brain <--> stores
    brain --> policy
    policy <-->|"Required owner approval"| brain
    policy --> connect
    connect <--> apps
```

The lite phone path stays on the local network and does not need the full
deployment's Cloudflare tunnel or native iPhone app. Composio sign-in is optional;
OpenRouter sign-in enables the brain.

## Implementation guides

- [Repository overview and component table](../README.md#how-it-works)
- [Eve Link pairing and Action Button](action-button.md), [chat](app-chat.md) and [voice mode](voice-mode.md)
- [App registry and adding an app](adding-an-app.md)
- [Connected accounts and their policies](apps.md)
- [Computer-task routing](stackchan/routing.md) and [Codex integration](codex-computer-use/README.md)
- [Conversation memory](stackchan/memory.md) and [chief's job lifecycle](stackchan/chief.md)
- [eve lite deployment](lite.md) and [verified state machines](verification.md)
