// The sheet for an app the phone has no Swift for (docs/adding-an-app.md, "The phone"): the app's own panel on the
// Mac describes it (app_registry.panel, link_chat.py) and this draws it. A status headline, then its buttons,
// toggles and sliders, then the muted line. Every press goes to POST /api/link/controls/<id>, the same door as the
// native sheets (ControlSheets.swift); each answer redraws the sheet (AppPanelModel, ControlsClient.swift).
import SwiftUI
import UIKit

struct GenericAppSheet: View {
    let entry: AppEntry
    let client: WorkClient
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            GenericAppControls(entry: entry, client: client)
                .background(Color.eveBackground)
                .navigationTitle(entry.title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
                }
        }
        .presentationDragIndicator(.visible)
    }
}

/// An SF Symbol name this iOS has, or the fallback (a name from a newer Mac draws nothing otherwise).
enum AppSymbol {
    static func named(_ name: String?, fallback: String = AppEntry.fallbackSymbol) -> String {
        guard let name, !name.isEmpty, UIImage(systemName: name) != nil else { return fallback }
        return name
    }
}

private struct GenericAppControls: View {
    let entry: AppEntry
    let client: WorkClient
    @State private var model = AppPanelModel()
    /// A slider's value while the finger is on it; sent when the drag ends.
    @State private var drafts: [String: Double] = [:]
    @State private var taps = 0
    @State private var done = 0
    @State private var fails = 0

    var body: some View {
        ScrollView {
            VStack(spacing: 24) {
                header
                if !model.controls.isEmpty { controls }
                SaidLine(text: model.line, error: model.failed)
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 20)
        }
        .task { await refresh() }
        .refreshable { await refresh() }
        .sensoryFeedback(.impact(weight: .light), trigger: taps)
        .sensoryFeedback(.success, trigger: done)
        .sensoryFeedback(.error, trigger: fails)
    }

    @ViewBuilder private var header: some View {
        if !model.answered {
            ProgressView()
                .frame(maxWidth: .infinity, minHeight: 52)
                .accessibilityLabel("Asking \(entry.title)")
        } else if let status = model.status {
            HStack(spacing: 14) {
                Image(systemName: AppSymbol.named(entry.symbol))
                    .font(.system(size: 20, weight: .medium))
                    .frame(width: 44, height: 44)
                    .background(RoundedRectangle(cornerRadius: 11, style: .continuous).fill(Color.eveFill))
                    .accessibilityHidden(true)
                Text(status)
                    .font(.title3.weight(.semibold))
                    .lineLimit(3)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if model.busy { ProgressView().accessibilityHidden(true) }
            }
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
        }
    }

    private var controls: some View {
        VStack(spacing: 12) {
            ForEach(model.controls) { control in
                switch control.kind {
                case .button(let symbol, let destructive):
                    button(control, symbol: symbol, destructive: destructive)
                case .toggle(let on):
                    toggle(control, on: on)
                case .slider(let value, let lo, let hi, let step):
                    slider(control, value: value, range: lo...hi, step: step)
                }
            }
        }
        .disabled(model.busy || !model.answered)
    }

    private func button(_ control: AppControl, symbol: String?, destructive: Bool) -> some View {
        Button(role: destructive ? .destructive : nil) { press(control) } label: {
            Group {
                if let symbol {
                    Label(control.label, systemImage: AppSymbol.named(symbol, fallback: "circle"))
                } else {
                    Text(control.label)
                }
            }
            .font(.body.weight(.medium))
            .foregroundStyle(destructive ? Color.red : Color.primary)
            .frame(maxWidth: .infinity, minHeight: 52)
            .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Color.eveFill))
            .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Color.eveEdge))
        }
        .buttonStyle(PressStyle())
        .accessibilityLabel(control.label)
    }

    private func toggle(_ control: AppControl, on: Bool) -> some View {
        Toggle(control.label, isOn: Binding(get: { on }, set: { press(control, on: $0) }))
            .font(.body)
            .padding(.horizontal, 16)
            .frame(minHeight: 52)
            .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Color.eveFill))
            .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Color.eveEdge))
    }

    private func slider(_ control: AppControl, value: Double, range: ClosedRange<Double>, step: Double?) -> some View {
        let shown = drafts[control.id] ?? value
        let binding = Binding(get: { drafts[control.id] ?? value }, set: { drafts[control.id] = $0 })
        let editing: (Bool) -> Void = { active in
            guard !active, let v = drafts[control.id] else { return }
            press(control, value: v)
            drafts[control.id] = nil
        }
        return VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(control.label).font(.body)
                Spacer()
                Text(Self.format(shown, step: step)).font(.body.monospacedDigit()).foregroundStyle(.secondary)
            }
            .accessibilityHidden(true)
            Group {
                if let step {
                    Slider(value: binding, in: range, step: step, onEditingChanged: editing)
                } else {
                    Slider(value: binding, in: range, onEditingChanged: editing)
                }
            }
            .accessibilityLabel(control.label)
            .accessibilityValue(Self.format(shown, step: step))
            // VoiceOver's swipe up or down is one press, sent at once (a drag sends when the finger lifts).
            .accessibilityAdjustableAction { direction in
                let by = step ?? (range.upperBound - range.lowerBound) / 10
                switch direction {
                case .increment: press(control, value: value + by)
                case .decrement: press(control, value: value - by)
                @unknown default: break
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Color.eveFill))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Color.eveEdge))
    }

    /// "3", or "0.25" when the step is not whole.
    private static func format(_ v: Double, step: Double?) -> String {
        if let step, step == step.rounded() { return String(Int(v.rounded())) }
        if v == v.rounded() { return String(Int(v)) }
        return v.formatted(.number.precision(.fractionLength(0...2)))
    }

    private func refresh() async {
        guard let body = model.refresh() else { return }
        await send(body)
    }

    private func press(_ control: AppControl, on: Bool? = nil, value: Double? = nil) {
        guard let body = model.press(control, on: on, value: value) else { return }
        taps += 1
        Task { await send(body) }
    }

    private func send(_ body: [String: Any]) async {
        let result = await client.app(entry.id, body)
        model.finish(result)
        if model.failed { fails += 1 } else if body["action"] as? String != "status" { done += 1 }
    }
}
