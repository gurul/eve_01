// The phone's own turns (docs/action-button.md): the Action Button, the Hold to talk button and evelink://talk start
// a turn on the phone's microphone; eve answers on the phone's speaker. This file is the pure logic, so the Mac test
// harness (Tests/run.sh) can check it without a phone. The widget extension compiles it too (CallPhase).
//
// iOS gives an app no release event for the Action Button: the system runs the assigned intent once, after the hold.
// So an Action Button turn opens the microphone while the finger is still down and ends itself (EndOfSpeech), or on a
// second press. The in-app Hold to talk button is true hold-to-talk: press opens the microphone, release sends.
//
// Notes on the go (owner, 2026-10-04: "when i say start taking notes the phone should hold the port until i click
// action button to turn it off, this allows me to take notes on the go"): once eve says notes are on, the call is a
// recorder, not push to talk, and the next press of any kind stops the notes (PressAction.stopNotes).
import Foundation

/// What a evelink:// URL asks for.
enum LinkRoute: Equatable {
    case pair(URL)
    case talk
    case work       // evelink://work: the home-screen widget's tap opens the Work view (docs/widget.md)
    case chat       // evelink://chat: a tap on a notification opens the Chat (docs/notifications.md)

    init?(_ url: URL) {
        guard url.scheme?.lowercased() == "evelink" else { return nil }
        switch url.host?.lowercased() {
        case "pair": self = .pair(url)
        case "talk": self = .talk
        case "work": self = .work
        case "chat": self = .chat
        default: return nil
        }
    }
}

/// What one press of the Action Button (or the Talk button) does, from where the call is.
enum PressAction: Equatable {
    case notPaired      // no pairing in the Keychain: say how to pair, start nothing
    case start          // nobody is talking: open the call if needed and start a turn on the phone's microphone
    case send           // the phone is talking: end the turn, eve answers
    case bargeIn        // eve is speaking: cut it off and start a new turn
    case stopNotes      // the phone is taking notes: stop them; eve writes them up and texts them

    /// `takingNotes` wins over everything but the pairing: while notes are on the microphone is the notes', so a press
    /// can only mean "stop", even while eve is still saying that notes are on.
    static func decide(paired: Bool, takingNotes: Bool = false, phoneTalking: Bool,
                       eveSpeaking: Bool) -> PressAction {
        guard paired else { return .notPaired }
        if takingNotes { return .stopNotes }
        if phoneTalking { return .send }
        if eveSpeaking { return .bargeIn }
        return .start
    }
}

/// Whether the phone's turn is live on the call.
enum TurnOwner: Equatable {
    case none, phone

    var takesPhoneAudio: Bool { self == .phone }
}

/// The Hold to talk button and the one automatic retry after eve's address changed: the retry restarts a held turn
/// only while a finger is still on the button (verification/Eve/LinkTurn.lean, P3, 2026-10-04).
struct TurnHold: Equatable {
    private(set) var holdDown = false          // the Hold to talk button is under a finger
    private(set) var lastEndsItself = true     // how the last phone turn was started, for the one retry

    /// `phoneBegan` started a turn: it ends itself (a press), or a finger holds it.
    mutating func began(endsItself: Bool) { lastEndsItself = endsItself }
    /// A finger went down on Hold to talk.
    mutating func pressed() { holdDown = true }
    /// The finger took over a live Action Button turn: the finger now ends it, so a retry must not restart it as one
    /// that ends itself after the finger lifted.
    mutating func tookOverLiveTurn() { lastEndsItself = false }
    /// The hold did not start a turn (`holdBegan` threw). The app stops `holding`, so no release will come.
    mutating func failed() { holdDown = false }
    /// The finger lifted.
    mutating func released() { holdDown = false }
    /// A retry that captured `endsItself` when it started: does it start the turn again now?
    func retryRestarts(endsItself: Bool) -> Bool { endsItself || holdDown }
}

/// What has happened on the call since the one automatic address retry started. The retry awaits `refreshAddress()`;
/// meanwhile the owner may start a new turn (the Action Button, Hold to talk) or the call may open again.
/// The retry then must not start the lost turn over the newer one (verification/Eve/LinkTurn.lean, P4, 2026-10-04).
struct TurnGeneration: Equatable {
    private(set) var count = 0

    /// A turn began (`turn = .phone`), or the call opened (`.open`).
    mutating func bump() { count &+= 1 }

    /// The retry that captured `since` when it started: does it start its turn now? Only if nothing began and the call
    /// did not open since, and no turn is live and no call is open now.
    func retryMayStart(since: Int, turn: TurnOwner, callOpen: Bool) -> Bool {
        since == count && turn == .none && !callOpen
    }
}

/// Ends a phone turn by itself: the Action Button has no release, and the daemon's server VAD is off, so the phone
/// must send "done". It ends after `silence` seconds of quiet once speech was heard, after `noSpeech` seconds if no
/// speech came at all, and after `longest` seconds in any case.
struct EndOfSpeech {
    enum Verdict: Equatable { case going, spoke, silent, tooLong }

    var threshold: Float = 0.015        // RMS of float samples in -1...1; room hiss is about 0.002-0.006
    var silence: Double = 1.2
    var noSpeech: Double = 8
    var longest: Double = 60
    private(set) var heard = false
    private var elapsed = 0.0
    private var quiet = 0.0

    init(threshold: Float = 0.015, silence: Double = 1.2, noSpeech: Double = 8, longest: Double = 60) {
        self.threshold = threshold
        self.silence = silence
        self.noSpeech = noSpeech
        self.longest = longest
    }

    /// A turn a finger holds open: only the `longest` cap ends it (the release, or a lost touch, sends it).
    static func held(longest: Double = 60) -> EndOfSpeech {
        EndOfSpeech(silence: .infinity, noSpeech: .infinity, longest: longest)
    }

    /// A turn that ended itself is now held by a finger: keep its clock, drop the pause and no-speech endings.
    mutating func hold() {
        silence = .infinity
        noSpeech = .infinity
    }

    /// Feed one chunk of 16-bit PCM at `rate` samples a second.
    mutating func feed(_ pcm: [Int16], rate: Double) -> Verdict {
        guard !pcm.isEmpty else { return .going }
        let seconds = Double(pcm.count) / rate
        elapsed += seconds
        if Self.rms(pcm) >= threshold {
            heard = true
            quiet = 0
        } else {
            quiet += seconds
        }
        let slack = 1e-9                // 80 chunks of 0.1 s add up to 7.999…, not 8
        if elapsed + slack >= longest { return .tooLong }
        if heard, quiet + slack >= silence { return .spoke }
        if !heard, elapsed + slack >= noSpeech { return .silent }
        return .going
    }

    static func rms(_ pcm: [Int16]) -> Float {
        guard !pcm.isEmpty else { return 0 }
        var sum: Double = 0
        for s in pcm { let f = Double(s) / 32768; sum += f * f }
        return Float((sum / Double(pcm.count)).squareRoot())
    }
}

/// Voice mode's hands-free call (docs/voice-mode.md): the microphone is open, but nothing goes to eve until the owner
/// starts to speak. It holds the last `preroll` seconds, so the first syllable is not cut, and fires once `voiced`
/// seconds in a row are louder than EndOfSpeech's threshold. It judges 10 ms frames, not whole chunks: the microphone
/// hands over 40 to 100 ms at a time, and one loud chunk can be a 5 ms click, a clap or a cup set down.
struct SpeechOnset {
    var threshold: Float = 0.015
    var voiced: Double = 0.15
    var preroll: Double = 0.5
    var rate: Double = 24_000         // PCM16.sampleRate; PCM.swift is not in the widget extension
    var frame: Double = 0.01
    private(set) var held: [Int16] = []
    private var run = 0               // loud frames in a row
    private var partial: [Int16] = [] // the start of a frame the last chunk did not finish

    init(threshold: Float = 0.015, voiced: Double = 0.15, preroll: Double = 0.5,
         rate: Double = 24_000, frame: Double = 0.01) {
        self.threshold = threshold
        self.voiced = voiced
        self.preroll = preroll
        self.rate = rate
        self.frame = frame
    }

    /// Feed one chunk. Nil while nobody speaks; the held audio, oldest first and ending with this chunk, once the owner
    /// has spoken long enough. It holds nothing after that.
    mutating func feed(_ pcm: [Int16]) -> [Int16]? {
        guard !pcm.isEmpty else { return nil }
        held += pcm
        let limit = max(pcm.count, Int(preroll * rate))
        if held.count > limit { held.removeFirst(held.count - limit) }
        let size = max(1, Int(frame * rate))
        let need = max(1, Int((voiced * rate / Double(size) - 1e-9).rounded(.up)))   // 15 frames of 10 ms
        var samples = partial + pcm
        var start = 0
        var spoke = false
        while samples.count - start >= size {
            let f = Array(samples[start..<start + size])
            start += size
            run = EndOfSpeech.rms(f) >= threshold ? run + 1 : 0
            if run >= need { spoke = true }
        }
        samples.removeFirst(start)
        partial = samples
        guard spoke else { return nil }
        return drain()
    }

    /// Everything held, oldest first (a finger took the turn before the owner spoke).
    mutating func drain() -> [Int16] {
        let out = held
        held.removeAll()
        partial.removeAll()
        run = 0
        return out
    }
}

/// When voice mode opens the microphone by itself.
enum HandsFree {
    /// Voice mode just opened: start listening now, or let the call's own turn finish first (eve thinking or
    /// speaking opens the microphone when he is done; a turn already open is the owner's; notes keep the microphone).
    static func startsNow(phase: CallPhase, phoneTalking: Bool, takingNotes: Bool) -> Bool {
        guard !phoneTalking, !takingNotes else { return false }
        return phase == .ended || phase == .waiting
    }

    /// eve said `listening` (his reply is over, or he could not make the turn out): open the microphone again,
    /// while voice mode is open, the call is the phone's and up, and no turn or notes have it.
    static func reopens(handsFree: Bool, replyToPhone: Bool, phoneTalking: Bool, takingNotes: Bool,
                        callOpen: Bool) -> Bool {
        handsFree && replyToPhone && !phoneTalking && !takingNotes && callOpen
    }
}

/// The line under voice mode's orb, from where the call is. Voice mode shows "Muted" itself.
enum VoiceStatus {
    /// Voice mode with nobody talking and eve quiet: talking is the Action Button (2026-10-09).
    static let holdToTalk = "Press the Action Button to talk"

    static func text(phase: CallPhase, connected: Bool, notes: NotesMode, error: String?) -> String {
        if let error, !error.isEmpty { return error }
        switch notes {
        case .on: return "Taking notes"
        case .stopping: return "Writing up your notes…"
        case .off: break
        }
        switch phase {
        case .listening, .waiting: return connected ? "Listening" : "Connecting…"
        case .thinking: return "eve is thinking…"
        case .speaking: return "eve is speaking"
        case .notes: return "Taking notes"
        case .ended: return "Connecting…"
        }
    }
}

/// Where a phone call is, for the Live Activity and the app's Talk section.
enum CallPhase: String, Codable, Hashable, Sendable {
    case listening      // the phone's microphone is open
    case thinking       // the turn was sent; eve has not started to answer
    case speaking       // eve's reply is playing on the phone
    case waiting        // the call is open and quiet: press again to talk
    case notes          // the phone is taking notes: the microphone streams until a press stops it
    case ended          // no phone call

    var title: String {
        switch self {
        case .listening: "Listening…"
        case .thinking: "eve is thinking…"
        case .speaking: "eve is speaking"
        case .waiting: "On a call with eve"
        case .notes: "Taking notes"
        case .ended: "Idle"
        }
    }

    /// What the Action Button (or the Live Activity's button) does next, in words.
    var nextPress: String {
        switch self {
        case .listening: "Send"
        case .speaking: "Interrupt"
        case .notes: "Stop"
        default: "Talk"
        }
    }
}

/// Where notes on the phone are (Relay): off; on, with the microphone streaming to the call; or stopped by a press
/// and waiting for eve to say they are written up.
enum NotesMode: Equatable {
    case off, on, stopping

    var active: Bool { self != .off }
}

/// What the end of a call means while notes are on (Relay, the call's `.ended`). A connection that was lost, not
/// ended by eve, is dialled back whether the notes are recording or a Stop is waiting to go out: a Stop pressed while
/// dialling back used to turn the next failed attempt into "notes over", which threw the held audio away and never sent
/// the stop (review, 2026-10-04). An end eve chose is final.
enum NotesDrop: Equatable {
    case callBack, giveUp

    static func decide(notes: NotesMode, endedByEve: Bool) -> NotesDrop {
        notes.active && !endedByEve ? .callBack : .giveUp
    }
}

/// Whether a notes call is still alive. While notes record, eve sends nothing but a `pong` to each ping (every 10 s),
/// so a call that has heard nothing at all for `deadAfter` is half-open: the phone treats it as a drop, holds its
/// audio and calls back, rather than stream into a socket that goes nowhere (review, 2026-10-04). Only watched while
/// eve says notes are on; an ordinary call is quiet between turns and is never judged by this.
struct NotesLiveness {
    static let deadAfter: TimeInterval = 25
    private(set) var watching = false
    private(set) var lastHeard: Date

    init(now: Date = .now) { lastHeard = now }

    /// Anything arrived from eve.
    mutating func heard(_ now: Date = .now) { lastHeard = now }
    /// eve said notes are on (true) or moved to anything else (false: speaking, writing up, listening).
    mutating func watch(_ on: Bool, now: Date = .now) {
        if on, !watching { lastHeard = now }
        watching = on
    }

    func dead(_ now: Date = .now) -> Bool { watching && now.timeIntervalSince(lastHeard) > Self.deadAfter }
}

/// What the phone holds while a notes call is dropped and dialling back: the microphone's audio, up to `limit`
/// samples (the daemon waits 60 s for the phone, phone_call.NOTES_GRACE_SECS), oldest dropped first.
struct NotesBuffer {
    let limit: Int
    private(set) var pcm: [Int16] = []
    private(set) var dropped = 0

    init(seconds: Double = 60, rate: Int = 24_000) { limit = Int(seconds * Double(rate)) }

    mutating func add(_ chunk: [Int16]) {
        pcm += chunk
        if pcm.count > limit {
            let over = pcm.count - limit
            pcm.removeFirst(over)
            dropped += over
        }
    }

    /// The held audio in frames of at most `size` samples, in order; the buffer is empty after.
    mutating func drain(size: Int) -> [[Int16]] {
        let out = stride(from: 0, to: pcm.count, by: size).map { Array(pcm[$0..<min($0 + size, pcm.count)]) }
        pcm.removeAll()
        dropped = 0
        return out
    }
}

/// Why a phone turn did not start. The message is what the Action Button's intent shows.
enum PhoneTurnError: Error, Equatable, CustomStringConvertible {
    case notPaired
    case micDenied
    case micUnasked
    case noPhoneAudio
    case audio(String)

    var description: String {
        switch self {
        case .notPaired: "Pair Eve Link first: send /stick to eve on Telegram."
        case .micDenied: "Eve Link may not use the microphone. Allow it in Settings > Apps > Eve Link."
        case .micUnasked: "Open Eve Link once and allow the microphone."
        case .noPhoneAudio: "This device has no microphone for eve."
        case .audio(let why): "The microphone did not start (\(why))."
        }
    }
}
