// The composer's mic (docs/app-chat.md): dictation into the text field with the system speech recognizer. It is not a
// call: nothing goes to eve until the owner taps send. A call (Talk, the Action Button) owns the microphone, so
// dictation does not start while one is open.
import AVFoundation
import Speech
import SwiftUI

@MainActor
final class Dictation: ObservableObject {
    @Published private(set) var listening = false
    /// Why dictation could not start or stopped early, for the line above the composer.
    @Published var problem: String?

    private let engine = AVAudioEngine()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private var recognizer: SFSpeechRecognizer?
    private var onText: ((String) -> Void)?
    /// A start is waiting on the permission answers; a stop meanwhile cancels it.
    private var starting = false
    private var startCancelled = false

    /// Start listening. `onText` gets the whole transcription so far each time it changes.
    func start(onText: @escaping (String) -> Void) async {
        guard !listening, !starting else { return }
        starting = true
        startCancelled = false
        defer { starting = false }
        problem = nil
        guard await Self.speechAllowed() else {
            problem = "Allow Speech Recognition for Eve Link in Settings to dictate."
            return
        }
        guard await AVAudioApplication.requestRecordPermission() else {
            problem = "Allow the microphone for Eve Link in Settings to dictate."
            return
        }
        guard !startCancelled else { return }        // stopped (a call, the background, send) while asking
        guard let recognizer = SFSpeechRecognizer() ?? SFSpeechRecognizer(locale: Locale(identifier: "en-US")),
              recognizer.isAvailable else {
            problem = "Dictation isn't available right now."
            return
        }
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .measurement, options: .duckOthers)
            try session.setActive(true, options: .notifyOthersOnDeactivation)
        } catch {
            problem = "The microphone is busy."
            return
        }
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        if recognizer.supportsOnDeviceRecognition { request.requiresOnDeviceRecognition = true }

        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        let feed = DictationFeed(request)
        input.removeTap(onBus: 0)
        input.installTap(onBus: 0, bufferSize: 1024, format: format, block: feed.tap())
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            deactivate()
            problem = "The microphone is busy."
            return
        }
        self.request = request
        self.recognizer = recognizer
        self.onText = onText
        listening = true
        task = recognizer.recognitionTask(with: request, resultHandler: Self.results { [weak self] text, ended in
            guard let self, self.listening else { return }
            if let text { self.onText?(text) }
            if ended { self.stop() }
        })
        diag("dictation: listening")
    }

    /// Stop listening. `releaseSession: false` when a call is taking the microphone: the shared audio session is the
    /// call's now, and deactivating it would cut the call's audio.
    func stop(releaseSession: Bool = true) {
        if starting { startCancelled = true }
        guard listening else { return }
        listening = false
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
        request?.endAudio()
        task?.finish()
        task = nil
        request = nil
        recognizer = nil
        onText = nil
        if releaseSession { deactivate() }
        diag("dictation: stopped")
    }

    private func deactivate() {
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    /// The recognizer calls back on its own queue. Built outside the main actor, so Swift does not assert the main
    /// actor on that queue; only plain values cross back.
    nonisolated private static func results(_ then: @escaping @MainActor @Sendable (String?, Bool) -> Void)
        -> (SFSpeechRecognitionResult?, (any Error)?) -> Void {
        { result, error in
            let text = result?.bestTranscription.formattedString
            let ended = (result?.isFinal ?? false) || error != nil
            Task { @MainActor in then(text, ended) }
        }
    }

    nonisolated private static func speechAllowed() async -> Bool {
        switch SFSpeechRecognizer.authorizationStatus() {
        case .authorized: return true
        case .denied, .restricted: return false
        case .notDetermined:
            return await withCheckedContinuation { c in
                SFSpeechRecognizer.requestAuthorization { c.resume(returning: $0 == .authorized) }
            }
        @unknown default: return false
        }
    }
}

/// The audio tap runs on a real-time thread; appending to the request from there is what Apple's own sample does.
private final class DictationFeed: @unchecked Sendable {
    private let request: SFSpeechAudioBufferRecognitionRequest
    init(_ request: SFSpeechAudioBufferRecognitionRequest) { self.request = request }
    /// The tap block, made outside the main actor so the real-time thread runs it without an isolation check.
    nonisolated func tap() -> AVAudioNodeTapBlock {
        { [self] buffer, _ in request.append(buffer) }
    }
}

/// The field's words while dictating: what was typed before the mic was tapped, then what is being said.
enum DictationText {
    static func join(_ before: String, _ said: String) -> String {
        let head = before.trimmingCharacters(in: .whitespacesAndNewlines)
        if head.isEmpty { return said }
        if said.isEmpty { return head }
        return head + " " + said
    }
}
