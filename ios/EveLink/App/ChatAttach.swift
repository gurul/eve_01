// The composer's attachments (docs/app-chat.md, "Attachments"): the "+" menu (Photos, Files), the small removable
// previews above the field, and turning what was picked into bytes the daemon takes. A photo goes as a JPEG of at most
// 2048 px on its long side (a HEIC photo too, as Telegram sends one), so it stays well under the daemon's image limit;
// a file goes as it is, up to 20 MB. Uploading and sending are ChatComposer's (ChatScreen.swift).
import ImageIO
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// One photo or file picked in the composer and not sent yet.
struct PendingAttachment: Identifiable, Equatable, Sendable {
    let id = UUID()
    var isImage: Bool
    var name: String
    var mime: String
    var data: Data
    /// A small JPEG: the preview here, and the owner's bubble on the page (as a data: URL).
    var thumb: Data?

    var thumbDataURL: String? { thumb.map { "data:image/jpeg;base64," + $0.base64EncodedString() } }
}

enum AttachmentPrep {
    static let photoPixels = 2048
    static let thumbPixels = 240

    /// The file types Files offers: what the daemon takes (link_uploads.py).
    static let fileTypes: [UTType] = {
        var types: [UTType] = [.pdf, .image, .plainText, .commaSeparatedText, .tabSeparatedText, .json, .yaml, .rtf]
        for ext in ["md", "docx", "xlsx", "pptx"] {
            if let t = UTType(filenameExtension: ext) { types.append(t) }
        }
        return types
    }()

    /// Image bytes in any format ImageIO reads (HEIC included) → a JPEG of at most `maxPixels`, upright.
    nonisolated static func jpeg(_ data: Data, maxPixels: Int, quality: Double) -> Data? {
        guard let src = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let opts: [CFString: Any] = [kCGImageSourceCreateThumbnailFromImageAlways: true,
                                     kCGImageSourceCreateThumbnailWithTransform: true,
                                     kCGImageSourceThumbnailMaxPixelSize: maxPixels]
        guard let image = CGImageSourceCreateThumbnailAtIndex(src, 0, opts as CFDictionary) else { return nil }
        let out = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(out, UTType.jpeg.identifier as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(dest, image, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
        return CGImageDestinationFinalize(dest) ? out as Data : nil
    }

    /// A photo's bytes → the attachment, or nil when they are not an image.
    @concurrent
    static func photo(_ data: Data, name: String) async -> PendingAttachment? {
        guard let full = jpeg(data, maxPixels: photoPixels, quality: 0.82) else { return nil }
        let stem = (name as NSString).deletingPathExtension
        return PendingAttachment(isImage: true, name: (stem.isEmpty ? "Photo" : stem) + ".jpg", mime: "image/jpeg",
                                 data: full, thumb: jpeg(full, maxPixels: thumbPixels, quality: 0.7))
    }

    /// One file from Files → the attachment, or a sentence saying why not.
    @concurrent
    static func file(_ url: URL) async -> Result<PendingAttachment, AttachError> {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let size = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
        guard size <= ChatAttachments.maxBytes else { return .failure(AttachError("That file is over 20 MB.")) }
        guard let data = try? Data(contentsOf: url, options: .mappedIfSafe), !data.isEmpty,
              data.count <= ChatAttachments.maxBytes else {
            return .failure(AttachError("Couldn't read that file."))
        }
        let type = UTType(filenameExtension: url.pathExtension)
        if type?.conforms(to: .image) == true {
            guard let a = await photo(data, name: url.lastPathComponent) else {
                return .failure(AttachError("Couldn't read that image."))
            }
            return .success(a)
        }
        return .success(PendingAttachment(isImage: false, name: url.lastPathComponent,
                                          mime: type?.preferredMIMEType ?? "application/octet-stream", data: data))
    }
}

struct AttachError: Error, Sendable {
    var message: String
    init(_ message: String) { self.message = message }
}

/// The "+" at the composer's left: Photos and Files (off when four are attached), and Commands, which types "/" and
/// opens the / menu (ChatSlash.swift).
struct AttachButton: View {
    var full: Bool
    var ready = true
    var photos: () -> Void
    var files: () -> Void
    var commands: () -> Void = {}

    var body: some View {
        Menu {
            Button(action: photos) { Label("Photos", systemImage: "photo.on.rectangle") }.disabled(full)
            Button(action: files) { Label("Files", systemImage: "folder") }.disabled(full)
            Button(action: commands) { Label("Commands", systemImage: "slash.circle") }
        } label: {
            Image(systemName: "plus")
                .font(.system(size: 20, weight: .regular))
                .foregroundStyle(Color.primary.opacity(ready ? 1 : 0.3))
                .frame(width: 36, height: 36)
                .contentShape(Rectangle())
        }
        .menuOrder(.fixed)
        .disabled(!ready)
        .accessibilityLabel("Add")
        .accessibilityHint(full ? "One of eve's commands. Four attachments at most" : "A photo, a file, or one of eve's commands")
    }
}

/// The picked attachments above the field: a photo as a rounded thumbnail, a file as a small card with its name; each
/// with an x to take it off.
struct AttachmentStrip: View {
    var items: [PendingAttachment]
    var remove: (PendingAttachment.ID) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(items) { item in
                    preview(item)
                        .overlay(alignment: .topTrailing) {
                            Button { remove(item.id) } label: {
                                Image(systemName: "xmark")
                                    .font(.system(size: 9, weight: .bold))
                                    .foregroundStyle(Color(uiColor: .systemBackground))
                                    .frame(width: 20, height: 20)
                                    .background(Circle().fill(Color.primary))
                                    .frame(width: 32, height: 32)        // a finger-sized target around the dot
                                    .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .offset(x: 10, y: -10)
                            .accessibilityLabel("Remove \(item.name)")
                        }
                }
            }
            .padding(.top, 10)
            .padding(.trailing, 10)
            .padding(.horizontal, 2)
        }
        .scrollClipDisabled()
    }

    @ViewBuilder private func preview(_ item: PendingAttachment) -> some View {
        if item.isImage, let data = item.thumb, let image = UIImage(data: data) {
            Image(uiImage: image)
                .resizable()
                .scaledToFill()
                .frame(width: 56, height: 56)
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                .accessibilityLabel("Photo \(item.name)")
        } else {
            HStack(spacing: 8) {
                Image(systemName: item.isImage ? "photo" : "doc.text")
                    .font(.system(size: 18))
                    .foregroundStyle(.secondary)
                Text(item.name)
                    .font(.footnote)
                    .lineLimit(2)
                    .truncationMode(.middle)
                    .frame(maxWidth: 110, alignment: .leading)
            }
            .padding(.horizontal, 10)
            .frame(height: 56)
            .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Color.eveSelected))
            .accessibilityElement(children: .combine)
            .accessibilityLabel("File \(item.name)")
        }
    }
}
