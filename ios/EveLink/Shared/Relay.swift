// The relay: the phone's turns (docs/action-button.md) become a call to eve, and eve's replies play on the
// phone's speaker. The microphone and speaker are `phone`.
//
// A call is opened on the first press and hung up after a quiet spell, so eve's other chat replies (texts on
// Telegram) are not read out on the phone all day: a call hears everything the chat says while it is open.
import Foundation
#if os(iOS)
import UIKit
#endif

/// The phone's own microphone and speaker (App/PhoneAudio.swift).
@MainActor
protocol PhoneVoice: AnyObject {
    /// Nil: never asked. The intent cannot ask from the background, so it says to open the app.
    var micAllowed: Bool? { get }
    func requestMic() async -> Bool
    /// Starts the audio session: microphone chunks of 24 kHz mono PCM arrive on the main actor, in order.
    func start(_ onPCM: @escaping @MainActor @Sendable ([Int16]) -> Void) throws
    func play(_ pcm: [Int16])
    func flush()
    func stop()
    /// While true, audio lost to an interruption keeps the audio session active, so a locked phone taking notes is not
    /// suspended before the interruption ends and the microphone starts again. `stop()` always lets it go.
    var holdSessionOnLoss: Bool { get set }
    /// The audio stopped by itself (an interruption, or a route change it could not recover from): the phone's audio
    /// is already stopped; the next turn starts it again.
    var onLost: (@MainActor () -> Void)? { get set }
    /// Voice mode's output choice: true plays eve on the earpiece, false (the default) on the speaker. Headphones
    /// take over either way. Only voice mode sets it; every other call stays on the speaker.
    var earpiece: Bool { get set }
    /// The interruption that stopped the audio is over; true when iOS says to resume. Notes on the phone start the
    /// microphone again; an ordinary call waits for the next press.
    var onResumable: (@MainActor (Bool) -> Void)? { get set }
}

/// Voice mode's meter (App/VoiceLevels.swift): the microphone's chunks and eve's played audio, for the orb.
@MainActor
protocol VoiceLevelSink: AnyObject {
    func ownerPCM(_ samples: [Int16])
    func evePCM(_ samples: [Int16])
    func eveFlushed()
    func reset()
    /// The owner's words as eve heard them, and a sentence of eve's reply (shown when its sound plays).
    func ownerSaid(_ text: String)
    func eveSaid(_ text: String)
    /// eve's expression for the face (Shared/FaceExpression.swift), as the call's "expression" message brought it.
    func showExpression(_ request: FaceExpressionRequest)
}

@MainActor
final class Relay: ObservableObject {
    static let hangUpAfter: TimeInterval = 45       // quiet this long after a turn: the call ends
    static let batchSamples = 2400                  // 100 ms of the turn per WebSocket frame

    @Published private(set) var callStatus = "Idle"
    @Published private(set) var lastHeard = ""
    /// Set when a press found Live Activities off: the app says how to turn them on for the Lock Screen.
    @Published private(set) var liveActivitiesOff = false
    func noteLiveActivitiesOff() { liveActivitiesOff = true }
    @Published var pairing: Pairing? { didSet { if let pairing { Keychain.save(pairing) } } }
    /// Where the phone's own call is; `onPhase` hears every change (the Live Activity).
    @Published private(set) var phase: CallPhase = .ended { didSet { if phase != oldValue { onPhase?(phase) } } }
    var onPhase: ((CallPhase) -> Void)?
    /// Called just before the phone's call takes the microphone, in the same main-actor turn: the composer's
    /// dictation stops here, without releasing the audio session the call is about to use.
    var beforeMic: (@MainActor () -> Void)?
    var onHeard: ((String) -> Void)?
    /// The phone's microphone and speaker. Nil: the phone cannot take turns.
    var phone: PhoneVoice? {
        didSet {
            phone?.onLost = { [weak self] in self?.phoneLost() }
            phone?.onResumable = { [weak self] in self?.phoneResumable($0) }
        }
    }
    /// Notes on the phone (docs/action-button.md, Notes on the go): `notesStarted` is when eve began them, for the
    /// Live Activity's timer and the app's banner.
    @Published private(set) var notes = NotesMode.off
    @Published private(set) var notesStarted: Date?
    @Published private(set) var notesWords = 0
    /// Voice mode (docs/voice-mode.md) is open: a hands-free call. After eve answers the microphone opens again by
    /// itself, and a turn starts only when the owner starts to speak (SpeechOnset).
    @Published private(set) var handsFree = false
    /// Voice mode's mute: a muted microphone sends nothing, feeds no end of speech and shows nothing on the orb.
    @Published var muted = false {
        didSet { if muted != oldValue { diag(muted ? "mic: muted" : "mic: unmuted") } }
    }
    /// The call's WebSocket is up (eve said `connected`): voice mode says "Connecting…" until then.
    @Published private(set) var connected = false
    /// Why the last call ended, when eve or the network ended it (not the owner); cleared when a turn begins.
    @Published private(set) var callError: String?
    /// Voice mode's meter. Nil when voice mode is closed.
    var voiceLevels: VoiceLevelSink?

    private let call = CallSocket()
    private var batch: [Int16] = []
    private var turn = TurnOwner.none      // the phone's turn is live, or none
    private var talking: Bool { turn != .none }
    private var phoneTalking: Bool { turn.takesPhoneAudio }
    private var replyToPhone = false       // the phone has the call: eve's voice plays on it
    private var phoneLive = false          // the phone's audio session is running
    private var eveSpeaking = false
    private var endOfSpeech: EndOfSpeech?  // the phone turn's own ending; `held()` while a finger holds it open
    private var hangUpTimer: Timer?
    private var hold = TurnHold()           // the finger on Hold to talk, and how the last phone turn ends
    private var retriedAddress = false      // this press already looked up a new address once
    private var generation = TurnGeneration()   // turns begun and opens, so a waiting retry knows it is stale
    private var notesHeld = NotesBuffer()   // the microphone while a dropped notes call dials back
    private var notesRedial: Task<Void, Never>?
    private var notesRedialing: Bool { notesRedial != nil }
    private var notesRedialOpened = false   // the call back reached eve; `notes` should follow at once
    private var notesKeeper: Timer?         // keeps the Live Activity up while notes record
    private var onset: SpeechOnset?         // hands-free: the turn is open, but `talk` waits for the owner's voice
    /// When eve's audio queued on the phone's speaker ends (an estimate from the samples handed to `play`). eve
    /// sends his reply faster than it plays and says `listening` after the last chunk, so voice mode waits for this
    /// before it opens the microphone again; a reopen never flushes the tail of his reply.
    private var playsUntil = Date.distantPast
    private var reopenAfterPlay: Task<Void, Never>?
    static let playbackMargin: TimeInterval = 0.25  // the output's own latency after the last buffer is handed over
    /// Voice mode is open with the app in the background (VoiceCallScreen): the 45 s quiet hang-up is on again, so a
    /// locked phone never keeps an open microphone for long.
    var handsFreeBackgrounded = false {
        didSet {
            guard handsFreeBackgrounded != oldValue else { return }
            diag("hands-free: \(handsFreeBackgrounded ? "in the background; quiet hang-up on" : "in the foreground")")
            if handsFreeBackgrounded, handsFree { armHangUp() }
        }
    }
    #if os(iOS)
    private var background: UIBackgroundTaskIdentifier = .invalid
    #endif

    init() {
        pairing = Keychain.load()
        if let pairing { Keychain.share(pairing) }      // paired before the widget: let it read the pairing too
        call.onEvent = { [weak self] in self?.fromEve($0) }
    }

    /// Ask the rendezvous (Rendezvous.swift) for eve's current address and take it when it is new. True when the
    /// address changed.
    @discardableResult
    func refreshAddress() async -> Bool {
        guard let pairing else { return false }
        guard let fresh = await Rendezvous.latest(token: pairing.token) else {
            diag("address: rendezvous had nothing")
            return false
        }
        guard fresh != pairing.base, self.pairing?.token == pairing.token else {
            diag("address: current")
            return false
        }
        self.pairing = Pairing(base: fresh, token: pairing.token)
        hangUpCall()
        diag("address: updated to \(fresh.host ?? "?")")
        return true
    }

    func pair(with link: URL) -> Bool {
        guard let p = Pairing(link: link) else { return false }
        pairing = p
        hangUpCall()
        callStatus = "Paired with \(p.base.host ?? "eve")"
        return true
    }

    // ---- eve ----------------------------------------------------------------------------------------------

    private func fromEve(_ event: CallEvent) {
        switch event {
        case .open:
            diag("call: open")
            retriedAddress = false
            connected = true
            generation.bump()
            if notesRedialing { notesRedialOpened = true }
            callStatus = notesRedialing ? "Back on the call; waiting for the notes…" : "On a call with eve"
        case .notes(let started, let words):
            notesOn(started: started, words: words)
        case .state(let state, let note):
            diag("eve: \(state)")
            if notes == .on, state == "thinking" {
                // The notes were stopped from somewhere else (a text, "stop taking notes" said into the phone, the
                // CLI): eve is writing them up, so the microphone stops sending.
                diag("notes: eve is writing them up (\(note ?? "stopped elsewhere"))")
                notes = .stopping
                flushNotesBatch()
                phase = .thinking
            }
            if state == "speaking" {
                reopenAfterPlay?.cancel()           // more of eve: a waiting reopen waits for this reply instead
                reopenAfterPlay = nil
                eveSpeaking = true
                callStatus = notes.active ? callStatus : "eve is speaking"
                // Voice mode's open microphone has not heard the owner yet, so eve's voice is what is going on.
                if replyToPhone, !phoneTalking || onset != nil, !notes.active { phase = .speaking }
            }
            if state == "listening", notes == .stopping || (notes == .on && note != nil) {
                // eve confirmed the notes are over: written up and texted ("Notes saved: <title>").
                diag("notes: done: \(note ?? "")")
                notesOff()
                if let note { callStatus = note }
            }
            if state == "listening" {
                armHangUp()
                let tail = playsUntil.timeIntervalSinceNow
                if reopensHandsFree, tail > 0 {
                    // Voice mode: eve has sent all of his reply, but the phone is still playing it. He is still
                    // speaking for the owner; the microphone opens when the speaker is done.
                    diag(String(format: "hands-free: reply still playing; mic opens in %.1f s", tail))
                    reopenWhenPlayed(in: tail)
                } else {
                    eveDone(note: note)
                    // Voice mode: eve is done, so the microphone opens again by itself.
                    if reopensHandsFree { reopenMic() }
                }
            }
        case .heard(let text):
            lastHeard = text
            onHeard?(text)
            voiceLevels?.ownerSaid(text)
        case .said(let text):
            if replyToPhone { voiceLevels?.eveSaid(text) }
        case .expression(let request):
            if replyToPhone { voiceLevels?.showExpression(request) }
        case .audio(let pcm):
            if replyToPhone {
                phone?.play(pcm)
                voiceLevels?.evePCM(pcm)
                let now = Date.now
                playsUntil = max(now, playsUntil).addingTimeInterval(Double(pcm.count) / Double(PCM16.sampleRate))
            }
            armHangUp()
        case .flush:
            eveSpeaking = false
            flushPlayback()
        case .ended(let reason):
            diag("call: ended: \(reason)")
            if NotesDrop.decide(notes: notes, endedByEve: call.endedByEve) == .callBack {
                // The connection dropped while notes are on, or while a Stop waits to go out: keep the microphone,
                // hold its audio, and call back (each failed attempt lands here too, and keeps the call-back going).
                // eve waits 60 s for the phone before it writes up what it has (phone_call.NOTES_GRACE_SECS).
                redialForNotes(after: reason)
                return
            }
            if notes.active { notesOff() }
            let phoneTurnLost = turn == .phone
            connected = false
            callStatus = reason
            callError = reason
            if reason.contains("address changed"), phoneTurnLost, !retriedAddress {
                // The tunnel moved: find the new address and start the turn again, once.
                retriedAddress = true
                let endsItself = hold.lastEndsItself
                let since = generation.count
                callStatus = "Finding eve's new address…"
                Task { @MainActor [weak self] in
                    guard let self, await self.refreshAddress() else {
                        self?.callStatus = reason
                        return
                    }
                    guard self.hold.retryRestarts(endsItself: endsItself) else { return }
                    // A turn began or the call opened while the address was looked up: that turn is the owner's
                    // now, so the lost one is not started over it (verification/Eve/LinkTurn.lean, P4).
                    guard self.generation.retryMayStart(since: since, turn: self.turn,
                                                        callOpen: self.call.isOpen) else {
                        diag("retry: a newer turn began; not restarting")
                        return
                    }
                    do {
                        try self.phoneBegan(endsItself: endsItself)
                    } catch {
                        diag("retry: \(error.localizedDescription)")
                    }
                }
            }
            turn = .none
            endOfSpeech = nil
            onset = nil
            stopPhone()
            letSleep()
        case .failed(let why):
            diag("call: failed: \(why)")
            callStatus = why
        }
    }

    private func armHangUp() {
        hangUpTimer?.invalidate()
        hangUpTimer = Timer.scheduledTimer(withTimeInterval: Self.hangUpAfter, repeats: false) { [weak self] _ in
            Task { @MainActor in
                guard let self, !self.notes.active else { return }
                if self.handsFree {
                    // Voice mode never hangs up in the foreground. In the background it does after 45 quiet seconds,
                    // unless the owner is mid-sentence (a turn that has gone out); an open mic still waiting for
                    // speech counts as quiet.
                    guard self.handsFreeBackgrounded, !self.talking || self.onset != nil else { return }
                    diag("hands-free: quiet in the background; the call ends")
                    self.endHandsFree()
                    return
                }
                guard !self.talking else { return }
                self.hangUpCall()
                self.callStatus = "Idle"
                self.stopPhone()
                self.letSleep()
            }
        }
    }

    /// Voice mode would open the microphone again now that eve is done (HandsFree.reopens).
    private var reopensHandsFree: Bool {
        HandsFree.reopens(handsFree: handsFree, replyToPhone: replyToPhone, phoneTalking: phoneTalking,
                          takingNotes: notes.active, callOpen: call.isOpen)
    }

    /// eve is done speaking: the call is quiet, or the owner's open turn is listening.
    private func eveDone(note: String?) {
        eveSpeaking = false
        if note == nil || phoneTalking { callStatus = phoneTalking ? "Listening…" : "On a call with eve" }
        if replyToPhone, !phoneTalking { phase = .waiting }
        if onset != nil { phase = .listening }
    }

    private func reopenMic() {
        do {
            try phoneBegan(endsItself: true)
        } catch {
            diag("hands-free: could not reopen the mic: \(error.localizedDescription)")
        }
    }

    /// Voice mode: open the microphone once eve's queued reply has played. Anything that starts a turn, a new
    /// `speaking` or the end of the call cancels the wait.
    private func reopenWhenPlayed(in seconds: TimeInterval) {
        reopenAfterPlay?.cancel()
        reopenAfterPlay = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .seconds(seconds + Self.playbackMargin))
            guard let self, !Task.isCancelled else { return }
            self.reopenAfterPlay = nil
            let more = self.playsUntil.timeIntervalSinceNow
            if more > 0, self.reopensHandsFree {        // more audio came after `listening`: wait for it too
                self.reopenWhenPlayed(in: more)
                return
            }
            self.eveDone(note: nil)
            if self.reopensHandsFree { self.reopenMic() }
        }
    }

    /// Drop eve's audio queued on the phone (a barge-in, or eve's own flush).
    private func flushPlayback() {
        phone?.flush()
        voiceLevels?.eveFlushed()
        playsUntil = .distantPast
    }

    // ---- the phone's own turns (docs/action-button.md) -------------------------------------------------------

    /// True when a press now would start a phone turn (not send one, not refuse): the Live Activity must come first.
    var pressWouldStart: Bool {
        let action = PressAction.decide(paired: pairing != nil, takingNotes: notes.active, phoneTalking: phoneTalking,
                                        eveSpeaking: eveSpeaking && replyToPhone)
        return action == .start || action == .bargeIn
    }

    /// One press of the Action Button, the Live Activity's button or evelink://talk: start a turn that ends itself
    /// after a pause, send the turn in progress, or cut eve off and start a new turn.
    @discardableResult
    func press() throws -> PressAction {
        let action = PressAction.decide(paired: pairing != nil, takingNotes: notes.active, phoneTalking: phoneTalking,
                                        eveSpeaking: eveSpeaking && replyToPhone)
        diag("press → \(action)")
        switch action {
        case .notPaired: throw PhoneTurnError.notPaired
        case .start, .bargeIn: try phoneBegan(endsItself: true)
        case .send: phoneEnded()
        case .stopNotes: stopNotes(by: "a press")
        }
        return action
    }

    /// The Hold to talk button went down: true hold-to-talk, the turn lasts until `holdEnded`.
    func holdBegan() throws {
        diag("hold: pressed")
        hold.pressed()
        do {
            guard pairing != nil else { throw PhoneTurnError.notPaired }
            if notes.active {                               // a hold while notes are on stops them; its release does nothing
                stopNotes(by: "the Hold to talk button")
                return
            }
            if phoneTalking {                               // an Action Button turn is now held: the finger ends it
                talkNow()                                   // voice mode's open microphone: the hold is the turn
                if endOfSpeech == nil { endOfSpeech = .held() } else { endOfSpeech?.hold() }
                hold.tookOverLiveTurn()                     // so an address retry never restarts it after the lift
                return
            }
            try phoneBegan(endsItself: false)
        } catch {
            // The app stops `holding` (EveLinkApp.holdBegan), so no release comes: no finger holds anything now.
            hold.failed()
            throw error
        }
    }

    /// The Hold to talk button came up: send the turn.
    func holdEnded() {
        diag("hold: released")
        hold.released()
        phoneEnded()
    }

    /// Hang up the phone's call now (the app's End call button).
    func endPhoneCall() {
        if notes.active {
            // End while notes are on: eve stops them and writes them up (the daemon's `end`), and texts them.
            diag("notes: ended with the call; eve writes them up")
            notesOff()
            callStatus = "Call ended. eve will text the notes."
        }
        turn = .none
        endOfSpeech = nil
        onset = nil
        batch.removeAll()
        hangUpCall()
        callStatus = "Idle"
        stopPhone()
        letSleep()
    }

    // ---- voice mode: a hands-free call (docs/voice-mode.md) ----------------------------------------------------

    /// Voice mode opened: start listening on the call path the Action Button uses, unless a turn is already open,
    /// eve is answering (the microphone opens when he is done), or notes are recording.
    func beginHandsFree() throws {
        handsFree = true
        muted = false
        callError = nil
        let start = HandsFree.startsNow(phase: phase, phoneTalking: phoneTalking, takingNotes: notes.active)
        diag("hands-free: on\(start ? "" : " (waiting for the call's turn)")")
        guard start else { return }
        do {
            try phoneBegan(endsItself: true)
        } catch {
            handsFree = false
            throw error
        }
    }

    /// Voice mode closed (the X, or Ask eve): the call ends, as End call does.
    func endHandsFree() {
        guard handsFree else { return }
        diag("hands-free: off")
        handsFree = false
        handsFreeBackgrounded = false
        muted = false
        endPhoneCall()
    }

    /// Voice mode's output: the earpiece or the speaker.
    func useEarpiece(_ on: Bool) {
        phone?.earpiece = on
    }

    /// Hands-free: the owner started speaking (or a finger took the turn). Now eve hears `talk`, then what the
    /// microphone held from just before.
    private func talkNow() {
        guard var waiting = onset else { return }
        onset = nil
        call.talk()
        batch += waiting.drain()
        diag("hands-free: speech; turn sent to eve")
    }

    private func hangUpCall() {
        call.hangUp()
        connected = false
    }

    private func phoneBegan(endsItself: Bool) throws {
        guard let pairing else { throw PhoneTurnError.notPaired }
        guard let phone else { throw PhoneTurnError.noPhoneAudio }
        hold.began(endsItself: endsItself)
        diag("turn: mic permission \(phone.micAllowed.map { $0 ? "granted" : "denied" } ?? "not asked")")
        switch phone.micAllowed {
        case .some(true): break
        case .some(false): throw PhoneTurnError.micDenied
        case .none: throw PhoneTurnError.micUnasked
        }
        // The Live Activity first: an AudioRecordingIntent's recording stops without one.
        let before = phase
        phase = .listening
        if !phoneLive {
            beforeMic?()
            do {
                try phone.start { [weak self] pcm in self?.fromPhoneMic(pcm) }
            } catch {
                phase = before
                diag("mic: start failed: \(error.localizedDescription)")
                throw PhoneTurnError.audio(error.localizedDescription)
            }
            diag("mic: on")
            phoneLive = true
        }
        keepAwake()
        hangUpTimer?.invalidate()
        let bargingIn = eveSpeaking && replyToPhone
        reopenAfterPlay?.cancel()
        reopenAfterPlay = nil
        // Only a barge-in or a press outside voice mode cuts eve's queued audio: voice mode's own reopen waits for it
        // to play out (reopenWhenPlayed), and must never drop its tail.
        if bargingIn || !handsFree { flushPlayback() }
        eveSpeaking = false
        callError = nil
        turn = .phone
        generation.bump()
        replyToPhone = true
        batch.removeAll()
        endOfSpeech = endsItself ? EndOfSpeech() : .held()
        if !call.isOpen {
            diag("call: opening \(pairing.base.host ?? "?")")
            call.open(base: pairing.base, token: pairing.token)
        }
        callStatus = "Listening…"
        // Voice mode waits for the owner's voice before it says `talk`, so silence never becomes a turn. A barge-in
        // says it at once: `talk` is what stops eve.
        if handsFree, endsItself, !bargingIn {
            onset = SpeechOnset()
            if handsFreeBackgrounded { armHangUp() }    // the open mic waits at most 45 s in the background
        } else {
            onset = nil
            call.talk()
        }
    }

    private func fromPhoneMic(_ input: [Int16]) {
        if muted {
            // Voice mode's mute: nothing is sent, heard or shown. A turn that has already gone out still keeps its
            // clock, as silence, so it ends (and eve answers the half-sentence) rather than stay open on eve's side.
            if phoneTalking, onset == nil, endOfSpeech != nil {
                endOfSpeechHeard([Int16](repeating: 0, count: input.count))
            }
            return
        }
        // The orb shows the owner only while the microphone is listened to: not while eve thinks or speaks.
        if phoneTalking || notes == .on { voiceLevels?.ownerPCM(input) }
        var pcm = input
        if notes == .on {
            // Notes: every chunk goes to the call, with no end of speech; held while a dropped call dials back.
            if notesRedialing { notesHeld.add(pcm); return }
            batch += pcm
            if batch.count >= Self.batchSamples {
                call.audio(batch)
                batch.removeAll(keepingCapacity: true)
            }
            return
        }
        guard phoneTalking else { return }
        if onset != nil {
            // Hands-free and nobody has spoken yet: hold a moment of audio, send nothing.
            guard let held = onset?.feed(pcm) else { return }
            onset = nil
            call.talk()
            diag("hands-free: speech; turn sent to eve")
            pcm = held
        }
        batch += pcm
        if batch.count >= Self.batchSamples {
            call.audio(batch)
            batch.removeAll(keepingCapacity: true)
        }
        endOfSpeechHeard(pcm)
    }

    /// Feed the turn's end of speech, and end the turn when it says so.
    private func endOfSpeechHeard(_ pcm: [Int16]) {
        guard var ear = endOfSpeech else { return }
        let verdict = ear.feed(pcm, rate: Double(PCM16.sampleRate))
        endOfSpeech = ear
        switch verdict {
        case .going: break
        case .spoke, .tooLong: phoneEnded()
        case .silent where handsFree:
            // Voice mode stays on the call: a barge-in that never turned into words goes to eve as it is.
            phoneEnded()
        case .silent:
            // Nothing was said: hang up rather than send seconds of room noise as a question.
            endPhoneCall()
            callStatus = "Nothing heard."
        }
    }

    private func phoneEnded() {
        guard phoneTalking else { return }
        if onset != nil {
            // Voice mode's open microphone has heard no words yet: there is nothing to send, so it keeps listening.
            diag("hands-free: nothing said yet; still listening")
            return
        }
        turn = .none
        endOfSpeech = nil
        if !batch.isEmpty { call.audio(batch) }
        batch.removeAll()
        call.done()
        callStatus = "eve is thinking…"
        phase = .thinking
        armHangUp()
    }

    /// The phone's audio engine stopped by itself mid-call (PhoneAudio.onLost): send what was heard, and let the next
    /// turn start the engine again rather than wait on a dead microphone.
    private func phoneLost() {
        guard phoneLive else { return }
        phoneLive = false
        if notes == .on {
            diag("notes: the phone's audio was interrupted; paused")
            flushNotesBatch()
            callStatus = "Notes paused: the phone's audio was interrupted. They carry on when it is free."
            return
        }
        if onset != nil {
            // Voice mode's open microphone had heard no words: eve was never told of a turn, so drop it rather
            // than wait on a dead microphone. A tap on the orb starts the audio again; so does the interruption's end.
            diag("hands-free: the phone's audio stopped before anyone spoke; the waiting turn is dropped")
            turn = .none
            onset = nil
            endOfSpeech = nil
            batch.removeAll()
            phase = .waiting
            callError = "The phone's audio stopped. Tap to talk."
        } else if phoneTalking {
            phoneEnded()
        }
        callStatus = "The phone's audio stopped. Press again to talk."
    }

    /// The interruption is over: notes on the phone carry on (even without iOS's "should resume", since the owner
    /// asked for them to run until a press), and voice mode opens its microphone again if the call is quiet. An
    /// ordinary call waits for the next press.
    private func phoneResumable(_ shouldResume: Bool) {
        guard !phoneLive else { return }
        if notes == .on {
            diag("notes: interruption over (should resume: \(shouldResume)); microphone on again")
            startNotesMic()
            return
        }
        guard handsFree, !phoneTalking, !notes.active, phase == .waiting, call.isOpen else { return }
        diag("hands-free: interruption over (should resume: \(shouldResume)); microphone on again")
        reopenMic()
    }

    // ---- notes on the phone (owner, 2026-10-04) ------------------------------------------------------------
    // "when i say start taking notes the phone should hold the port until i click action button to turn it off,
    // this allows me to take notes on the go". eve decides when notes are on (take_notes, phone_call.PhoneNotes) and
    // says so with a `notes` state; from then on the call is a recorder: the microphone streams to eve with no end
    // of speech and no hang-up timer, the Live Activity shows "Taking notes" with a timer and a Stop button (it must
    // stay up: an AudioRecordingIntent's recording stops without one), and any press stops the notes. A dropped call
    // is dialled back while the microphone's audio is held, so a lift or a dead spot loses nothing.

    /// eve says notes are on: when they start, after its spoken reply, and when the phone calls back.
    private func notesOn(started: Date?, words: Int) {
        notesStarted = started ?? notesStarted ?? .now
        notesWords = words
        if notesRedialing {
            // Back on the call after a drop: the held audio first, then (if Stop was pressed meanwhile) the stop.
            notesRedial?.cancel()
            notesRedial = nil
            notesRedialOpened = false
            let frames = notesHeld.drain(size: Self.batchSamples)
            diag("notes: back on the call; sending \(frames.count) held frames")
            frames.forEach(call.audio)
            if notes == .stopping { call.notesStop() }
        }
        if notes == .stopping {
            diag("notes: eve still says on (\(words) words); waiting for the write-up")
            return
        }
        let first = notes == .off
        notes = .on
        diag(first ? "notes: on (\(words) words so far)" : "notes: still on, \(words) words")
        if first {
            // A turn the owner was still in is over: its audio is the notes' from here.
            if phoneTalking { turn = .none }
            endOfSpeech = nil
            batch.removeAll()
        }
        hangUpTimer?.invalidate()
        replyToPhone = true
        keepAwake()
        phone?.holdSessionOnLoss = true     // an interruption must not let a locked phone suspend mid-notes
        if !phoneLive { startNotesMic() }
        callStatus = "Taking notes. Press the Action Button to stop."
        phase = .notes
        // The Live Activity must be up the whole time the microphone records: bring it back if it was swiped away,
        // now and every minute while the notes run. From the background iOS refuses the request, so the minute timer
        // is only a best effort; a press while notes are on brings the panel up first, from the foreground if it
        // must (PhoneLink.press).
        onPhase?(.notes)
        if notesKeeper == nil {
            notesKeeper = Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in
                Task { @MainActor in
                    guard let self, self.notes == .on else { return }
                    self.onPhase?(.notes)
                }
            }
        }
    }

    /// A press (or a hold) stops the notes: send what the microphone has, then `notes_stop`. The notes
    /// are over for the app once eve confirms (a `listening` state with "Notes saved").
    private func stopNotes(by who: String) {
        guard notes.active else { return }
        diag("notes: stop pressed (\(who))")
        if notes == .stopping {
            // A second press: say it again (eve ignores a repeat), but never while dialling back, where it would
            // overtake the held audio; notesOn sends the stop after that audio once the call is back.
            if !notesRedialing { call.notesStop() }
            return
        }
        notes = .stopping
        flushNotesBatch()
        if notesRedialing {
            // The call is down: the stop goes out after the held audio once the call is back (notesOn). If it never
            // comes back, eve writes up what it had after its wait anyway.
            diag("notes: stop while dialling back; it is sent when the call is up")
        } else {
            call.notesStop()
        }
        callStatus = "Writing up your notes…"
        phase = .thinking
        armHangUp()
    }

    private func flushNotesBatch() {
        if !batch.isEmpty, call.isOpen, !notesRedialing { call.audio(batch) }
        batch.removeAll()
    }

    /// Notes are over: back to an ordinary call, quiet and waiting for a press.
    private func notesOff() {
        diag("notes: off")
        phone?.holdSessionOnLoss = false
        notesKeeper?.invalidate()
        notesKeeper = nil
        notes = .off
        notesStarted = nil
        notesWords = 0
        notesRedial?.cancel()
        notesRedial = nil
        notesRedialOpened = false
        _ = notesHeld.drain(size: Self.batchSamples)
        batch.removeAll()
        if replyToPhone { phase = call.isOpen ? .waiting : .ended }
        armHangUp()
    }

    private func startNotesMic() {
        guard let phone else { return }
        do {
            try phone.start { [weak self] pcm in self?.fromPhoneMic(pcm) }
            phoneLive = true
            diag("notes: microphone on")
        } catch {
            diag("notes: microphone did not start: \(error.localizedDescription)")
            callStatus = "Notes: the microphone did not start (\(error.localizedDescription))."
        }
    }

    /// The call dropped while notes are on: call back with a backoff, holding the microphone's audio meanwhile.
    /// eve waits 60 s (phone_call.NOTES_GRACE_SECS); after that it writes up what it had and a new call finds no
    /// notes, so the app gives the notes up and says they will come by text.
    private func redialForNotes(after reason: String) {
        guard notesRedial == nil else { return }
        guard let pairing else {
            diag("notes: call dropped and the pairing is gone; notes over")
            notesOff()
            stopPhone()
            return
        }
        diag("notes: call dropped (\(reason)); calling back")
        callStatus = "Connection lost. Calling eve back; the notes are held."
        batch.removeAll()   // already sent, or lost with the connection
        keepAwake()
        let token = pairing.token
        notesRedial = Task { @MainActor [weak self] in
            var wait: Double = 1
            let deadline = Date.now.addingTimeInterval(55)
            while !Task.isCancelled, Date.now < deadline {
                try? await Task.sleep(for: .seconds(wait))
                guard let self, !Task.isCancelled, self.notes.active else { return }
                if !self.call.isOpen {
                    if reason.contains("address") { await self.refreshAddress() }
                    guard let base = self.pairing?.base, self.pairing?.token == token else { break }   // re-paired
                    diag("notes: calling back \(base.host ?? "?")")
                    self.call.open(base: base, token: token)
                }
                // eve re-attaches the notes straight after `connected`: give it a few seconds to say so. A call that
                // is up with no notes means eve already wrote them up.
                try? await Task.sleep(for: .seconds(5))
                if Task.isCancelled || !self.notesRedialing { return }
                if self.notesRedialOpened {
                    diag("notes: back on the call, but eve has no notes running")
                    break
                }
                wait = min(wait * 2, 8)
            }
            guard let self, !Task.isCancelled, self.notesRedialing else { return }
            diag("notes: could not get the notes back; eve writes up what it had")
            self.notesRedial = nil
            self.notesRedialOpened = false
            self.notesOff()
            self.callStatus = "Lost the call while taking notes. eve will text what it heard."
            self.hangUpCall()
            self.stopPhone()
            self.letSleep()
        }
    }

    private func stopPhone() {
        if notes.active { notesOff() }
        onset = nil
        reopenAfterPlay?.cancel()
        reopenAfterPlay = nil
        playsUntil = .distantPast
        voiceLevels?.reset()
        replyToPhone = false
        eveSpeaking = false
        if phoneLive {
            phone?.stop()
            phoneLive = false
        }
        phase = .ended
    }

    // ---- staying awake with the screen locked -------------------------------------------------------------
    // The audio background mode keeps the app running while the microphone or the reply plays, and a background task
    // covers the gaps between turns, so the WebSocket keeps being served.

    private func keepAwake() {
        #if os(iOS)
        guard background == .invalid else { return }
        background = UIApplication.shared.beginBackgroundTask(withName: "eve call") { [weak self] in
            Task { @MainActor in self?.letSleep() }
        }
        #endif
    }

    private func letSleep() {
        #if os(iOS)
        if background != .invalid {
            UIApplication.shared.endBackgroundTask(background)
            background = .invalid
        }
        #endif
    }
}
