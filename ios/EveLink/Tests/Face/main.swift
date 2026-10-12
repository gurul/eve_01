// The voice-mode face's motion on the Mac (App/StackChanFace.swift FaceMotion): an expression's heights are the eye,
// whatever the mood's openness, as the board draws them (expression.h, eyes.cpp applyState).
import Foundation
var fails = 0
func check(_ ok: Bool, _ what: String) { print(ok ? "ok  " : "FAIL", what); if !ok { fails += 1 } }

/// The frame after `secs` of steady 60 fps, Reduce Motion on so no blink lands on the sample.
@MainActor func settle(_ mood: FaceMood, _ label: FaceExpressionLabel?, level: Double = 0, secs: Double = 3) -> FaceFrame {
    let motion = FaceMotion(random: { 0.5 })
    let shown = label.map { FaceExpressionShown(id: 1, label: $0) }
    var frame = motion.step(now: 0, mood: mood, level: level, expression: shown, reduceMotion: true)
    for i in 1...Int(secs * 60) {
        frame = motion.step(now: Double(i) / 60, mood: mood, level: level, expression: shown, reduceMotion: true)
    }
    return frame
}

@MainActor func run() {
    let near = { (a: Double, b: Double) in abs(a - b) < 0.01 }
    let thinking = settle(.thinking, nil)
    check(near(thinking.left, 0.78) && near(thinking.right, 0.78), "thinking alone squints (0.78): \(thinking.left)")
    let surprised = settle(.thinking, .surprised)
    check(near(surprised.left, 110.0 / 96) && near(surprised.right, 110.0 / 96),
          "surprised while thinking settles at 110/96, taller than the rest eye: \(surprised.left)")
    let sad = settle(.thinking, .sad)
    check(near(sad.left, 72.0 / 96), "sad while thinking settles at 72/96: \(sad.left)")
    let quiet = settle(.speaking, .frustrated, level: 0)
    check(near(quiet.left, 70.0 / 96), "frustrated in a speaking trough keeps its 70/96: \(quiet.left)")
    let loud = settle(.speaking, .frustrated, level: 1)
    check(near(loud.left, 1.1 * 70.0 / 96), "and bounces a little with eve's voice: \(loud.left)")
    let rest = settle(.idle, .calm)
    check(near(rest.left, 1) && near(rest.right, 1), "calm at rest is the rest eye")
    newExpressions()
    print(fails == 0 ? "FACE_OK" : "FACE_FAILED \(fails)")
}
// The 2026-10-12 round (docs/expressions-design.json): eight labels, their shapes, motion and marks.
let boardEleven: [FaceExpressionLabel] = [.calm, .happy, .curious, .affection, .surprised, .sad, .worried, .skeptical,
                                          .frustrated, .excited, .wink]
let newEight: [FaceExpressionLabel] = [.laughing, .sleepy, .confused, .thinking, .sheepish, .relieved, .proud, .flattered]

/// Two poses the same to within `tol` (float rounding in the loop's remainder).
func same(_ a: FaceExpressionPose, _ b: FaceExpressionPose, tol: Double = 1e-6) -> Bool {
    func n(_ x: Double, _ y: Double) -> Bool { abs(x - y) <= tol }
    guard n(a.left, b.left), n(a.right, b.right), n(a.gaze?.x ?? 0, b.gaze?.x ?? 0), n(a.gaze?.y ?? 0, b.gaze?.y ?? 0),
          (a.gaze == nil) == (b.gaze == nil), n(a.jiggle.x, b.jiggle.x), n(a.jiggle.y, b.jiggle.y),
          n(a.closed, b.closed), n(a.shut, b.shut), a.blinks == b.blinks, a.overlays.count == b.overlays.count
    else { return false }
    return zip(a.overlays, b.overlays).allSatisfy { o, p in
        o.kind == p.kind && o.anchor == p.anchor && n(o.dx, p.dx) && n(o.dy, p.dy) && n(o.size, p.size)
            && n(o.scale, p.scale) && n(o.angle, p.angle)
    }
}

/// Every 10 ms over the longest time to live.
let samples = (0...600).map { Double($0) / 100 }

@MainActor func newExpressions() {
    let near = { (a: Double, b: Double, tol: Double) in abs(a - b) <= tol }
    check(FaceExpressionLabel.allCases == boardEleven + newEight, "nineteen labels: the board's eleven, then the eight")

    // Every label has a style the face can draw.
    for label in FaceExpressionLabel.allCases {
        let s = label.style
        check(s.left > 0 && s.right > 0 && s.width > 0 && s.radius > 0 && s.blink >= 1
              && FaceExpressionRequest.ttlRange.contains(s.ttlMs / 1000) && s.blinkShut > 0,
              "\(label.rawValue) has a drawable style")
    }

    // The new shapes are the design's numbers: heights, width, corner, lid (and its depth), blink pace, time to live.
    struct Spec { var l, r, w, rad: Int; var lid: FaceLid; var depth: Double; var blink: Int; var ttl: Double }
    let design: [FaceExpressionLabel: Spec] = [
        .laughing: Spec(l: 94, r: 94, w: 100, rad: 22, lid: .smile, depth: 1.4, blink: 3, ttl: 3000),
        .sleepy: Spec(l: 40, r: 40, w: 96, rad: 18, lid: .flat, depth: 1, blink: 6, ttl: 5000),
        .confused: Spec(l: 100, r: 88, w: 92, rad: 22, lid: .none, depth: 1, blink: 3, ttl: 3500),
        .thinking: Spec(l: 84, r: 84, w: 88, rad: 24, lid: .flat, depth: 1, blink: 3, ttl: 3500),
        .sheepish: Spec(l: 78, r: 78, w: 92, rad: 22, lid: .flat, depth: 1, blink: 2, ttl: 4000),
        .relieved: Spec(l: 86, r: 86, w: 98, rad: 26, lid: .droopSmile, depth: 1, blink: 4, ttl: 3500),
        .proud: Spec(l: 80, r: 80, w: 100, rad: 24, lid: .smile, depth: 0.5, blink: 3, ttl: 4000),
        .flattered: Spec(l: 66, r: 66, w: 100, rad: 28, lid: .smile, depth: 0.8, blink: 4, ttl: 4000),
    ]
    for label in newEight {
        let s = label.style, d = design[label]!
        check(s.left == d.l && s.right == d.r && s.width == d.w && s.radius == d.rad && s.lid == d.lid
              && s.depth == d.depth && s.blink == d.blink && s.ttlMs == d.ttl && !s.curious,
              "\(label.rawValue): the design's shape \([s.left, s.right, s.width, s.radius, s.blink]) \(s.lid) \(s.depth)")
    }
    check(near(94 * (1 - FaceExpressionLabel.laughing.style.smile / 2), 28, 0.5),
          "laughing's crescent: 70% cover leaves the design's 28 px of a 94 px eye")
    check(FaceExpressionLabel.sleepy.style.blinkShut == 0.4 && newEight.filter { $0 != .sleepy }
              .allSatisfy { $0.style.blinkShut == 0.11 }, "only sleepy blinks slowly (400 ms)")
    check(FaceExpressionLabel.relieved.style.droop == 0.5 && FaceExpressionLabel.relieved.style.smile == 0.7,
          "relieved: the droop at half of sad's over a smile at 70% of happy's (so it never reads as sad)")
    check(FaceExpressionLabel.laughing.style.easeIn == 40
          && FaceExpressionLabel.allCases.filter { $0 != .laughing }.allSatisfy { $0.style.easeIn == 12 },
          "only laughing snaps into its shape")
    check(FaceLid.flat.rawValue == 4 && FaceLid.droopSmile.rawValue == 5, "the two new moods follow the board's four")

    // The board's eleven are unchanged: still poses, no gaze or marks, the old lid depths and the 4 s default.
    for label in boardEleven {
        let s = label.style
        let still = samples.allSatisfy { label.pose(at: $0) == FaceExpressionPose(left: Double(s.left), right: Double(s.right)) }
        check(still && s.depth == 1 && s.blinkShut == 0.11 && s.ttlMs == 4000 && label.overlayKinds.isEmpty
              && s.smile == (s.lid == .smile ? 1 : 0) && s.droop == (s.lid == .droop ? 1 : 0) && s.flat == 0,
              "\(label.rawValue) unchanged: a still pose, no marks, 4 s")
    }

    for label in FaceExpressionLabel.allCases {
        let name = label.rawValue
        // Deterministic: the same time gives the same pose.
        check(samples.allSatisfy { label.pose(at: $0) == label.pose(at: $0) }
              && samples.allSatisfy { label.pose(at: $0, reduceMotion: true) == label.pose(at: $0, reduceMotion: true) },
              "\(name): the pose is a function of the time")
        // Bounded: heights 20...112 px, gaze within 16 x 22, quick moves within 4 px, marks at most 1.15 and 10 degrees.
        let bounded = samples.allSatisfy { t in
            let p = label.pose(at: t)
            return (20...112).contains(p.left) && (20...112).contains(p.right)
                && abs(p.gaze?.x ?? 0) <= 16 && abs(p.gaze?.y ?? 0) <= 22
                && abs(p.jiggle.x) <= 4 && abs(p.jiggle.y) <= 4 && (0...1).contains(p.closed) && (0...1).contains(p.shut)
                && p.overlays.allSatisfy { (0...1.15).contains($0.scale) && abs($0.angle) <= 10 && abs($0.dx) <= 28
                    && abs($0.dy) <= 22 && $0.size <= 24 }
        }
        check(bounded, "\(name): bounded")
        // Continuous: no frame-to-frame jump bigger than a 5 Hz bounce makes in 10 ms.
        let smooth = zip(samples, samples.dropFirst()).allSatisfy { a, b in
            let p = label.pose(at: a), q = label.pose(at: b)
            return abs(p.left - q.left) < 3.5 && abs(p.right - q.right) < 3.5 && abs((p.gaze?.x ?? 0) - (q.gaze?.x ?? 0)) < 2
                && abs((p.gaze?.y ?? 0) - (q.gaze?.y ?? 0)) < 2 && abs(p.jiggle.y - q.jiggle.y) < 1.6
                && abs(p.jiggle.x - q.jiggle.x) < 1.6 && abs(p.closed - q.closed) < 0.2 && abs(p.shut - q.shut) < 0.2
        }
        check(smooth, "\(name): moves without jumps")
        // Settles: still after `settle`, or looping every `period`.
        let after = samples.filter { $0 >= label.settle }
        let settles = after.allSatisfy { t in
            if let period = label.period { return same(label.pose(at: t), label.pose(at: t + period)) }
            return same(label.pose(at: t), label.pose(at: label.settle + 10))
        }
        check(settles, "\(name): settles by \(label.settle) s" + (label.period.map { ", then loops every \($0) s" } ?? ""))
        // Reduce Motion: the end pose held, nothing moving, every mark shown still.
        let rest = label.pose(at: 0, reduceMotion: true)
        check(samples.allSatisfy { label.pose(at: $0, reduceMotion: true) == rest } && rest.jiggle == (0, 0)
              && rest.closed == 0 && rest.shut == 0 && rest.overlays.allSatisfy { $0.scale == 1 }
              && Set(rest.overlays.map(\.kind)) == label.overlayKinds,
              "\(name): Reduce Motion holds one pose, marks still")
        if label.period == nil, label.pose(at: label.settle + 10).gaze != nil {
            let end = label.pose(at: label.settle + 10)
            check(rest.gaze?.x == end.gaze?.x && rest.gaze?.y == end.gaze?.y && rest.left == end.left
                  && rest.right == end.right, "\(name): Reduce Motion's pose is the end pose")
        }
        // Marks only for their labels.
        let seen = Set(samples.flatMap { t in label.pose(at: t).overlays.filter { $0.scale > 0 }.map(\.kind) })
        check(seen == label.overlayKinds, "\(name): marks \(seen.map(\.rawValue).sorted())")
    }
    check(Set(newEight.flatMap(\.overlayKinds)) == Set(FaceOverlay.Kind.allCases)
          && [FaceExpressionLabel.laughing, .thinking, .relieved].allSatisfy { $0.overlayKinds.isEmpty },
          "z, ?, sweat, sparkle and blush each belong to one label; laughing, thinking and relieved have none")

    // Each label's motion, against the design's words.
    let laugh = { (t: Double) in FaceExpressionLabel.laughing.pose(at: t) }
    let early = stride(from: 0.0, to: 1.2, by: 0.005).map { abs(laugh($0).jiggle.y) }.max() ?? 0
    let late = stride(from: 1.2, to: 3, by: 0.005).map { abs(laugh($0).jiggle.y) }.max() ?? 0
    check(near(early, 4, 0.01) && near(late, 2, 0.01) && near(laugh(0.05).jiggle.y, -4, 1e-9)
          && near(laugh(1.2 + 0.125).jiggle.y, -2, 1e-9), "laughing bounces 4 px at 5 Hz, then 2 px at 2 Hz")
    check(samples.allSatisfy { !laugh($0).blinks && laugh($0).gaze == nil }, "laughing never blinks and keeps the gaze")

    let sleepy = FaceExpressionLabel.sleepy
    check(sleepy.pose(at: 1).left == 40 && near(sleepy.pose(at: 2.38).left, 24, 1e-9)
          && near(sleepy.pose(at: 2.38 + 2.5).right, 24, 1e-9) && sleepy.pose(at: 2.6).left == 40,
          "sleepy: 40 px slits that sag to 24 every 2.5 s and pop back")
    check(sleepy.pose(at: 0).gaze?.y == 0 && sleepy.pose(at: 4.5).gaze?.y == 6, "sleepy's gaze sinks 6 px")
    let zs = sleepy.pose(at: 1.0).overlays
    check(zs.map(\.size) == [14, 20] && zs[0].dy < zs[1].dy && zs[1].dx > zs[0].dx && zs.allSatisfy { $0.anchor == .aboveRightEye }
          && sleepy.pose(at: 0.35).overlays[1].scale == 0 && sleepy.pose(at: 0.6).overlays[1].scale > 0,
          "two z's (14 and 20 px) rise and drift right over the right eye, 400 ms apart, the second further right")
    let together = stride(from: 0.0, to: 2.5, by: 0.01).filter { t in sleepy.pose(at: t).overlays.allSatisfy { $0.scale > 0.9 } }
    check(Double(together.count) * 0.01 >= 0.9, "both z's show at full size together for \(together.count * 10) ms of each nod")

    let confused = FaceExpressionLabel.confused
    let c = { (t: Double) in confused.pose(at: t) }
    check(c(0.3).left == 104 && c(0.3).right == 80 && c(0.8).left == 80 && c(0.8).right == 104
          && c(1.3).left == 104 && c(1.8).right == 104 && c(2.5).left == 100 && c(2.5).right == 88,
          "confused: the tilt swaps twice, 500 ms a side, then rests at 100/88")
    check(c(0.3).gaze?.x == -8 && c(0.8).gaze?.x == 8 && c(2.5).gaze?.x == 0, "its gaze sweeps 8 px with the taller eye")
    check(c(0.1).overlays[0].scale == 0 && c(0.5).overlays[0].scale == 1 && c(0.3).overlays[0].angle == -10
          && c(0.8).overlays[0].angle == 10 && c(0.5).overlays[0].anchor == .aboveCentre && c(0.5).overlays[0].size == 22,
          "a 22 px ? over the pair pops in at 200 ms and tips toward the taller eye")

    let think = { (t: Double) in FaceExpressionLabel.thinking.pose(at: t) }
    check(think(1.0).gaze! == (-16, -22) && think(2.5).gaze! == (16, -22) && near(think(1.75).gaze!.x, 0, 1e-9),
          "thinking looks up (22 px) and left 1.6 s, then one 300 ms saccade up-right")
    check(!think(1.0).blinks && !think(2.5).blinks && think(1.7).blinks, "thinking never blinks during its holds")

    let sheep = { (t: Double) in FaceExpressionLabel.sheepish.pose(at: t) }
    check(sheep(1.5).gaze! == (14, 10) && sheep(2.2).gaze! == (0, 0) && sheep(3).gaze! == (14, 10)
          && FaceExpressionLabel.thinking.pose(at: 3).gaze!.y < -20 && sheep(3).gaze!.y >= 10,
          "sheepish looks aside and down, glances back at 2 s for 300 ms, and away again")
    let shuffle = stride(from: 0.0, to: 1, by: 0.005).map { abs(sheep($0).jiggle.x) }.max() ?? 0
    check(near(shuffle, 2, 0.01) && sheep(1.2).jiggle.x == 0, "two 2 px shuffles in the first second")
    check(sheep(0.15).overlays[0].dy == 0 && sheep(1.15).overlays[0].dy == 6
          && sheep(1).overlays[0].anchor == .outerTop(right: true) && sheep(1).overlays[0].size == 15,
          "a 15 px sweat drop slides 6 px over a second")

    let relief = { (t: Double) in FaceExpressionLabel.relieved.pose(at: t) }
    check(relief(0.3).closed == 1 && relief(0.8).closed == 0 && relief(0.35).gaze!.y == 4 && relief(0.7).gaze!.y == 0
          && relief(2.5).gaze!.y == 6, "relieved: shut and reopen in 700 ms rising 4 px, then sink 6 px")
    check(!relief(0.3).blinks && relief(1).blinks, "no blink during the exhale")

    let proud = { (t: Double) in FaceExpressionLabel.proud.pose(at: t) }
    check(near(proud(0.15).jiggle.y, -4, 1e-9) && proud(0.3).jiggle.y == 0 && proud(1).gaze! == (0, -8),
          "proud: one 4 px chin-up nudge, the gaze straight up 8 px")
    check(proud(0.6).overlays.allSatisfy { $0.scale == 1 && $0.kind == .sparkle && $0.size == 12 }
          && proud(0.6).overlays.map(\.anchor) == [.outerTop(right: false), .outerTop(right: true)]
          && proud(1.3).overlays.allSatisfy { $0.scale == 1 } && proud(1.7).overlays.allSatisfy { $0.scale == 0 }
          && proud(2.6).overlays.allSatisfy { $0.scale == 1 } && proud(3.0).overlays.allSatisfy { $0.scale == 1 },
          "two 12 px sparkles at the outer corners, held over a second, twinkling again every 1.75 s")
    check(proud(1.65).shut == 1 && proud(1.4).shut == 0 && proud(1.9).shut == 0 && !proud(1).blinks && proud(2).blinks,
          "one deliberate 300 ms blink at 1.5 s, then the usual pace")

    let flat = { (t: Double) in FaceExpressionLabel.flattered.pose(at: t) }
    check(flat(1).gaze! == (0, 8) && near(flat(1.25).jiggle.x, 3, 1e-9) && near(flat(1.75).jiggle.x, -3, 1e-9)
          && flat(0.5).overlays.allSatisfy { $0.scale == 1 && $0.size == 20 }
          && flat(0.5).overlays.map(\.anchor) == [.under(right: false), .under(right: true)]
          && flat(0.5).overlays[0].dx < 0 && flat(0.5).overlays[1].dx > 0,
          "flattered looks down 8 px, sways 3 px at 1 Hz, 20 px blush on each outer cheek")

    // The face: the pose reaches the frame.
    let sleepyFrame = settle(.idle, .sleepy)
    check(near(sleepyFrame.left, 40.0 / 96, 0.01) && near(sleepyFrame.flat, 1, 0.01)
          && Set(sleepyFrame.overlays.map(\.kind)) == [.z] && near(sleepyFrame.y, 6 * 92.0 / 96, 0.05),
          "sleepy on the face: flat 40 px slits, z's, the gaze sunk (Reduce Motion)")
    let laughFrame = settle(.speaking, .laughing)
    check(near(laughFrame.smile, 1.4, 0.01) && laughFrame.overlays.isEmpty, "laughing on the face: the deep smile lid")
    let calmFrame = settle(.idle, .calm)
    check(calmFrame.flat == 0 && calmFrame.closed == 0 && calmFrame.overlays.isEmpty && calmFrame.x == 0,
          "calm draws none of it")
    // Live motion: laughing bounces the pair, and marks shrink away when the expression ends.
    let motion = FaceMotion(random: { 0.5 })
    var ys: [Double] = []
    for i in 0...90 {
        let f = motion.step(now: Double(i) / 60, mood: .idle, level: 0, expression: FaceExpressionShown(id: 1, label: .laughing),
                            reduceMotion: false)
        ys.append(f.y)
    }
    check((ys.max() ?? 0) - (ys.min() ?? 0) > 3, "laughing bounces the eyes on the face: \((ys.max() ?? 0) - (ys.min() ?? 0)) pt")
    let marks = FaceMotion(random: { 0.5 })
    var f = FaceFrame(x: 0, y: 0, open: 1, wide: 1)
    for i in 0...60 {
        f = marks.step(now: Double(i) / 60, mood: .idle, level: 0, expression: FaceExpressionShown(id: 2, label: .flattered),
                       reduceMotion: false)
    }
    check(f.overlays.count == 2 && f.overlays.allSatisfy { $0.scale > 0.9 }, "flattered's blush is drawn")
    let gone = marks.step(now: 1.02, mood: .idle, level: 0, expression: nil, reduceMotion: false)
    check(gone.overlays.count == 2 && gone.overlays.allSatisfy { $0.scale > 0.5 }, "and shrinks, not snaps, when it ends")
    for i in 62...120 { f = marks.step(now: Double(i) / 60, mood: .idle, level: 0, expression: nil, reduceMotion: false) }
    check(f.overlays.isEmpty, "then it is gone")
    // The motion runs from when the id first showed: a new id starts it again.
    let again = FaceMotion(random: { 0.5 })
    var g = FaceFrame(x: 0, y: 0, open: 1, wide: 1)
    for i in 0...150 {
        g = again.step(now: Double(i) / 60, mood: .idle, level: 0, expression: FaceExpressionShown(id: 3, label: .thinking),
                       reduceMotion: false)
    }
    check(g.x > 10, "thinking looks right after its saccade: \(g.x)")
    for i in 151...170 {
        g = again.step(now: Double(i) / 60, mood: .idle, level: 0, expression: FaceExpressionShown(id: 4, label: .thinking),
                       reduceMotion: false)
    }
    check(g.x < 0, "a new thinking request looks left first again: \(g.x)")
    // Cut off while the eyes are shut (the owner talks, the time to live ends, the next sentence's pick): the
    // eyes reopen over a few frames, not in one.
    for (label, at) in [(FaceExpressionLabel.relieved, 0.3), (.proud, 1.65)] {
        let cut = FaceMotion(random: { 0.5 })
        var h = FaceFrame(x: 0, y: 0, open: 1, wide: 1)
        for i in 0...Int(at * 60) {
            h = cut.step(now: Double(i) / 60, mood: .speaking, level: 0, expression: FaceExpressionShown(id: 5, label: label),
                         reduceMotion: false)
        }
        let before = h.left
        let next = cut.step(now: (at * 60 + 1) / 60, mood: .listening, level: 0, expression: nil, reduceMotion: false)
        var later = next
        for i in 2...30 { later = cut.step(now: (at * 60 + Double(i)) / 60, mood: .listening, level: 0, expression: nil,
                                           reduceMotion: false) }
        check(before < 0.1 && next.left < 0.5 && later.left > 0.9,
              "\(label.rawValue) cut off shut reopens over frames: \(before) -> \(next.left) -> \(later.left)")
    }
    // VoiceOver names each.
    check(FaceExpressionLabel.allCases.allSatisfy { StackChanFace.spoken(.speaking, $0) == "speaking, \($0.spoken)" }
          && StackChanFace.spoken(.thinking, .thinking) == "thinking, mulling it over", "VoiceOver names every expression")
}

MainActor.assumeIsolated { run() }
