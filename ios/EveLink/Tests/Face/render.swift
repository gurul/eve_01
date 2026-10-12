// A contact sheet of every expression on the voice-mode face (App/StackChanFace.swift), drawn by the app's own code:
// each label at 0.3 s, 2 s and 3 s after it shows (3 s is inside every time to live, so it shows the settled poses),
// on the idle face. Run Tests/Face/render.sh; it writes ios/EveLink/build/expressions.png (ignored by git). Not a
// test: it is for looking.
//   render [out.png] [--phone] [--only label,label,...]
// --phone draws at true phone scale: one pixel per point, each face in a column as wide as an iPhone screen (393 pt),
// so marks that are specks on the phone are specks on the sheet too. --only draws just the labels named.
import AppKit
import SwiftUI

@main
struct RenderExpressions {
    static let times = [0.3, 2.0, 3.0]
    static let eye = CGSize(width: 74, height: 92)
    static let phoneWidth: CGFloat = 393

    /// The face `secs` after `label` showed, stepped at 60 fps from rest. No blink or glance lands before 2.6 s; by 3 s a
    /// quick-blinking label (excited) may be mid-blink.
    @MainActor static func frame(_ label: FaceExpressionLabel, at secs: Double) -> FaceFrame {
        let motion = FaceMotion(random: { 0.5 })
        let shown = FaceExpressionShown(id: 1, label: label)
        var f = motion.step(now: 0, mood: .idle, level: 0, expression: shown, reduceMotion: false)
        for i in 1...Int((secs * 60).rounded()) {
            f = motion.step(now: Double(i) / 60, mood: .idle, level: 0, expression: shown, reduceMotion: false)
        }
        return f
    }

    @MainActor static func main() {
        var args = Array(CommandLine.arguments.dropFirst())
        let phone = args.contains("--phone")
        args.removeAll { $0 == "--phone" }
        var only: Set<String>?
        if let i = args.firstIndex(of: "--only"), i + 1 < args.count {
            only = Set(args[i + 1].split(separator: ",").map(String.init))
            args.removeSubrange(i...(i + 1))
        }
        let size = CGSize(width: eye.width * 2 + StackChanFace.gap + StackChanFace.margin.width * 2,
                          height: eye.height * 1.2 + StackChanFace.margin.height * 2)
        let column = phone ? max(phoneWidth, size.width) : size.width
        let labels = FaceExpressionLabel.allCases.filter { only?.contains($0.rawValue) ?? true }
        guard !labels.isEmpty else { print("RENDER_FAILED: no such label in --only"); exit(1) }
        let sheet = VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 0) {
                ForEach(times, id: \.self) { t in
                    Text(String(format: "%.1f s", t)).frame(width: column).foregroundStyle(.gray)
                }
            }.padding(.leading, 100)
            ForEach(labels, id: \.self) { label in
                HStack(spacing: 0) {
                    Text(label.rawValue).font(.system(size: 15, weight: .medium)).foregroundStyle(.white)
                        .frame(width: 100, alignment: .leading)
                    ForEach(times, id: \.self) { t in
                        let f = frame(label, at: t)
                        Canvas { gc, canvas in StackChanFace.draw(f, eye: eye, in: &gc, size: canvas) }
                            .frame(width: size.width, height: size.height)
                            .frame(width: column)
                            .border(Color(white: 0.15))
                    }
                }
            }
        }
        .padding(16)
        .background(Color.black)
        let renderer = ImageRenderer(content: sheet)
        renderer.scale = phone ? 1 : 2
        guard let image = renderer.cgImage else { print("RENDER_FAILED: no image"); exit(1) }
        let out = URL(fileURLWithPath: args.first ?? "ios/EveLink/build/expressions.png")
        try? FileManager.default.createDirectory(at: out.deletingLastPathComponent(), withIntermediateDirectories: true)
        let rep = NSBitmapImageRep(cgImage: image)
        guard let png = rep.representation(using: .png, properties: [:]), (try? png.write(to: out)) != nil else {
            print("RENDER_FAILED: could not write \(out.path)"); exit(1)
        }
        print("wrote \(out.path) (\(image.width) x \(image.height), \(labels.count) expressions x \(times.count) times"
              + (phone ? ", phone scale)" : ")"))
    }
}
