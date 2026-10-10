// Notes (docs/app-chat.md, "Notes"): the owner's second-brain vault as a small native Notion. Owner, 2026-10-10: "an
// app where I can look at my notes and interact with them change delete updates", "essentially like a mini notion",
// "make sure it's mobile native". The list is the vault's folders, collapsible, newest first; a page renders its
// markdown (headings, to-dos you tick, lists, quotes, code) and Edit turns it into a text editor. New, Move, Delete
// (with Undo) and search are here too. Every change goes to the Mac (link_notes.py), which keeps the file's
// frontmatter, refuses a save over a version changed since, and keeps a delete restorable.
import SwiftUI

/// Which note a row or a New note opens.
struct NoteRef: Hashable, Identifiable {
    var path: String
    var id: String { path }
}

/// A delete that can still be undone: the bar at the bottom of the list.
struct NoteDeleted: Equatable {
    var title: String
    var trashID: String
}

private let noAnswer = "eve didn't answer. Is the Mac awake?"

// ---- the list ------------------------------------------------------------------------------------------------------

struct NotesControls: View {
    let client: WorkClient
    @State private var notes: [NoteSummary] = []
    @State private var folders: [String] = []
    @State private var loaded = false
    @State private var line = ""
    @State private var failed = false
    @State private var query = ""
    @State private var hits: [NoteHit] = []
    @State private var hitsFor = ""                    // the query the hits are for
    @State private var collapsed: Set<String> = ["09-archive"]
    @State private var newNote = false
    @State private var created: NoteRef?               // a New note's page, opened once its sheet has gone
    @State private var opened: NoteRef?
    @State private var deleted: NoteDeleted?
    @State private var done = 0
    @State private var fails = 0

    var body: some View {
        List {
            if !loaded {
                quiet("Loading…")
            } else if !query.isEmpty {
                searchRows
            } else if notes.isEmpty {
                quiet(line.isEmpty ? "No notes yet. Tap the pencil to start one." : line)
            } else {
                ForEach(NotesText.group(notes, order: folders)) { folder in
                    Section(isExpanded: expanded(folder.path)) {
                        ForEach(folder.notes) { row($0) }
                    } header: {
                        Label(folder.name, systemImage: folder.symbol)
                    }
                }
            }
        }
        .listStyle(.sidebar)
        .scrollContentBackground(.hidden)
        .searchable(text: $query, prompt: "Search notes")
        .onSubmit(of: .search) { Task { await search() } }
        .onChange(of: query) { _, now in if now.isEmpty { hits = []; hitsFor = "" } }
        .refreshable { await load() }
        .task {
            await load()
            #if DEBUG && targetEnvironment(simulator)
            if let path = DesignHook.note, !path.isEmpty { opened = NoteRef(path: path) }
            #endif
        }
        .navigationDestination(for: NoteRef.self) { page($0) }
        .navigationDestination(item: $opened) { page($0) }
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button { newNote = true } label: { Image(systemName: "square.and.pencil") }
                    .accessibilityLabel("New note")
            }
        }
        .sheet(isPresented: $newNote, onDismiss: {
            if let made = created {
                created = nil
                opened = made
            }
        }) {
            NewNoteSheet(client: client, folders: folders.isEmpty ? ["01-inbox"] : folders) { made in
                created = NoteRef(path: made)
                Task { await load() }
            }
        }
        .safeAreaInset(edge: .bottom) { undoBar }
        .task(id: deleted?.trashID) {
            guard let shown = deleted?.trashID else { return }
            try? await Task.sleep(for: .seconds(6))
            if deleted?.trashID == shown { withAnimation { deleted = nil } }
        }
        .sensoryFeedback(.success, trigger: done)
        .sensoryFeedback(.error, trigger: fails)
    }

    private func page(_ ref: NoteRef) -> some View {
        NotePage(client: client, path: ref.path, folders: folders,
                 changed: { Task { await load() } },
                 deleted: { gone in
                     notes.removeAll { $0.path == ref.path }
                     withAnimation { deleted = gone }
                     Task { await load() }
                 })
    }

    private func expanded(_ folder: String) -> Binding<Bool> {
        Binding(get: { !collapsed.contains(folder) },
                set: { open in if open { collapsed.remove(folder) } else { collapsed.insert(folder) } })
    }

    private func row(_ n: NoteSummary) -> some View {
        NavigationLink(value: NoteRef(path: n.path)) {
            VStack(alignment: .leading, spacing: 3) {
                Text(n.title).font(.body).lineLimit(1)
                HStack(spacing: 6) {
                    Text(NotesText.stamp(n.modified))
                    if !n.preview.isEmpty { Text(n.preview).lineLimit(1) }
                }
                .font(.subheadline)
                .foregroundStyle(.secondary)
            }
            .padding(.vertical, 2)
        }
        .swipeActions(edge: .trailing) {
            Button(role: .destructive) { Task { await delete(n) } } label: { Label("Delete", systemImage: "trash") }
        }
        .contextMenu {
            moveMenu(n)
            Button(role: .destructive) { Task { await delete(n) } } label: { Label("Delete", systemImage: "trash") }
        }
    }

    private func moveMenu(_ n: NoteSummary) -> some View {
        Menu {
            ForEach(folders.filter { $0 != n.folder }, id: \.self) { f in
                Button { Task { await move(n, into: f) } } label: {
                    Label(NotesText.folderName(f), systemImage: NotesText.folderSymbol(f))
                }
            }
        } label: {
            Label("Move to", systemImage: "folder")
        }
    }

    @ViewBuilder private var searchRows: some View {
        let titled = NotesText.filter(notes, query)
        Section("Notes") {
            if titled.isEmpty { quiet("No title has those words.") }
            ForEach(titled) { row($0) }
        }
        if hitsFor == query.trimmingCharacters(in: .whitespaces), !hits.isEmpty {
            Section("In the text") {
                ForEach(hits) { h in
                    NavigationLink(value: NoteRef(path: h.path)) {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(h.title).font(.body).lineLimit(1)
                            Text(h.snippet).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
                        }
                    }
                }
            }
        } else if hitsFor.isEmpty {
            Section { quiet("Press Search to look inside every note.") }
        } else if hitsFor == query.trimmingCharacters(in: .whitespaces) {
            Section { quiet(line.isEmpty ? "No note has those words." : line) }
        }
    }

    @ViewBuilder private var undoBar: some View {
        if let gone = deleted {
            HStack(spacing: 12) {
                Text("Deleted \u{201C}\(gone.title)\u{201D}").lineLimit(1)
                Spacer()
                Button("Undo") { Task { await restore(gone) } }.bold()
            }
            .font(.subheadline)
            .padding(.horizontal, 18)
            .frame(minHeight: 48)
            .background(.regularMaterial, in: Capsule())
            .padding(.horizontal, 16)
            .padding(.bottom, 8)
            .transition(.move(edge: .bottom).combined(with: .opacity))
        } else if failed, !line.isEmpty, loaded {
            Text(line).font(.footnote).foregroundStyle(.red).padding(.bottom, 8)
        }
    }

    private func quiet(_ text: String) -> some View {
        Text(text).font(.subheadline).foregroundStyle(.secondary).listRowSeparator(.hidden)
    }

    // MARK: Calls

    private func load() async {
        switch await client.notes(["action": "list"]) {
        case .success(let r) where r.ok:
            notes = r.notes
            folders = r.folders
            line = ""
            failed = false
        case .success(let r):
            line = r.line
            failed = true
        case .failure(let f):
            line = f.message
            failed = true
        }
        loaded = true
    }

    private func search() async {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return }
        switch await client.notes(["action": "search", "query": q]) {
        case .success(let r):
            hits = r.ok ? r.hits : []
            line = r.line
        case .failure(let f):
            hits = []
            line = f.message
        }
        hitsFor = q
    }

    private func delete(_ n: NoteSummary) async {
        // the Mac deletes only the version the phone has seen: read it, then delete that revision
        guard case .success(let page) = await client.notes(["action": "read", "path": n.path]), page.ok,
              let revision = page.revision else {
            return fail("That note couldn't be deleted just now.")
        }
        switch await client.notes(["action": "delete", "path": n.path, "revision": revision]) {
        case .success(let r) where r.ok:
            notes.removeAll { $0.path == n.path }
            if let id = r.trashID { withAnimation { deleted = NoteDeleted(title: n.title, trashID: id) } }
            done += 1
        case .success(let r): fail(r.line)
        case .failure(let f): fail(f.message)
        }
    }

    private func restore(_ gone: NoteDeleted) async {
        withAnimation { deleted = nil }
        switch await client.notes(["action": "restore", "trash_id": gone.trashID]) {
        case .success(let r) where r.ok:
            done += 1
            await load()
        case .success(let r): fail(r.line)
        case .failure(let f): fail(f.message)
        }
    }

    private func move(_ n: NoteSummary, into folder: String) async {
        switch await client.notes(["action": "move", "path": n.path, "into": folder]) {
        case .success(let r) where r.ok:
            done += 1
            await load()
        case .success(let r): fail(r.line)
        case .failure(let f): fail(f.message)
        }
    }

    private func fail(_ text: String) {
        line = text.isEmpty ? noAnswer : text
        failed = true
        fails += 1
        AccessibilityNotification.Announcement(line).post()
    }
}

// ---- a page --------------------------------------------------------------------------------------------------------

/// One note: its markdown drawn natively, to-dos ticked with a tap, Edit for the text itself. A save sends the
/// revision the phone read; when the file changed on the Mac since, nothing is written and the page says so.
struct NotePage: View {
    let client: WorkClient
    let folders: [String]
    let changed: () -> Void
    let deleted: (NoteDeleted) -> Void
    @State private var path: String
    @State private var page: NotesReply?
    @State private var text = ""                     // the body as the Mac has it
    @State private var draft = ""
    @State private var editing = false
    @State private var busy = false
    @State private var line = ""
    @State private var failed = false
    @State private var conflict = false
    @State private var confirmDelete = false
    @State private var done = 0
    @State private var fails = 0
    @FocusState private var typing: Bool
    @Environment(\.dismiss) private var dismiss

    init(client: WorkClient, path: String, folders: [String], changed: @escaping () -> Void,
         deleted: @escaping (NoteDeleted) -> Void) {
        self.client = client
        self.folders = folders
        self.changed = changed
        self.deleted = deleted
        _path = State(initialValue: path)
    }

    private var folder: String { path.contains("/") ? String(path[..<path.lastIndex(of: "/")!]) : "" }

    var body: some View {
        Group {
            if editing {
                TextEditor(text: $draft)
                    .font(.body)
                    .focused($typing)
                    .scrollContentBackground(.hidden)
                    .padding(.horizontal, 14)
            } else if let page {
                ScrollView {
                    VStack(alignment: .leading, spacing: 10) {
                        if !page.front.isEmpty {
                            Text(page.front.joined(separator: "  ·  "))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(2)
                        }
                        let blocks = NotesText.blocks(text)
                        if blocks.isEmpty {
                            Text(page.editable ? "An empty page. Tap Edit to write." : "An empty page.")
                                .foregroundStyle(.secondary)
                        }
                        ForEach(blocks) { block in
                            BlockRow(block: block, busy: busy) { Task { await tick(block) } }
                        }
                        status
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 20)
                    .padding(.vertical, 16)
                    .textSelection(.enabled)
                }
                .refreshable { await load() }
            } else {
                VStack(spacing: 12) {
                    if failed { status } else { ProgressView() }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Color.eveBackground)
        .safeAreaInset(edge: .bottom) { if editing, !line.isEmpty { status.padding(.bottom, 6) } }
        .navigationTitle(NotesText.folderName(folder))
        .navigationBarTitleDisplayMode(.inline)
        .navigationBarBackButtonHidden(editing)
        .toolbar { toolbar }
        .task { await load() }
        .confirmationDialog("Delete this note?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete", role: .destructive) { Task { await delete() } }
            Button("Keep", role: .cancel) {}
        } message: {
            Text("You can undo it from the list right after.")
        }
        .sensoryFeedback(.success, trigger: done)
        .sensoryFeedback(.error, trigger: fails)
    }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        if editing {
            ToolbarItem(placement: .topBarLeading) {
                Button("Cancel") {
                    editing = false
                    typing = false
                    line = ""
                }
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button("Done") { Task { await saveDraft() } }.bold().disabled(busy)
            }
        } else if let page {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Menu {
                        ForEach(folders.filter { $0 != folder }, id: \.self) { f in
                            Button { Task { await move(into: f) } } label: {
                                Label(NotesText.folderName(f), systemImage: NotesText.folderSymbol(f))
                            }
                        }
                    } label: { Label("Move to", systemImage: "folder") }
                    if page.undoID != nil {
                        Button { Task { await undo() } } label: { Label("Undo last change", systemImage: "arrow.uturn.backward") }
                    }
                    Button(role: .destructive) { confirmDelete = true } label: { Label("Delete", systemImage: "trash") }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .accessibilityLabel("More")
            }
            if page.editable {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Edit") {
                        draft = text
                        line = ""
                        editing = true
                        typing = true
                    }
                }
            }
        }
    }

    @ViewBuilder private var status: some View {
        if !line.isEmpty {
            VStack(spacing: 8) {
                Text(line)
                    .font(.footnote)
                    .foregroundStyle(failed ? Color.red : Color.secondary)
                    .multilineTextAlignment(.center)
                if conflict {
                    Button("Open the Mac's version") {
                        editing = false
                        Task { await load() }
                    }
                    .font(.footnote.bold())
                }
            }
            .frame(maxWidth: .infinity)
        }
    }

    // MARK: Calls

    private func load() async {
        switch await client.notes(["action": "read", "path": path]) {
        case .success(let r) where r.ok:
            page = r
            text = r.body ?? ""
            line = ""
            failed = false
            conflict = false
        case .success(let r): fail(r.line)
        case .failure(let f): fail(f.message)
        }
    }

    private func tick(_ block: NoteBlock) async {
        guard !busy, page?.editable == true, let toggled = NotesText.toggle(text, line: block.line) else { return }
        let before = text
        text = toggled                                  // shown at once; put back if the Mac says no
        if await save(toggled) { done += 1 } else { text = before }
    }

    private func saveDraft() async {
        if draft == text {
            editing = false
            typing = false
            return
        }
        if await save(draft) {
            text = draft
            editing = false
            typing = false
            done += 1
        }
    }

    /// Sends the body with the revision read; true when the Mac kept it.
    private func save(_ body: String) async -> Bool {
        guard let revision = page?.revision else { return false }
        busy = true
        defer { busy = false }
        switch await client.notes(["action": "save", "path": path, "revision": revision, "body": body]) {
        case .success(let r) where r.ok:
            page?.revision = r.revision ?? revision
            if r.changed == true { page?.undoID = r.undoID }
            line = ""
            failed = false
            conflict = false
            changed()
            return true
        case .success(let r):
            conflict = r.line.hasPrefix("This note changed")
            fail(r.line)
        case .failure(let f):
            fail(f.message)
        }
        return false
    }

    private func undo() async {
        guard let revision = page?.revision, let undoID = page?.undoID else { return }
        switch await client.notes(["action": "undo", "path": path, "revision": revision, "undo_id": undoID]) {
        case .success(let r) where r.ok:
            done += 1
            await load()
            changed()
        case .success(let r):
            conflict = r.line.hasPrefix("This note changed")
            fail(r.line)
        case .failure(let f): fail(f.message)
        }
    }

    private func move(into f: String) async {
        switch await client.notes(["action": "move", "path": path, "into": f]) {
        case .success(let r) where r.ok:
            if let moved = r.path { path = moved }
            done += 1
            changed()
        case .success(let r): fail(r.line)
        case .failure(let f): fail(f.message)
        }
    }

    private func delete() async {
        guard let revision = page?.revision else { return }
        switch await client.notes(["action": "delete", "path": path, "revision": revision]) {
        case .success(let r) where r.ok:
            deleted(NoteDeleted(title: page?.title ?? "the note", trashID: r.trashID ?? ""))
            dismiss()
        case .success(let r):
            conflict = r.line.hasPrefix("This note changed")
            fail(r.line)
        case .failure(let f): fail(f.message)
        }
    }

    private func fail(_ text: String) {
        line = text.isEmpty ? noAnswer : text
        failed = true
        fails += 1
        AccessibilityNotification.Announcement(line).post()
    }
}

/// One block of a page, drawn as its kind: a to-do's box is a 44 pt button; inline markdown (bold, links) is kept.
private struct BlockRow: View {
    let block: NoteBlock
    let busy: Bool
    let tick: () -> Void

    var body: some View {
        content.padding(.leading, CGFloat(min(block.indent, 6)) * 20)
    }

    @ViewBuilder private var content: some View {
        switch block.kind {
        case .heading(let level):
            Text(inline)
                .font(level == 1 ? .title.bold() : level == 2 ? .title2.bold() : .title3.weight(.semibold))
                .padding(.top, level == 1 ? 0 : 8)
                .accessibilityAddTraits(.isHeader)
        case .todo(let done):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Button(action: tick) {
                    Image(systemName: done ? "checkmark.square.fill" : "square")
                        .font(.title3)
                        .foregroundStyle(done ? Color.accentColor : Color.secondary)
                        .frame(width: 30, height: 30)
                        .contentShape(Rectangle().inset(by: -7))
                }
                .buttonStyle(.plain)
                .disabled(busy)
                .accessibilityLabel(done ? "Done" : "Not done")
                .accessibilityHint(done ? "Marks it not done" : "Marks it done")
                Text(inline)
                    .strikethrough(done, color: .secondary)
                    .foregroundStyle(done ? .secondary : .primary)
            }
        case .bullet:
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text("•").foregroundStyle(.secondary).accessibilityHidden(true)
                Text(inline)
            }
        case .numbered(let n):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(n).monospacedDigit().foregroundStyle(.secondary)
                Text(inline)
            }
        case .quote:
            HStack(spacing: 10) {
                RoundedRectangle(cornerRadius: 1.5).fill(Color.secondary.opacity(0.5)).frame(width: 3)
                Text(inline).italic().foregroundStyle(.secondary)
            }
            .fixedSize(horizontal: false, vertical: true)
        case .code:
            ScrollView(.horizontal, showsIndicators: false) {
                Text(block.text).font(.callout.monospaced()).padding(12)
            }
            .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Color.eveFill))
        case .rule:
            Divider().padding(.vertical, 6)
        case .text:
            Text(inline)
        }
    }

    private var inline: AttributedString {
        let words = NotesText.inline(block.text)
        return (try? AttributedString(markdown: words, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(words)
    }
}

// ---- a new note ----------------------------------------------------------------------------------------------------

private struct NewNoteSheet: View {
    let client: WorkClient
    let folders: [String]
    /// The new note's path: the caller opens it once this sheet has gone.
    let made: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var title = ""
    @State private var text = ""
    @State private var folder = "01-inbox"
    @State private var busy = false
    @State private var problem: String?
    @FocusState private var titleFocus: Bool

    init(client: WorkClient, folders: [String], made: @escaping (String) -> Void) {
        self.client = client
        self.folders = folders
        self.made = made
        _folder = State(initialValue: folders.contains("01-inbox") ? "01-inbox" : folders.first ?? "01-inbox")
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Title", text: $title)
                        .font(.title3.weight(.semibold))
                        .focused($titleFocus)
                        .submitLabel(.next)
                    Picker("Folder", selection: $folder) {
                        ForEach(folders, id: \.self) { f in
                            Label(NotesText.folderName(f), systemImage: NotesText.folderSymbol(f)).tag(f)
                        }
                    }
                }
                Section {
                    TextEditor(text: $text)
                        .frame(minHeight: 220)
                } header: {
                    Text("Page")
                } footer: {
                    Text("Markdown works: # for a heading, - for a list, - [ ] for a to-do.")
                }
                if let problem {
                    Section { Text(problem).foregroundStyle(.red) }
                }
            }
            .navigationTitle("New note")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") { Task { await create() } }
                        .bold()
                        .disabled(busy || title.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .onAppear { titleFocus = true }
        }
        .presentationDragIndicator(.visible)
    }

    private func create() async {
        busy = true
        defer { busy = false }
        switch await client.notes(["action": "create", "folder": folder, "title": title, "body": text]) {
        case .success(let r) where r.ok:
            if let path = r.path { made(path) }
            dismiss()
        case .success(let r):
            problem = r.line.isEmpty ? noAnswer : r.line
        case .failure(let f):
            problem = f.message
        }
    }
}
