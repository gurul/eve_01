// eve's controls as native sheets (docs/app-chat.md, "Work"): Music, Lights and the TV remote, the Telegram Mini
// App's three cards (miniapp_page.html) with the same actions, drawn in SwiftUI. Every press goes to the card's own
// panel on the Mac (ControlsClient.swift, link_chat.py); what the Mac answers is what the sheet shows.
import SwiftUI
import UIKit

/// The sheet for one control, in a navigation stack with a Done button.
struct ControlSheet: View {
    let kind: ControlKind
    let client: WorkClient
    let lights: [LightName]
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                switch kind {
                case .spotify: MusicControls(client: client)
                case .lights: LightsControls(client: client, lights: lights)
                case .firetv: RemoteControls(client: client)
                case .memory: MemoryControls(client: client)
                case .notes: NotesControls(client: client)
                case .pika: PikaControls(client: client)
                case .obsbot: ObsbotControls(client: client)
                }
            }
            .background(Color.eveBackground)
            .navigationTitle(kind.title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
        }
        .presentationDragIndicator(.visible)
    }
}

/// The muted line under a control: what the Mac said last. Read out by VoiceOver when it changes.
struct SaidLine: View {
    let text: String
    var error = false

    var body: some View {
        Text(text.isEmpty ? " " : text)
            .font(.footnote)
            .foregroundStyle(error ? Color.red : Color.secondary)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity)
            .accessibilityHidden(text.isEmpty)
            .onChange(of: text) { _, now in
                if !now.isEmpty { AccessibilityNotification.Announcement(now).post() }
            }
    }
}

private let noAnswer = "eve didn't answer. Is the Mac awake?"

// ---- Music ---------------------------------------------------------------------------------------------------------

private struct MusicControls: View {
    let client: WorkClient
    @State private var state: MusicState?
    @State private var line = ""
    @State private var failed = false
    @State private var volume: Double = 50
    @State private var dragging = false
    @State private var taps = 0
    @State private var fails = 0

    var body: some View {
        ScrollView {
            VStack(spacing: 28) {
                artwork
                VStack(spacing: 4) {
                    Text(state?.track ?? (state == nil ? " " : "Nothing playing"))
                        .font(.title3.weight(.semibold))
                        .lineLimit(2)
                    Text(state?.byLine ?? " ")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
                .multilineTextAlignment(.center)
                .accessibilityElement(children: .combine)
                transport
                volumeRow
                if let state, !state.devices.isEmpty || state.mode != nil {
                    VStack(spacing: 12) {
                        if !state.devices.isEmpty { devicePicker(state) }
                        if let mode = state.mode {
                            Toggle("Voice PE Spotify mode", isOn: Binding(
                                get: { mode }, set: { on in send(["action": "mode", "on": on]) }))
                                .font(.subheadline)
                        }
                    }
                }
                SaidLine(text: line, error: failed)
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 20)
        }
        .refreshable { await call(["action": "status"]) }
        .task { await call(["action": "status"]) }
        .sensoryFeedback(.impact(weight: .light), trigger: taps)
        .sensoryFeedback(.error, trigger: fails)
    }

    private var artwork: some View {
        RoundedRectangle(cornerRadius: 14, style: .continuous)
            .fill(Color.eveFill)
            .aspectRatio(1, contentMode: .fit)
            .frame(maxWidth: 280)
            .overlay { Image(systemName: "music.note").font(.system(size: 44)).foregroundStyle(.secondary) }
            .overlay {
                if let art = state?.art {
                    AsyncImage(url: art) { image in
                        image.resizable().scaledToFill()
                    } placeholder: {
                        Color.clear
                    }
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            .padding(.horizontal, 24)
            .accessibilityHidden(true)
    }

    private var transport: some View {
        let playing = state?.playing ?? false
        return HStack(spacing: 36) {
            Button { send(["action": "previous"]) } label: {
                Image(systemName: "backward.fill").font(.title2).frame(width: 56, height: 56)
            }
            .accessibilityLabel("Previous track")
            Button { send(["action": playing ? "pause" : "play"]) } label: {
                Image(systemName: playing ? "pause.fill" : "play.fill")
                    .font(.title)
                    .foregroundStyle(Color.eveBackground)
                    .frame(width: 72, height: 72)
                    .background(Circle().fill(Color.primary))
            }
            .accessibilityLabel(playing ? "Pause" : "Play")
            Button { send(["action": "next"]) } label: {
                Image(systemName: "forward.fill").font(.title2).frame(width: 56, height: 56)
            }
            .accessibilityLabel("Next track")
        }
        .buttonStyle(PressStyle())
        .foregroundStyle(.primary)
    }

    private var volumeRow: some View {
        HStack(spacing: 12) {
            Image(systemName: "speaker.fill").foregroundStyle(.secondary).accessibilityHidden(true)
            Slider(value: $volume, in: 0...100, step: 2) { editing in
                dragging = editing
                if !editing { send(["action": "volume", "volume": Int(volume)]) }
            }
            .accessibilityLabel("Volume")
            .accessibilityValue("\(Int(volume)) percent")
            Image(systemName: "speaker.wave.3.fill").foregroundStyle(.secondary).accessibilityHidden(true)
        }
        .disabled(state?.volume == nil)
    }

    private func devicePicker(_ state: MusicState) -> some View {
        let active = state.devices.first(where: \.active)
        return Menu {
            ForEach(state.devices) { d in
                Button {
                    send(["action": "transfer", "device": d.name])
                } label: {
                    if d.active { Label(d.name, systemImage: "checkmark") } else { Text(d.name) }
                }
                .disabled(d.restricted)
            }
        } label: {
            HStack(spacing: 8) {
                Image(systemName: "hifispeaker").accessibilityHidden(true)
                Text(active?.name ?? "Choose a device").lineLimit(1)
                Image(systemName: "chevron.up.chevron.down").font(.caption).accessibilityHidden(true)
            }
            .font(.subheadline)
            .foregroundStyle(.secondary)
            .frame(minHeight: 44)
        }
        .accessibilityLabel("Playing on \(active?.name ?? "no device")")
    }

    private func send(_ body: [String: Any]) {
        taps += 1
        let body = body
        Task { await call(body) }
    }

    private func call(_ body: [String: Any]) async {
        switch await client.music(body) {
        case .success(let s):
            if s.ok {
                state = s
                if !dragging, let v = s.volume { volume = Double(v) }
            }
            failed = !s.ok
            line = s.line.isEmpty && !s.ok ? "Something went wrong." : s.line
            if !s.ok, body["action"] as? String != "status" { fails += 1 }
        case .failure(let f):
            failed = true
            line = f.message.isEmpty ? noAnswer : f.message
            fails += 1
        }
    }
}

// ---- Lights --------------------------------------------------------------------------------------------------------

private struct LightsControls: View {
    let client: WorkClient
    let lights: [LightName]
    @State private var target = "all"
    @State private var brightness: Double = 100
    @State private var picked = Color.white
    @State private var line = ""
    @State private var failed = false
    @State private var taps = 0
    @State private var done = 0
    @State private var fails = 0
    @State private var colorBusy = false
    @State private var colorNext: String?

    private var choices: [LightChoice] { LightChoice.choices(lights) }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                if choices.count > 1 { targetPicker }
                HStack(spacing: 12) {
                    powerButton("On", on: true)
                    powerButton("Off", on: false)
                }
                swatches
                brightnessRow
                SaidLine(text: line, error: failed)
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 20)
        }
        .refreshable { await status() }
        .task { await status() }
        .sensoryFeedback(.impact(weight: .light), trigger: taps)
        .sensoryFeedback(.success, trigger: done)
        .sensoryFeedback(.error, trigger: fails)
    }

    private var targetPicker: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(choices) { c in
                    Button { target = c.value } label: {
                        Text(c.label)
                            .font(.subheadline.weight(target == c.value ? .semibold : .regular))
                            .foregroundStyle(target == c.value ? Color.eveBackground : Color.primary)
                            .padding(.horizontal, 16)
                            .frame(minHeight: 44)
                            .background(Capsule().fill(target == c.value ? Color.primary : Color.eveFill))
                    }
                    .buttonStyle(PressStyle())
                    .accessibilityAddTraits(target == c.value ? .isSelected : [])
                }
            }
        }
        .accessibilityLabel("Which lights")
    }

    private func powerButton(_ title: String, on: Bool) -> some View {
        Button { set(["power": on]) } label: {
            Label(title, systemImage: on ? "lightbulb.fill" : "lightbulb")
                .font(.body.weight(.medium))
                .frame(maxWidth: .infinity, minHeight: 52)
                .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Color.eveFill))
                .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Color.eveEdge))
        }
        .buttonStyle(PressStyle())
        .foregroundStyle(.primary)
        .accessibilityLabel("Turn \(target == "all" ? "the lights" : target) \(on ? "on" : "off")")
    }

    private var swatches: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 44, maximum: 56), spacing: 12)], spacing: 12) {
            ForEach(LightSwatch.all, id: \.name) { swatch in
                Button { set(["color": swatch.name]) } label: {
                    Circle()
                        .fill(color(swatch.hex))
                        .overlay(Circle().strokeBorder(Color.eveEdge, lineWidth: 1))
                        .frame(width: 44, height: 44)
                }
                .buttonStyle(PressStyle())
                .accessibilityLabel(swatch.name.capitalized)
            }
            ColorPicker("Any colour", selection: $picked, supportsOpacity: false)
                .labelsHidden()
                .frame(width: 44, height: 44)
                .accessibilityLabel("Pick any colour")
        }
        .onChange(of: picked) { _, now in
            Task { await sendColor(hex(now)) }
        }
    }

    private var brightnessRow: some View {
        HStack(spacing: 12) {
            Image(systemName: "sun.min").foregroundStyle(.secondary).accessibilityHidden(true)
            Slider(value: $brightness, in: 1...100, step: 1) { editing in
                if !editing { set(["brightness": Int(brightness)]) }
            }
            .accessibilityLabel("Brightness")
            .accessibilityValue("\(Int(brightness)) percent")
            Image(systemName: "sun.max.fill").foregroundStyle(.secondary).accessibilityHidden(true)
        }
    }

    private func color(_ hex: String) -> Color {
        guard let (r, g, b) = LightSwatch.rgb(hex) else { return .gray }
        return Color(red: r, green: g, blue: b)
    }

    private func hex(_ c: Color) -> String {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        UIColor(c).getRed(&r, green: &g, blue: &b, alpha: &a)
        return LightSwatch.hex(red: Double(r), green: Double(g), blue: Double(b))
    }

    /// A drag on the colour picker: one request on its way, only the newest colour waiting behind it (a Bluetooth
    /// light takes a moment, and replaying every colour passed would replay the whole drag).
    private func sendColor(_ hex: String) async {
        if colorBusy { colorNext = hex; return }
        colorBusy = true
        await change(["color": hex], quiet: true)
        colorBusy = false
        if let next = colorNext {
            colorNext = nil
            await sendColor(next)
        }
    }

    private func set(_ change: [String: Any]) {
        taps += 1
        let change = change
        Task { await self.change(change, quiet: false) }
    }

    private func change(_ change: [String: Any], quiet: Bool) async {
        var body: [String: Any] = ["action": "set", "target": target]
        for (k, v) in change { body[k] = v }
        switch await client.lights(body) {
        case .success(let r):
            failed = !r.ok
            line = r.said.isEmpty && !r.ok ? "Something went wrong." : r.said
            if r.ok { if !quiet { done += 1 } } else { fails += 1 }
        case .failure(let f):
            failed = true
            line = f.message.isEmpty ? noAnswer : f.message
            fails += 1
        }
    }

    private func status() async {
        switch await client.lights(["action": "status"]) {
        case .success(let r):
            failed = !r.ok
            line = r.said
            if let b = r.brightness { brightness = Double(max(1, b)) }
        case .failure(let f):
            failed = true
            line = f.message
        }
    }
}

// ---- the TV remote -------------------------------------------------------------------------------------------------

private struct RemoteControls: View {
    let client: WorkClient
    @State private var tv: TvState?
    @State private var now = "Reading the TV…"
    @State private var apps: [String] = []
    @State private var muted: Bool?
    @State private var volume: Double = 0
    @State private var dragging = false
    @State private var queue = TvQueue()
    @State private var line = ""
    @State private var failed = false
    @State private var taps = 0
    @State private var fails = 0

    private static let step = 10              // the − and + keys, in percent (firetv.VOLUME_STEP)

    var body: some View {
        ScrollView {
            VStack(spacing: 28) {
                header
                if !apps.isEmpty { appChips }
                dpad
                keys
                volumeRow
                SaidLine(text: line, error: failed)
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 16)
        }
        .task { await status() }
        .refreshable { await status() }
        .sensoryFeedback(.impact(weight: .light), trigger: taps)
        .sensoryFeedback(.error, trigger: fails)
    }

    private var header: some View {
        HStack(spacing: 16) {
            Text(now)
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button {
                let on = tv?.isOn ?? false
                now = on ? "Turning the TV off…" : "Turning the TV on…"
                press(TvPress(action: on ? "power_off" : "power_on"))
            } label: {
                Image(systemName: "power")
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(tv?.isOn == true ? Color.eveBackground : Color.primary)
                    .frame(width: 52, height: 52)
                    .background(Circle().fill(tv?.isOn == true ? Color.primary : Color.eveFill))
                    .overlay(Circle().strokeBorder(Color.eveEdge))
            }
            .buttonStyle(PressStyle())
            .accessibilityLabel(tv?.isOn == true ? "Turn the TV off" : "Turn the TV on")
        }
    }

    private var appChips: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(apps, id: \.self) { name in
                    let current = tv?.app == name
                    Button { press(TvPress(action: "open_app", app: name)) } label: {
                        Text(name)
                            .font(.subheadline.weight(current ? .semibold : .regular))
                            .foregroundStyle(current ? Color.eveBackground : Color.primary)
                            .padding(.horizontal, 16)
                            .frame(minHeight: 44)
                            .background(Capsule().fill(current ? Color.primary : Color.eveFill))
                    }
                    .buttonStyle(PressStyle())
                    .accessibilityLabel("Open \(name)")
                    .accessibilityAddTraits(current ? .isSelected : [])
                }
            }
        }
    }

    private var dpad: some View {
        let side: CGFloat = 232
        return ZStack {
            Circle().fill(Color.eveFill).overlay(Circle().strokeBorder(Color.eveEdge))
            VStack(spacing: 0) {
                padKey("up", "chevron.up", "Up")
                HStack(spacing: 0) {
                    padKey("left", "chevron.left", "Left")
                    Button { press(TvPress(action: "select")) } label: {
                        Text("OK")
                            .font(.headline)
                            .frame(width: 84, height: 84)
                            .background(Circle().fill(Color.eveSelected))
                            .overlay(Circle().strokeBorder(Color.eveEdge))
                    }
                    .buttonStyle(PressStyle())
                    .accessibilityLabel("OK")
                    padKey("right", "chevron.right", "Right")
                }
                padKey("down", "chevron.down", "Down")
            }
        }
        .frame(width: side, height: side)
        .foregroundStyle(.primary)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Direction pad")
    }

    private func padKey(_ action: String, _ symbol: String, _ label: String) -> some View {
        Button { press(TvPress(action: action)) } label: {
            Image(systemName: symbol)
                .font(.title3.weight(.semibold))
                .frame(width: 64, height: 64)
                .contentShape(Rectangle())
        }
        .buttonStyle(PressStyle())
        .accessibilityLabel(label)
    }

    private var keys: some View {
        Grid(horizontalSpacing: 16, verticalSpacing: 16) {
            GridRow {
                key("arrow.uturn.backward", "Back") { press(TvPress(action: "back")) }
                key("house", "Home") { press(TvPress(action: "home")) }
                key("line.3.horizontal", "Menu") { press(TvPress(action: "menu")) }
            }
            GridRow {
                key("backward.fill", "Rewind") { press(TvPress(action: "rewind")) }
                key("playpause.fill", "Play or pause") { press(TvPress(action: "play_pause")) }
                key("forward.fill", "Fast-forward") { press(TvPress(action: "fast_forward")) }
            }
            GridRow {
                key("minus", "Volume down") { nudge(-1) }
                key(muted == true ? "speaker.slash.fill" : "speaker.fill", muted == true ? "Unmute" : "Mute") {
                    let mute = muted != true
                    muted = mute                                  // at once; the TV's answer corrects it
                    press(TvPress(action: mute ? "mute" : "unmute"))
                }
                key("plus", "Volume up") { nudge(1) }
            }
        }
    }

    private func key(_ symbol: String, _ label: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.title3)
                .frame(width: 64, height: 52)
                .background(Capsule().fill(Color.eveFill))
                .overlay(Capsule().strokeBorder(Color.eveEdge))
        }
        .buttonStyle(PressStyle())
        .foregroundStyle(.primary)
        .accessibilityLabel(label)
    }

    private var volumeRow: some View {
        HStack(spacing: 12) {
            Image(systemName: "speaker.fill").foregroundStyle(.secondary).accessibilityHidden(true)
            Slider(value: $volume, in: 0...100, step: 1) { editing in
                dragging = editing
                if !editing { press(TvPress(action: "volume", volume: Int(volume)), tap: false) }
            }
            .accessibilityLabel("TV volume")
            .accessibilityValue("\(Int(volume)) percent")
            Image(systemName: "speaker.wave.3.fill").foregroundStyle(.secondary).accessibilityHidden(true)
        }
        .disabled(tv?.volume == nil)
    }

    private func nudge(_ sign: Int) {
        volume = Double(max(0, min(100, Int(volume) + sign * Self.step)))
        press(TvPress(action: "volume_change", change: sign * Self.step))
    }

    /// A press waits its turn: the TV takes about a second, and gets the presses in the order they were made.
    private func press(_ p: TvPress, tap: Bool = true) {
        if tap { taps += 1 }
        queue.push(p)
        pump()
    }

    private func pump() {
        guard let p = queue.next() else { return }
        Task {
            let result = await client.tv(p.body)
            switch result {
            case .success(let s):
                paint(s)
                failed = !s.ok
                // a d-pad press says nothing when it worked: the screen is the answer
                line = s.ok && p.isNav ? "" : (s.line.isEmpty && !s.ok ? "Something went wrong." : s.line)
                if !s.ok {
                    fails += 1
                    if p.action == "mute" || p.action == "unmute", s.muted == nil { muted = p.action == "unmute" }
                }
            case .failure(let f):
                failed = true
                line = f.message.isEmpty ? noAnswer : f.message
                fails += 1
                if let shown = tv?.now { now = shown }
            }
            queue.finished()
            pump()
        }
    }

    private func paint(_ s: TvState) {
        if !s.apps.isEmpty, apps.isEmpty { apps = s.apps }
        if let shown = s.now, !shown.isEmpty { now = shown } else if !s.ok, tv?.now == nil { now = "TV not answering" }
        guard s.power != nil || s.volume != nil else { return }
        var merged = tv ?? s
        if s.power != nil { merged.power = s.power; merged.app = s.app }
        if let v = s.volume { merged.volume = v; if !dragging { volume = Double(v) } }
        if let m = s.muted { merged.muted = m; muted = m }
        if let shown = s.now { merged.now = shown }
        tv = merged
    }

    private func status() async {
        switch await client.tv(["action": "status"]) {
        case .success(let s):
            paint(s)
            failed = !s.ok
            line = s.ok ? "" : s.line
        case .failure(let f):
            now = noAnswer
            failed = true
            line = f.message
        }
    }
}

/// A press that dims while held, for the controls' own shapes.
// ---- Memory -------------------------------------------------------------------------------------------------------

/// What eve remembers (docs/stackchan/memory.md, "From the app"): the profile at rest, as sections of facts; a
/// search's facts, recalled meanings and the words said; and Forget, which previews counts and asks before anything
/// goes (link_memory.py). Every row's context menu has "Don't mention this again" (ChatGPT's control of that name):
/// eve leaves those words out of the profile and its searches, deletes nothing, and the Hidden card at the bottom
/// takes them back. The daemon's markdown is read into rows here (Shared/MemoryText.swift).
struct MemoryControls: View {
    let client: WorkClient
    @State private var query = ""
    @State private var searched = ""               // the query the results are for
    @State private var profile = MemoryProfile(updated: nil, intro: nil, sections: [])
    @State private var loaded = false
    @State private var reply: MemoryReply?
    @State private var line = ""
    @State private var failed = false
    @State private var busy = false
    @State private var expanded: Set<String> = []  // sections showing every row
    @State private var preview: MemoryReply?        // a forget preview waiting for the owner's yes
    @State private var adding = false               // the "Remember" prompt is up
    @State private var newMemory = ""
    @State private var hiddenItems: [String] = []   // "Don't mention this again", oldest first
    @State private var done = 0
    @State private var fails = 0
    @FocusState private var typing: Bool

    private static let rowsShown = 4

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                searchField
                if busy && reply == nil { ProgressView().frame(maxWidth: .infinity).padding(.top, 8) }
                if let reply, !searched.isEmpty {
                    results(reply)
                } else if loaded {
                    profileView
                }
                SaidLine(text: line, error: failed)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 16)
        }
        .scrollDismissesKeyboard(.interactively)
        .task {
            await loadProfile()
            #if DEBUG && targetEnvironment(simulator)
            if let q = DesignHook.memoryQuery, !q.isEmpty { query = q; await search() }
            #endif
        }
        .sensoryFeedback(.success, trigger: done)
        .sensoryFeedback(.error, trigger: fails)
        .confirmationDialog(preview?.line ?? "", isPresented: Binding(get: { preview != nil }, set: { if !$0 { preview = nil } }),
                            titleVisibility: .visible) {
            Button("Forget", role: .destructive) { Task { await apply() } }
            Button("Keep", role: .cancel) { preview = nil }
        } message: {
            Text("Forgotten everywhere: facts, what was said, meanings and notes. This cannot be undone.")
        }
        .alert("Remember", isPresented: $adding) {
            TextField("Something eve should keep", text: $newMemory)
            Button("Remember") { Task { await remember() } }
            Button("Cancel", role: .cancel) { newMemory = "" }
        } message: {
            Text("Kept for good, on eve's profile of you from the next message.")
        }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button { adding = true } label: { Image(systemName: "plus") }
                    .accessibilityLabel("Remember something")
            }
        }
    }

    // MARK: Search

    private var searchField: some View {
        HStack(spacing: 10) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary).accessibilityHidden(true)
            TextField("Search what eve remembers", text: $query)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(.search)
                .focused($typing)
                .onSubmit { Task { await search() } }
            if !query.isEmpty {
                Button {
                    query = ""
                    searched = ""
                    reply = nil
                    line = ""
                } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                }
                .accessibilityLabel("Clear")
            }
        }
        .padding(.horizontal, 14)
        .frame(minHeight: 44)
        .background(Capsule().fill(Color.eveFill))
    }

    // MARK: The profile

    @ViewBuilder private var profileView: some View {
        if profile.isEmpty {
            quiet(line.isEmpty ? "eve has nothing written down about you yet." : line)
            hiddenCard
        } else {
            HStack(alignment: .firstTextBaseline) {
                Text("What eve knows about you").font(.title3.weight(.semibold))
                Spacer()
                if let updated = profile.updated {
                    Text("to \(MemoryText.dayLabel(updated))").font(.caption).foregroundStyle(.secondary)
                }
            }
            .padding(.top, 4)
            ForEach(profile.sections) { section in
                card(section.title) {
                    let rows = expanded.contains(section.title) ? section.items : Array(section.items.prefix(Self.rowsShown))
                    ForEach(Array(rows.enumerated()), id: \.offset) { i, item in
                        row(item.text, caption: item.said.map { "said " + MemoryText.dayLabel($0) }, first: i == 0,
                            hide: item.text)
                    }
                    if section.items.count > Self.rowsShown {
                        Button {
                            withAnimation(.easeInOut(duration: 0.2)) {
                                if expanded.contains(section.title) { expanded.remove(section.title) } else { expanded.insert(section.title) }
                            }
                        } label: {
                            let hidden = section.items.count - Self.rowsShown
                            Text(expanded.contains(section.title) ? "Show less" : "\(hidden) more")
                                .font(.subheadline.weight(.medium))
                                .frame(maxWidth: .infinity, minHeight: 40)
                        }
                        .buttonStyle(PressStyle())
                    }
                }
            }
            hiddenCard
        }
    }

    /// What the owner said not to bring up again, each with Unhide. Nothing here was deleted.
    @ViewBuilder private var hiddenCard: some View {
        if !hiddenItems.isEmpty {
            card("Hidden") {
                ForEach(Array(hiddenItems.enumerated()), id: \.offset) { i, text in
                    VStack(alignment: .leading, spacing: 0) {
                        if i > 0 { Rectangle().fill(Color.eveEdge).frame(height: 1).padding(.bottom, 10) }
                        HStack(alignment: .firstTextBaseline, spacing: 12) {
                            Text(text)
                                .font(.body)
                                .foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            Button("Unhide") { Task { await unhide(text) } }
                                .font(.subheadline.weight(.medium))
                                .buttonStyle(PressStyle())
                                .frame(minHeight: 32)
                                .disabled(busy)
                                .accessibilityHint("eve can bring this up again")
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 8)
                    .padding(.bottom, 2)
                }
                Text("eve leaves these out of what it reads and finds. Nothing was deleted.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 12)
            }
        }
    }

    // MARK: Results

    @ViewBuilder private func results(_ r: MemoryReply) -> some View {
        if r.isEmpty {
            quiet(r.line.isEmpty ? "Nothing about “\(searched)”." : r.line)
        } else {
            if !r.hits.isEmpty {
                card("Facts") {
                    ForEach(Array(r.hits.enumerated()), id: \.offset) { i, f in
                        let item = MemoryText.item(f.line.hasPrefix("- ") ? String(f.line.dropFirst(2)) : f.line)
                        let when = item.said.map { "said " + MemoryText.dayLabel($0) } ?? f.src
                        row(item.text, caption: when, first: i == 0, hide: item.text)
                    }
                }
            }
            if !r.recalled.isEmpty {
                card("Remembered") {
                    ForEach(Array(r.recalled.enumerated()), id: \.offset) { i, text in
                        let (caption, body) = MemoryText.recalled(text)
                        row(body, caption: caption.isEmpty ? nil : caption, first: i == 0, hide: body)
                    }
                }
            }
            if !r.said.isEmpty {
                card("Said") {
                    ForEach(Array(r.said.enumerated()), id: \.offset) { i, s in
                        let who = MemoryText.whoLabel(s.who)
                        let caption = [s.day.isEmpty ? "" : MemoryText.dayLabel(s.day), s.time, who]
                            .filter { !$0.isEmpty }.joined(separator: " · ")
                        row(s.text, caption: caption, first: i == 0, quoted: true, hide: s.text)
                    }
                }
            }
            Button(role: .destructive) {
                Task { await previewForget() }
            } label: {
                Label("Forget everything about “\(searched)”", systemImage: "trash")
                    .font(.subheadline.weight(.medium))
                    .frame(maxWidth: .infinity, minHeight: 44)
            }
            .buttonStyle(PressStyle())
            .background(Capsule().fill(Color.eveFill))
            .disabled(busy)
            .padding(.top, 4)
            .accessibilityHint("Shows how much would be forgotten and asks first")
        }
    }

    // MARK: Pieces

    private func card<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(title.uppercased())
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 16)
                .padding(.top, 14)
                .padding(.bottom, 6)
            content()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(Color.eveFill))
    }

    /// One row; with `hide`, a long press offers "Don't mention this again" for those words.
    private func row(_ text: String, caption: String?, first: Bool, quoted: Bool = false, hide: String? = nil) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            if !first {
                Rectangle().fill(Color.eveEdge).frame(height: 1).padding(.bottom, 10)
            }
            Text(quoted ? "“\(text)”" : text)
                .font(.body)
                .foregroundStyle(.primary)
                .fixedSize(horizontal: false, vertical: true)
            if let caption, !caption.isEmpty {
                Text(caption).font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .padding(.bottom, 2)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .contextMenu {
            if let hide, !hide.isEmpty {
                Button { Task { await self.hide(hide) } } label: {
                    Label("Don't mention this again", systemImage: "eye.slash")
                }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityActions {
            if let hide, !hide.isEmpty {
                Button("Don't mention this again") { Task { await self.hide(hide) } }
            }
        }
    }

    private func quiet(_ text: String) -> some View {
        Text(text)
            .font(.subheadline)
            .foregroundStyle(.secondary)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 24)
    }

    // MARK: Calls

    private func loadProfile() async {
        defer { loaded = true }
        switch await client.memory(["action": "profile"]) {
        case .success(let r):
            profile = MemoryText.profile(r.profile ?? "")
            if !r.ok { line = r.line; failed = true }
        case .failure(let f):
            line = f.message
            failed = true
        }
        if case .success(let r) = await client.memory(["action": "hidden"]), r.ok { hiddenItems = r.hidden }
    }

    private func hide(_ text: String) async {
        busy = true
        defer { busy = false }
        switch await client.memory(["action": "hide", "text": text]) {
        case .success(let r):
            line = r.line
            failed = !r.ok
            if r.ok {
                done += 1
                await loadProfile()
                if !searched.isEmpty { await refreshSearch() }
            } else {
                fails += 1
            }
        case .failure(let f):
            line = f.message
            failed = true
            fails += 1
        }
    }

    private func unhide(_ text: String) async {
        busy = true
        defer { busy = false }
        switch await client.memory(["action": "unhide", "text": text]) {
        case .success(let r):
            line = r.line
            failed = !r.ok
            if r.ok { done += 1; await loadProfile() } else { fails += 1 }
        case .failure(let f):
            line = f.message
            failed = true
            fails += 1
        }
    }

    /// The same search again, quietly, so a row just hidden leaves the results.
    private func refreshSearch() async {
        if case .success(let r) = await client.memory(["action": "search", "query": searched]), r.ok { reply = r }
    }

    private func search() async {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty else { return }
        typing = false
        busy = true
        defer { busy = false }
        switch await client.memory(["action": "search", "query": q]) {
        case .success(let r):
            searched = q
            reply = r
            line = r.ok ? "" : r.line
            failed = !r.ok
        case .failure(let f):
            line = f.message
            failed = true
            fails += 1
        }
    }

    private func remember() async {
        let text = newMemory.trimmingCharacters(in: .whitespacesAndNewlines)
        newMemory = ""
        guard !text.isEmpty else { return }
        busy = true
        defer { busy = false }
        switch await client.memory(["action": "remember", "text": text]) {
        case .success(let r):
            line = r.line
            failed = !r.ok
            if r.ok { done += 1; await loadProfile() } else { fails += 1 }
        case .failure(let f):
            line = f.message
            failed = true
            fails += 1
        }
    }

    private func previewForget() async {
        busy = true
        defer { busy = false }
        switch await client.memory(["action": "forget", "query": searched]) {
        case .success(let r):
            if r.ok, let total = r.total, total > 0, r.token != nil {
                preview = r
            } else {
                line = r.line.isEmpty ? "Nothing to forget." : r.line
                failed = !r.ok
            }
        case .failure(let f):
            line = f.message
            failed = true
            fails += 1
        }
    }

    private func apply() async {
        guard let token = preview?.token else { return }
        preview = nil
        busy = true
        defer { busy = false }
        switch await client.memory(["action": "apply", "token": token]) {
        case .success(let r):
            line = r.line
            failed = !r.ok
            if r.ok {
                done += 1
                reply = nil
                searched = ""
                query = ""
                await loadProfile()
            } else {
                fails += 1
            }
        case .failure(let f):
            line = f.message
            failed = true
            fails += 1
        }
    }
}

struct PressStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .opacity(configuration.isPressed ? 0.55 : 1)
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}
