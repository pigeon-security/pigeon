import SwiftUI

/// Source › Repository: resolve a revision of the instance's approved repository
/// (`source_resolve_revision`) and read its files at that commit (`source_read_file`), README
/// first. The tools read one regular file at a time; they list no directories. Read-only.
struct SourceScreen: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var record: JSON = .null
    @State private var revision = "HEAD"
    @State private var resolved: Loadable<JSON> = .loading
    @State private var path = "README.md"
    @State private var file: Loadable<JSON>?
    @State private var recent: [String] = []
    @State private var started = false

    /// Text longer than this is cut, so a large file does not stall the list.
    private static let shownLimit = 60_000

    private var id: String { context.instance.id }
    private var commit: String? { resolved.value?["commit"].string }

    private var repoName: String? {
        let r = record["repo"]
        if let s = r.string { return s }
        if let full = r["full_name"].string { return full }
        if let ns = r["namespace"].string { return "\(ns)/\(r["repo"].string ?? "")" }
        return nil
    }

    var body: some View {
        List {
            ViewPickerSection(context: context)
            UpgradeSection(context: context)
            Section {
                MutedNoteRow(repoName.map { "Read-only. \($0), the instance’s approved repository." }
                    ?? "Read-only. The instance’s approved repository.")
            }
            Section {
                HStack(spacing: 8) {
                    TextField("HEAD", text: $revision)
                        .font(Theme.Fonts.idBody)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .submitLabel(.go)
                        .onSubmit { Task { await resolveThenRead() } }
                    Button("Resolve") { Task { await resolveThenRead() } }
                        .buttonStyle(RowButtonStyle(.quiet))
                }
                switch resolved {
                case .loading:
                    LoadingRows(count: 1)
                case .failed(let message):
                    SectionRetryError(message: message) { await resolveThenRead() }
                case .loaded(let d):
                    if let repoName { KeyValueRow("Repository", repoName, mono: true) }
                    KeyValueRow("Revision", d["revision"].text ?? revision, mono: true)
                    KeyValueRow("Commit", d["commit"].text ?? "—", mono: true)
                }
            } header: {
                Text("Revision")
            } footer: {
                Text("A branch, tag or commit.")
            }
            Section {
                HStack(spacing: 8) {
                    TextField("README.md", text: $path)
                        .font(Theme.Fonts.idBody)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .keyboardType(.URL)
                        .submitLabel(.go)
                        .onSubmit { Task { await read() } }
                    Button("Open") { Task { await read() } }
                        .buttonStyle(RowButtonStyle(.primary))
                        .disabled(commit == nil)
                }
                ForEach(recent, id: \.self) { p in
                    Button {
                        path = p
                        Task { await read() }
                    } label: {
                        ListRow(title: p, monoTitle: true) {
                            Image(systemName: "doc.text").foregroundStyle(Theme.muted)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            } header: {
                Text("Files")
            } footer: {
                Text("A path from the repository root.")
            }
            if let file {
                fileSection(file)
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await resolveThenRead() }
        .task {
            // Once per appearance of the tab's stack, not on every tab switch.
            guard !started else { return }
            started = true
            await resolveThenRead()
            if let found = try? await InstanceRecordLoader.load(api, id: id) {
                record = found
            }
        }
    }

    @ViewBuilder
    private func fileSection(_ state: Loadable<JSON>) -> some View {
        switch state {
        case .loading:
            Section { LoadingRows(count: 4) }
        case .failed(let message):
            Section { SectionRetryError(message: message) { await read() } }
        case .loaded(let d):
            let name = d["path"].string ?? path
            let meta = "\(ConsoleFormat.bytes(d["size"])) at \(ConsoleFormat.short(d["commit"].string ?? commit, 12))"
            Section {
                if d["encoding"].string != "utf-8" {
                    MutedNoteRow("A binary file (\(ConsoleFormat.count(d["size"])) bytes). It is not shown here.")
                } else {
                    let content = d["content"].string ?? ""
                    let cut = content.count > Self.shownLimit
                    Text(cut ? String(content.prefix(Self.shownLimit)) : content)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(Theme.ink2)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .accessibilityLabel("Contents of \(name)")
                    if cut {
                        MutedNoteRow("Cut at \(Self.shownLimit) characters. The web console shows the whole file.")
                    }
                }
            } header: {
                Text(name).font(.system(.footnote, design: .monospaced)).textCase(nil)
            } footer: {
                Text(meta)
            }
        }
    }

    private func resolveThenRead() async {
        await resolve()
        if commit != nil, !path.trimmingCharacters(in: .whitespaces).isEmpty { await read() }
    }

    private func resolve() async {
        let asked = revision.trimmingCharacters(in: .whitespacesAndNewlines)
        let rev = asked.isEmpty ? "HEAD" : asked
        resolved = .loading
        do {
            let d = try await api.op(
                "source_resolve_revision", args: ["instance": .string(id), "revision": .string(rev)])
            resolved = .loaded(d)
        } catch is CancellationError {
        } catch {
            resolved = .failed(error.localizedDescription)
        }
    }

    private func read() async {
        var p = path.trimmingCharacters(in: .whitespacesAndNewlines)
        while p.hasPrefix("/") { p.removeFirst() }
        guard !p.isEmpty else { return }
        guard let at = commit else {
            file = .failed("Resolve a revision first.")
            return
        }
        file = .loading
        do {
            let d = try await api.op(
                "source_read_file",
                args: ["instance": .string(id), "revision": .string(at), "path": .string(p)])
            file = .loaded(d)
            recent.removeAll { $0 == p }
            recent.insert(p, at: 0)
            if recent.count > 6 { recent.removeLast(recent.count - 6) }
        } catch is CancellationError {
        } catch {
            file = .failed(error.localizedDescription)
        }
    }
}
