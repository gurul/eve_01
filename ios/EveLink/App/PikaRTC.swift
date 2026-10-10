// Pika's video over WebRTC (docs/pika.md, pika_rtc.py; owner, 2026-10-10: "can we setup webrtc so that the camera
// stream is lower latency"). On the home Wi-Fi the phone talks to the Mac directly, past the tunnel; off it, through
// each side's public address (STUN). One data channel, "video", unordered with a short packet lifetime: a lost frame
// is skipped, never waited for. Each binary message is the Mac's stamp then the camera's JPEG
// (Shared/PikaDrive.swift PikaFrameMessage); a text message is a line for the owner. When this can't connect, the
// screen uses the tunnel stream instead (PikaFeed).
@preconcurrency import WebRTC
import Foundation

final class PikaRTC: NSObject, @unchecked Sendable {
    enum Event: Sendable {
        case frame(Data, stamp: Double)
        case line(String)
        case closed
    }

    static let stun = "stun:stun.cloudflare.com:3478"       // pika.DEFAULT_STUN
    private static let factory: RTCPeerConnectionFactory = {
        RTCInitializeSSL()
        return RTCPeerConnectionFactory()
    }()

    private let emit: @Sendable (Event) -> Void
    private let lock = NSLock()
    private var pc: RTCPeerConnection?
    private var channel: RTCDataChannel?
    private var ended = false

    init(emit: @escaping @Sendable (Event) -> Void) {
        self.emit = emit
    }

    /// Offer, send the offer through the Mac's panel, and wait for the channel to open. False: use the tunnel.
    func connect(_ client: WorkClient) async -> Bool {
        let config = RTCConfiguration()
        config.iceServers = [RTCIceServer(urlStrings: [Self.stun])]
        config.sdpSemantics = .unifiedPlan
        config.continualGatheringPolicy = .gatherOnce
        let constraints = RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)
        guard let pc = Self.factory.peerConnection(with: config, constraints: constraints, delegate: self) else {
            return false
        }
        let dc = RTCDataChannelConfiguration()
        dc.isOrdered = false
        dc.maxPacketLifeTime = 150
        guard let channel = pc.dataChannel(forLabel: "video", configuration: dc) else {
            pc.close()
            return false
        }
        channel.delegate = self
        lock.withLock {
            self.pc = pc
            self.channel = channel
        }

        let offer: RTCSessionDescription? = await withCheckedContinuation { (k: CheckedContinuation<Box, Never>) in
            pc.offer(for: constraints) { sdp, _ in k.resume(returning: Box(sdp)) }
        }.value
        guard let offer, await set(pc, local: offer) else { return fail() }
        // every candidate in the offer (the Mac answers without trickle); 2 s is plenty on a phone
        for _ in 0..<40 where pc.iceGatheringState != .complete {
            try? await Task.sleep(for: .milliseconds(50))
        }
        guard !isEnded, let local = pc.localDescription?.sdp, let answer = await client.pikaAnswer(local),
              !isEnded, await set(pc, remote: RTCSessionDescription(type: .answer, sdp: answer)) else {
            return fail()
        }
        for _ in 0..<100 where channel.readyState != .open && !isEnded {
            try? await Task.sleep(for: .milliseconds(50))
        }
        return channel.readyState == .open && !isEnded ? true : fail()
    }

    func close() {
        let (pc, channel) = lock.withLock { () -> (RTCPeerConnection?, RTCDataChannel?) in
            ended = true
            defer { self.pc = nil; self.channel = nil }
            return (self.pc, self.channel)
        }
        channel?.delegate = nil
        channel?.close()
        pc?.close()
    }

    private var isEnded: Bool { lock.withLock { ended } }

    private func fail() -> Bool {
        close()
        return false
    }

    private func set(_ pc: RTCPeerConnection, local: RTCSessionDescription) async -> Bool {
        await withCheckedContinuation { (k: CheckedContinuation<Bool, Never>) in
            pc.setLocalDescription(local) { error in k.resume(returning: error == nil) }
        }
    }

    private func set(_ pc: RTCPeerConnection, remote: RTCSessionDescription) async -> Bool {
        await withCheckedContinuation { (k: CheckedContinuation<Bool, Never>) in
            pc.setRemoteDescription(remote) { error in k.resume(returning: error == nil) }
        }
    }

    /// Lost: once, and only while it was not closed on purpose.
    private func lost() {
        let first = lock.withLock { () -> Bool in
            if ended { return false }
            ended = true
            return true
        }
        if first {
            close()
            emit(.closed)
        }
    }
}

private struct Box: @unchecked Sendable {
    let value: RTCSessionDescription?
    init(_ value: RTCSessionDescription?) { self.value = value }
}

extension PikaRTC: RTCDataChannelDelegate {
    func dataChannelDidChangeState(_ dataChannel: RTCDataChannel) {
        if dataChannel.readyState == .closed { lost() }
    }

    func dataChannel(_ dataChannel: RTCDataChannel, didReceiveMessageWith buffer: RTCDataBuffer) {
        if buffer.isBinary {
            if let f = PikaFrameMessage.parse(buffer.data) { emit(.frame(f.jpeg, stamp: f.stamp)) }
        } else if let line = String(data: buffer.data, encoding: .utf8), !line.isEmpty {
            emit(.line(line))
        }
    }
}

extension PikaRTC: RTCPeerConnectionDelegate {
    func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceConnectionState) {
        if newState == .failed || newState == .closed { lost() }
    }

    func peerConnection(_ peerConnection: RTCPeerConnection, didChange stateChanged: RTCSignalingState) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didAdd stream: RTCMediaStream) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didRemove stream: RTCMediaStream) {}
    func peerConnectionShouldNegotiate(_ peerConnection: RTCPeerConnection) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didChange newState: RTCIceGatheringState) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didGenerate candidate: RTCIceCandidate) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didRemove candidates: [RTCIceCandidate]) {}
    func peerConnection(_ peerConnection: RTCPeerConnection, didOpen dataChannel: RTCDataChannel) {}
}
