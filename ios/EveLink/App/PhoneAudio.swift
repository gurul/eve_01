// The phone's own microphone and speaker for a call (docs/action-button.md). The wire is the call's: 24 kHz mono
// 16-bit PCM (PCM16.sampleRate).
//
// One AVAudioEngine runs for the whole call: the input tap is converted to 24 kHz Int16, and eve's reply plays on a
// player node. Voice processing (echo cancellation) keeps eve's voice out of the microphone, as the Mini App's
// getUserMedia does with echoCancellation.
@preconcurrency import AVFoundation

@MainActor
final class PhoneAudio: PhoneVoice {
    private var engine = AVAudioEngine()
    private var player = AVAudioPlayerNode()
    private var configObserver: NSObjectProtocol?
    private let playFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: Double(PCM16.sampleRate),
                                           channels: 1, interleaved: false)!
    private var running = false
    var holdSessionOnLoss = false
    private var sessionHeld = false        // stopped by an interruption with the session kept (holdSessionOnLoss)
    private var wired = false
    private var onPCM: (@MainActor @Sendable ([Int16]) -> Void)?
    private var observers: [NSObjectProtocol] = []
    var onLost: (@MainActor () -> Void)?
    var onResumable: (@MainActor (Bool) -> Void)?
    /// Voice mode's output (Relay.useEarpiece). Changed mid-call, the session's category changes; iOS then reports a
    /// configuration change and `reconfigure` restarts the engine on the new route, as for AirPods.
    var earpiece = false {
        didSet {
            guard earpiece != oldValue, running else { return }
            do {
                try setCategory()
                diag("audio: output \(earpiece ? "earpiece" : "speaker")")
            } catch {
                diag("audio: output change failed: \(error.localizedDescription)")
            }
        }
    }

    init() {
        let center = NotificationCenter.default
        observeConfiguration()
        // A phone call, an alarm or Siri takes the audio session: the engine stops.
        observers.append(center.addObserver(forName: AVAudioSession.interruptionNotification,
                                            object: AVAudioSession.sharedInstance(), queue: .main) { [weak self] note in
            let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
            switch raw.flatMap(AVAudioSession.InterruptionType.init(rawValue:)) {
            case .began:
                diag("audio: interrupted")
                MainActor.assumeIsolated { self?.lose() }
            case .ended:
                // The interruption is over (the phone call hung up, Siri finished). Notes on the phone start the
                // microphone again by themselves; an ordinary call waits for the next press, as before.
                let options = (note.userInfo?[AVAudioSessionInterruptionOptionKey] as? UInt)
                    .map(AVAudioSession.InterruptionOptions.init(rawValue:)) ?? []
                MainActor.assumeIsolated { self?.onResumable?(options.contains(.shouldResume)) }
            default:
                break
            }
        })
    }

    /// A route change (AirPods, a Bluetooth headset) stops the engine and can change the input format.
    private func observeConfiguration() {
        if let configObserver { NotificationCenter.default.removeObserver(configObserver) }
        configObserver = NotificationCenter.default.addObserver(forName: .AVAudioEngineConfigurationChange,
                                                                object: engine, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.reconfigure() }
        }
    }

    var micAllowed: Bool? {
        switch AVAudioApplication.shared.recordPermission {
        case .granted: true
        case .denied: false
        default: nil
        }
    }

    func requestMic() async -> Bool {
        await AVAudioApplication.requestRecordPermission()
    }

    func start(_ onPCM: @escaping @MainActor @Sendable ([Int16]) -> Void) throws {
        guard !running else { return }
        let session = AVAudioSession.sharedInstance()
        try setCategory()
        try session.setActive(true)
        sessionHeld = false
        do {
            try wireAndStart(onPCM)
        } catch {
            // Seen on the iPhone 2026-10-12: after the call's audio stopped in the background, every later start
            // failed with avfaudio -50 until the app was relaunched. A fresh engine starts; so build one and retry.
            diag("mic: start failed (\(error.localizedDescription)); rebuilding the audio engine")
            rebuildEngine()
            try wireAndStart(onPCM)
        }
        self.onPCM = onPCM
        running = true
    }

    private func wireAndStart(_ onPCM: @escaping @MainActor @Sendable ([Int16]) -> Void) throws {
        if !wired {
            try engine.inputNode.setVoiceProcessingEnabled(true)
            engine.attach(player)
            engine.connect(player, to: engine.mainMixerNode, format: playFormat)
            wired = true
        }
        try startEngine(onPCM)
    }

    private func rebuildEngine() {
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        engine = AVAudioEngine()
        player = AVAudioPlayerNode()
        wired = false
        observeConfiguration()
    }

    /// Play and record for voice chat. The speaker unless voice mode chose the earpiece; Bluetooth headsets either way.
    private func setCategory() throws {
        let options: AVAudioSession.CategoryOptions = earpiece ? [.allowBluetoothHFP]
                                                               : [.defaultToSpeaker, .allowBluetoothHFP]
        try AVAudioSession.sharedInstance().setCategory(.playAndRecord, mode: .voiceChat, options: options)
    }

    /// Tap the input in its current format and start the engine.
    private func startEngine(_ onPCM: @escaping @MainActor @Sendable ([Int16]) -> Void) throws {
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else { throw PhoneTurnError.noPhoneAudio }
        guard let box = Downsampler(from: format) else { throw PhoneTurnError.audio("no converter") }
        input.installTap(onBus: 0, bufferSize: 2048, format: format, block: Self.tap(box, onPCM))
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            throw error
        }
        player.play()
    }

    /// The engine's configuration changed (it has stopped): tap the new input format and start again. If it will not
    /// start, give the call up so the next turn starts the audio from the beginning.
    private func reconfigure() {
        guard running, let onPCM else { return }
        engine.inputNode.removeTap(onBus: 0)
        player.stop()
        engine.stop()
        do {
            try startEngine(onPCM)
        } catch {
            diag("audio: restart after a route change failed: \(error.localizedDescription)")
            lose()
        }
    }

    /// The audio stopped under the call: stop cleanly and tell Relay.
    private func lose() {
        guard running else { return }
        // Notes on the phone keep the session (review, 2026-10-04): deactivating it lets a locked app suspend, and
        // then nothing restarts the microphone when the interruption ends. Not verified on a device yet.
        stop(deactivate: !holdSessionOnLoss)
        onLost?()
    }

    func play(_ pcm: [Int16]) {
        guard running, !pcm.isEmpty,
              let buffer = AVAudioPCMBuffer(pcmFormat: playFormat, frameCapacity: AVAudioFrameCount(pcm.count)),
              let out = buffer.floatChannelData?[0] else { return }
        buffer.frameLength = AVAudioFrameCount(pcm.count)
        for (i, s) in pcm.enumerated() { out[i] = Float(s) / 32768 }
        player.scheduleBuffer(buffer, completionHandler: nil)
        if !player.isPlaying { player.play() }
    }

    /// eve was interrupted: drop what is queued to play.
    func flush() {
        guard running else { return }
        player.stop()
        player.play()
    }

    func stop() { stop(deactivate: true) }

    private func stop(deactivate: Bool) {
        guard running else {
            if deactivate, sessionHeld {
                sessionHeld = false
                try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            }
            return
        }
        running = false
        sessionHeld = !deactivate
        onPCM = nil
        engine.inputNode.removeTap(onBus: 0)
        player.stop()
        engine.stop()
        if deactivate {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        }
    }

    /// The tap runs on the audio thread: build it outside the main actor, and hand each chunk to the main queue in
    /// order (DispatchQueue.main is FIFO; separate Tasks need not be).
    private nonisolated static func tap(_ box: Downsampler,
                                        _ onPCM: @escaping @MainActor @Sendable ([Int16]) -> Void) -> AVAudioNodeTapBlock {
        { buffer, _ in
            guard let pcm = box.convert(buffer), !pcm.isEmpty else { return }
            DispatchQueue.main.async { MainActor.assumeIsolated { onPCM(pcm) } }
        }
    }
}

/// The hardware rate and channels to 24 kHz mono Int16. Used only on the tap's thread.
private final class Downsampler: @unchecked Sendable {
    private let converter: AVAudioConverter
    private let out: AVAudioFormat

    init?(from format: AVAudioFormat) {
        guard let out = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: Double(PCM16.sampleRate),
                                      channels: 1, interleaved: true),
              let converter = AVAudioConverter(from: format, to: out) else { return nil }
        converter.downmix = true
        self.converter = converter
        self.out = out
    }

    func convert(_ buffer: AVAudioPCMBuffer) -> [Int16]? {
        let ratio = out.sampleRate / buffer.format.sampleRate
        let capacity = AVAudioFrameCount(Double(buffer.frameLength) * ratio + 64)
        guard let dst = AVAudioPCMBuffer(pcmFormat: out, frameCapacity: capacity) else { return nil }
        var fed = false
        var error: NSError?
        let status = converter.convert(to: dst, error: &error) { _, inputStatus in
            if fed {
                inputStatus.pointee = .noDataNow
                return nil
            }
            fed = true
            inputStatus.pointee = .haveData
            return buffer
        }
        guard status != .error, dst.frameLength > 0, let samples = dst.int16ChannelData?[0] else { return nil }
        return Array(UnsafeBufferPointer(start: samples, count: Int(dst.frameLength)))
    }
}
