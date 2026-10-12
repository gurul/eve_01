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
    print(fails == 0 ? "FACE_OK" : "FACE_FAILED \(fails)")
}
MainActor.assumeIsolated { run() }
