// Relay.swift's protocol, as the app declares it: VoiceLevels.swift conforms to it, and Relay is too big to build here.
@MainActor protocol VoiceLevelSink: AnyObject {
    func ownerPCM(_ samples: [Int16]); func evePCM(_ samples: [Int16]); func eveFlushed(); func reset()
    func ownerSaid(_ text: String); func eveSaid(_ text: String)
}
