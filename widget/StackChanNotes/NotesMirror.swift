// Watches the daemon's notes directory and mirrors it into the App Group container.
//
// Two watchers plus a timer, because a directory vnode does not change when a file
// inside it is appended to:
//   * DispatchSource on the directory fd  — new day files, renames, deletes
//   * DispatchSource on the newest day file — appends from the daemon
//   * 30 s timer                           — belt and braces (directory not yet created, missed events)
// Every trigger re-reads, and only a changed note list is written + pushed to WidgetKit.

import Foundation
import Observation
import WidgetKit
import os

@MainActor
@Observable
final class NotesMirror {
    static let shared = NotesMirror()

    static let envKey = "EVE_NOTES_DIR"
    static let defaultDir = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent(".config/eve-bridge/notes", isDirectory: true)

    let notesDir: URL
    private(set) var count = 0
    /// The last snapshot read (thoughts, notes, profile, reflections) — the diary window reads it.
    private(set) var snapshot: NotesSnapshot = .empty
    private(set) var lastSync: Date?
    private(set) var lastError: String?

    private let log = Logger(subsystem: "com.github.eve-bridge.StackChanNotes", category: "mirror")
    private var dirSource: DispatchSourceFileSystemObject?
    private var storeSource: DispatchSourceFileSystemObject?
    private var fileSource: DispatchSourceFileSystemObject?
    private var watchedFile: URL?
    private var timer: Timer?
    private var pending: Task<Void, Never>?
    /// What was last written to the App Group, as content bytes. One value rather
    /// than a field-by-field comparison, because the field-by-field version
    /// silently stopped covering the snapshot when it grew (`NoteStore.contentKey`).
    private var lastWritten: Data?

    private init() {
        if let override = ProcessInfo.processInfo.environment[Self.envKey], !override.isEmpty {
            notesDir = URL(fileURLWithPath: (override as NSString).expandingTildeInPath, isDirectory: true)
        } else {
            notesDir = Self.defaultDir
        }
    }

    func start() {
        log.info("watching \(self.notesDir.path, privacy: .public)")
        sync()
        timer = Timer.scheduledTimer(withTimeInterval: 30, repeats: true) { _ in
            Task { @MainActor in NotesMirror.shared.sync() }
        }
    }

    /// Coalesces bursts of file-system events into one read.
    private func scheduleSync() {
        pending?.cancel()
        pending = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled else { return }
            self?.sync()
        }
    }

    /// Star a claim into highlights.md, then re-sync so the widget and the diary window see it.
    func star(_ claim: String) {
        do {
            try NoteStore.star(claim, notesDir: notesDir)
            lastError = nil
        } catch {
            lastError = error.localizedDescription
            log.error("star failed: \(error.localizedDescription, privacy: .public)")
        }
        sync()
    }

    func sync() {
        do {
            if try LearningSnapshot.mirror() { WidgetCenter.shared.reloadAllTimelines() }
        } catch {
            log.error("learning mirror failed: \(error.localizedDescription, privacy: .public)")
        }
        armDirWatcher()
        let (snapshot, newest) = NoteStore.read(notesDir: notesDir)
        armFileWatcher(newest)

        count = max(snapshot.notes.count, snapshot.thoughts.filter(\.written).count)
        lastSync = .now
        self.snapshot = snapshot
        let key = NoteStore.contentKey(snapshot)
        guard key == nil || key != lastWritten else { return }

        do {
            let photos = NoteStore.mirrorPhotos(snapshot.thoughts, notesDir: notesDir)
            if photos > 0 { log.info("mirrored \(photos) new photo(s)") }
            try NoteStore.save(snapshot)
            lastWritten = key
            lastError = nil
            log.info("mirrored \(snapshot.notes.count) notes, \(snapshot.thoughts.count) thoughts → \(AppGroup.notesFileURL?.path ?? "?", privacy: .public)")
            WidgetCenter.shared.reloadAllTimelines()
        } catch {
            lastError = error.localizedDescription
            log.error("mirror failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    // MARK: watchers

    private func armDirWatcher() {
        if dirSource == nil {
            dirSource = makeSource(for: notesDir, mask: [.write, .rename, .delete, .attrib, .link])
            if dirSource != nil { log.debug("dir watcher armed") }
        }
        // eve's spoken memory lives in its memory folder, so a new dream journal
        // shows up here rather than in the notes directory. The deepest folder that
        // exists is watched; an appended star changes no directory, so the 30 s
        // timer is what picks that up.
        if storeSource == nil {
            let store = NoteStore.defaultMemoryDir()
            let fm = FileManager.default
            let target = [NoteStore.journalDir(store: store), NoteStore.recordsDir(store: store)]
                .first { fm.fileExists(atPath: $0.path) } ?? store
            storeSource = makeSource(for: target, mask: [.write, .rename, .delete, .attrib, .link])
            if storeSource != nil { log.debug("store watcher armed on \(target.lastPathComponent, privacy: .public)") }
        }
    }

    private func armFileWatcher(_ file: URL?) {
        if watchedFile == file, fileSource != nil { return }
        fileSource?.cancel()
        fileSource = nil
        watchedFile = file
        guard let file else { return }
        fileSource = makeSource(for: file, mask: [.write, .extend, .rename, .delete, .attrib])
        if fileSource != nil { log.debug("file watcher armed on \(file.lastPathComponent, privacy: .public)") }
    }

    /// Opens an O_EVTONLY fd on `url` and returns a resumed source. On rename/delete the
    /// source cancels itself (the fd is dead) so the next sync re-arms against the new inode.
    private func makeSource(for url: URL, mask: DispatchSource.FileSystemEvent) -> DispatchSourceFileSystemObject? {
        let fd = open(url.path, O_EVTONLY)
        guard fd >= 0 else { return nil }
        let source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: fd, eventMask: mask, queue: .main)
        source.setCancelHandler { close(fd) }
        source.setEventHandler { [weak self, weak source] in
            guard let source else { return }
            let gone = source.data.contains(.rename) || source.data.contains(.delete)
            if gone { source.cancel() }
            Task { @MainActor in
                guard let self else { return }
                if gone {
                    if self.dirSource === source { self.dirSource = nil }
                    if self.fileSource === source { self.fileSource = nil; self.watchedFile = nil }
                }
                self.scheduleSync()
            }
        }
        source.resume()
        return source
    }
}
