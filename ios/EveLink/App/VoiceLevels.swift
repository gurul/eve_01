// How loud the call is right now, and who is speaking, for the voice-mode orb (docs/voice-mode.md).
//
// The host hands over the same PCM the call already moves: the microphone's chunks (Relay.fromPhoneMic) and eve's
// reply (Relay.fromEve .audio, just before PhoneAudio.play). eve's audio arrives faster than it plays, so its
// levels are queued on a playback clock in 50 ms windows: the orb pulses with the word being heard, not with the
// network. The pure math (RMS, the dB scale, the spring) is static and testable on the Mac.
import Foundation
import Observation

/// Who the face is showing.
enum VoiceSpeaker: Sendable, Hashable {
    case idle, owner, eve
}

/// The words under the face: what eve heard the owner say, or the sentence of eve's that is playing.
struct VoiceCaption: Sendable, Hashable {
    var who: VoiceSpeaker
    var text: String
}

@MainActor
@Observable
final class VoiceLevels {
    /// The noise floor and the top of the perceptual scale, in dBFS. Below the floor is 0; at the ceiling is 1.
    nonisolated static let floorDB = -50.0
    nonisolated static let ceilingDB = -12.0
    /// Below this level a chunk is silence: it does not keep a speaker alive.
    nonisolated static let voicedLevel = 0.06
    /// Silence this long and the speaker falls back to idle.
    nonisolated static let holdSeconds = 0.3
    /// eve's audio is split into windows this long, each with its own level.
    nonisolated static let windowSeconds = 0.05

    /// The current loudness, 0...1 on the dB scale. Raw: the orb smooths it with a spring.
    private(set) var level: Double = 0
    private(set) var speaker: VoiceSpeaker = .idle
    /// The line under the face: "Connecting…", "Listening", "eve is speaking", "Muted", or an error.
    var status: String = "Connecting…"
    /// The caption under the face, or nil when there is nothing to show.
    private(set) var caption: VoiceCaption?
    /// A caption stays this long after it was shown (eve's: after his voice stops).
    nonisolated static let captionHoldSeconds = 4.0

    @ObservationIgnored private var pendingCaptions: [(at: Double, text: String)] = []   // eve's, on the playback clock
    @ObservationIgnored private var captionUntil = -Double.infinity

    @ObservationIgnored private var ownerLevel = 0.0
    @ObservationIgnored private var ownerHeard = -Double.infinity     // last voiced owner chunk
    @ObservationIgnored private var ownerLast = -Double.infinity      // last owner chunk of any kind
    @ObservationIgnored private var eveQueue: [(at: Double, level: Double)] = []
    @ObservationIgnored private var eveClock = 0.0                  // when eve's queued audio ends
    @ObservationIgnored private var ticker: Task<Void, Never>?

    init() {}

    // MARK: Entry points (any thread)

    /// A chunk from the phone's microphone (24 kHz mono). Call it after the mute check, so a muted mic shows idle.
    nonisolated func ownerPCM(_ samples: [Int16]) {
        guard !samples.isEmpty else { return }
        let value = Self.level(fromRMS: Self.rms(samples))
        Self.onMain { self.heardOwner(value, now: Self.clock()) }
    }

    /// A chunk of eve's reply (24 kHz mono), as it is handed to the speaker.
    nonisolated func evePCM(_ samples: [Int16]) {
        guard !samples.isEmpty else { return }
        let values = Self.windowLevels(samples, rate: Double(PCM16.sampleRate))
        Self.onMain { self.heardEve(values, now: Self.clock()) }
    }

    /// What eve heard the owner say: shown at once, until eve's own words replace it.
    nonisolated func ownerSaid(_ text: String) {
        let words = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !words.isEmpty else { return }
        Self.onMain {
            let now = Self.clock()
            self.caption = VoiceCaption(who: .owner, text: words)
            self.captionUntil = now + Self.captionHoldSeconds * 2
            self.tick(now: now)
        }
    }

    /// A sentence of eve's, sent just before its sound: shown when that sound starts to play (the playback clock).
    nonisolated func eveSaid(_ text: String) {
        let words = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !words.isEmpty else { return }
        Self.onMain {
            let now = Self.clock()
            self.pendingCaptions.append((max(now, self.eveClock), words))
            self.tick(now: now)
        }
    }

    /// eve was interrupted (the call's flush): drop the levels still queued to play.
    func eveFlushed() {
        pendingCaptions.removeAll()
        eveQueue.removeAll()
        eveClock = 0
        tick(now: Self.clock())
    }

    /// The call ended: everything back to idle.
    func reset() {
        ticker?.cancel()
        ticker = nil
        eveQueue.removeAll()
        eveClock = 0
        ownerLevel = 0
        ownerHeard = -.infinity
        ownerLast = -.infinity
        level = 0
        speaker = .idle
        pendingCaptions.removeAll()
        caption = nil
        captionUntil = -.infinity
    }

    // MARK: Pure math

    /// Root mean square of 16-bit samples, 0...1 of full scale.
    nonisolated static func rms(_ samples: some Collection<Int16>) -> Double {
        guard !samples.isEmpty else { return 0 }
        var sum = 0.0
        for s in samples {
            let x = Double(s) / 32768
            sum += x * x
        }
        return (sum / Double(samples.count)).squareRoot()
    }

    /// RMS (0...1 of full scale) to a 0...1 level, linear in decibels between the noise floor and the ceiling.
    nonisolated static func level(fromRMS rms: Double) -> Double {
        guard rms.isFinite, rms > 0 else { return 0 }
        let db = 20 * log10(rms)
        return min(1, max(0, (db - floorDB) / (ceilingDB - floorDB)))
    }

    /// One level per `windowSeconds` of audio (the last window may be shorter).
    nonisolated static func windowLevels(_ samples: [Int16], rate: Double) -> [Double] {
        let size = max(1, Int(rate * windowSeconds))
        return stride(from: 0, to: samples.count, by: size).map {
            level(fromRMS: rms(samples[$0..<min($0 + size, samples.count)]))
        }
    }

    /// Who the orb shows: the owner while voiced in the last `holdSeconds` (barge-in wins), else eve while his
    /// audio is still playing, else idle.
    nonisolated static func speaker(ownerVoicedAgo: Double, evePlaying: Bool) -> VoiceSpeaker {
        if ownerVoicedAgo < holdSeconds { return .owner }
        return evePlaying ? .eve : .idle
    }

    // MARK: State

    private func heardOwner(_ value: Double, now: Double) {
        ownerLevel = value
        ownerLast = now
        if value >= Self.voicedLevel { ownerHeard = now }
        tick(now: now)
    }

    private func heardEve(_ values: [Double], now: Double) {
        var at = max(now, eveClock)
        for value in values {
            eveQueue.append((at, value))
            at += Self.windowSeconds
        }
        eveClock = at
        tick(now: now)
    }

    /// Advance to `now`, and keep ticking while anything is still live.
    private func tick(now: Double) {
        if advance(now: now) { startTicking() }
    }

    /// Publish the level and the speaker at `now`. True while anything is still live.
    private func advance(now: Double) -> Bool {
        while let first = eveQueue.first, first.at + Self.windowSeconds <= now { eveQueue.removeFirst() }
        let eveNow = eveQueue.first.flatMap { $0.at <= now ? $0.level : nil }
        let next = Self.speaker(ownerVoicedAgo: now - ownerHeard, evePlaying: eveNow != nil)
        let value: Double = switch next {
        case .owner: ownerLevel
        case .eve: eveNow ?? 0
        // Quiet room noise still shows faintly until the mic goes still for the hold time.
        case .idle: now - ownerLast < Self.holdSeconds ? ownerLevel : 0
        }
        if speaker != next { speaker = next }
        if level != value { level = value }
        // eve's next sentence once its sound plays; any caption fades a while after its voice stops
        while let first = pendingCaptions.first, first.at <= now {
            pendingCaptions.removeFirst()
            caption = VoiceCaption(who: .eve, text: first.text)
        }
        if caption?.who == .eve, next == .eve { captionUntil = now + Self.captionHoldSeconds }
        if caption != nil, now > captionUntil, pendingCaptions.isEmpty { caption = nil }
        return !eveQueue.isEmpty || now - ownerLast < Self.holdSeconds || value > 0
            || !pendingCaptions.isEmpty || caption != nil
    }

    /// About 30 times a second while live, so silence decays to idle and eve's queue plays out without new PCM.
    private func startTicking() {
        guard ticker == nil else { return }
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(33))
                guard let self, !Task.isCancelled else { return }
                if !self.advance(now: Self.clock()) {
                    self.ticker = nil
                    return
                }
            }
        }
    }

    nonisolated private static func clock() -> Double { ProcessInfo.processInfo.systemUptime }

    /// Run on the main actor, in call order: inline on the main thread, else queued FIFO on the main queue.
    nonisolated private static func onMain(_ body: @escaping @MainActor @Sendable () -> Void) {
        if Thread.isMainThread {
            MainActor.assumeIsolated(body)
        } else {
            DispatchQueue.main.async { MainActor.assumeIsolated(body) }
        }
    }
}

/// Relay feeds the meter through this (Shared/Relay.swift); every method is already here.
extension VoiceLevels: VoiceLevelSink {}

/// A critically damped spring: it reaches the target as fast as it can without overshooting. `omega` is its speed
/// (about 4 / settle time). The step is the exact solution, so it is stable at any frame time.
struct CriticalSpring: Sendable, Equatable {
    var value: Double = 0
    var velocity: Double = 0

    mutating func step(to target: Double, dt: Double, omega: Double) {
        guard dt > 0 else { return }
        let x = value - target
        let decay = exp(-omega * dt)
        let k = velocity + omega * x
        value = target + (x + k * dt) * decay
        velocity = (velocity - omega * k * dt) * decay
    }
}
