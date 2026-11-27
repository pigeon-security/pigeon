import SwiftUI

/// One row of a storage listing: a prefix to open, or an object.
private struct StorageEntry: Identifiable, Hashable {
    let key: String
    let dir: Bool
    let object: JSON
    var id: String { (dir ? "d:" : "o:") + key }
}

/// Storage › Buckets: the instance's one approved location (store, bucket, prefix), browsed by
/// prefix a page at a time with `storage_list_objects` bound to the instance. Read-only.
struct StorageScreen: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var record: JSON = .null
    @State private var prefix = ""
    @State private var rows: [StorageEntry] = []
    @State private var next: String?
    @State private var phase: Loadable<Bool> = .loading
    @State private var loadingMore = false
    @State private var moreError: String?

    private var id: String { context.instance.id }
    /// The approved location, at the record's `data.read` (docs/plans/record-reshape.md).
    private var storage: JSON { record["data"]["read"] }

    var body: some View {
        List {
            ViewPickerSection(context: context)
            UpgradeSection(context: context)
            Section {
                if let store = storage["store"].string { KeyValueRow("Store", store, mono: true) }
                if let bucket = storage["bucket"].string { KeyValueRow("Bucket", bucket, mono: true) }
                if let p = storage["prefix"].string, !p.isEmpty { KeyValueRow("Prefix", p, mono: true) }
            } header: {
                Text("Approved location")
            } footer: {
                Text("Read-only. Only this instance’s approved bucket and prefix.")
            }
            Section { crumbs }
            switch phase {
            case .loading:
                Section { LoadingRows(count: 5) }
            case .failed(let message):
                Section { ErrorState(message: message) { await reload() } }
            case .loaded:
                if rows.isEmpty {
                    Section {
                        EmptyState(title: "Nothing here", systemImage: "externaldrive", message: "Nothing is stored under this prefix.")
                    }
                } else {
                    Section {
                        ForEach(rows) { entry in
                            row(entry)
                        }
                    }
                    if next != nil || moreError != nil {
                        Section {
                            if let moreError {
                                SectionRetryError(message: moreError) { await loadMore() }
                            } else {
                                Button {
                                    Task { await loadMore() }
                                } label: {
                                    HStack {
                                        Spacer()
                                        if loadingMore { ProgressView() } else { Text("Load more") }
                                        Spacer()
                                    }
                                }
                                .disabled(loadingMore)
                            }
                        }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await reload() }
        .task {
            if let r = try? await InstanceRecordLoader.load(api, id: id) { record = r }
        }
        .task(id: prefix) { await reload() }
    }

    /// The location's name, then each part of the prefix; each opens its prefix.
    private var crumbs: some View {
        let parts = prefix.split(separator: "/").map(String.init)
        let root = [storage["bucket"].string, storage["prefix"].string]
            .compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: "/")
        let rootName = root.hasSuffix("/") ? String(root.dropLast()) : root
        return ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                Button(rootName.isEmpty ? "Root" : rootName) { openPrefix("") }
                    .font(Theme.Fonts.id)
                    .disabled(parts.isEmpty)
                ForEach(Array(parts.enumerated()), id: \.offset) { i, part in
                    Text("/").font(Theme.Fonts.id).foregroundStyle(Theme.muted)
                    if i == parts.count - 1 {
                        Text(part).font(Theme.Fonts.id.weight(.semibold)).foregroundStyle(Theme.ink)
                    } else {
                        Button(part) { openPrefix(parts[0...i].joined(separator: "/") + "/") }
                            .font(Theme.Fonts.id)
                    }
                }
            }
            .buttonStyle(.borderless)
        }
        .accessibilityLabel("Prefix \(prefix.isEmpty ? "root" : prefix)")
    }

    /// Opens `to`, showing the skeleton until its first page arrives.
    private func openPrefix(_ to: String) {
        guard to != prefix else { return }
        rows = []
        next = nil
        moreError = nil
        phase = .loading
        prefix = to
    }

    private func name(_ key: String) -> String {
        let rest = key.hasPrefix(prefix) ? String(key.dropFirst(prefix.count)) : key
        return rest.isEmpty ? key : rest
    }

    @ViewBuilder
    private func row(_ entry: StorageEntry) -> some View {
        if entry.dir {
            Button {
                openPrefix(entry.key)
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: "folder").foregroundStyle(Theme.accent)
                    ListRow(title: name(entry.key), subtitle: "Folder", monoTitle: true) {
                        Image(systemName: "chevron.right")
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(Theme.lineStrong)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        } else {
            let o = entry.object
            let modified = ConsoleFormat.ms(o["last_modified"]).map { ConsoleFormat.when($0) }
            let etag = ConsoleFormat.short(o["etag"].string?.replacingOccurrences(of: "\"", with: ""), 10)
            let subtitle = [modified, etag.isEmpty ? nil : etag].compactMap { $0 }.joined(separator: " · ")
            ListRow(title: name(entry.key), subtitle: subtitle, monoTitle: true) {
                Text(ConsoleFormat.bytes(o["size"]))
                    .font(Theme.Fonts.meta.monospacedDigit())
                    .foregroundStyle(Theme.ink2)
            }
        }
    }

    private func args(token: String?) -> JSON {
        var a: JSON = ["instance": .string(id), "prefix": .string(prefix), "delimiter": "/", "max_keys": 100]
        if let token { a = RecordDiff.setting(a, "token", .string(token)) }
        return a
    }

    private static func entries(_ data: JSON) -> [StorageEntry] {
        let dirs = data["prefixes"].items.compactMap { p -> StorageEntry? in
            guard let key = p.string ?? p["prefix"].string else { return nil }
            return StorageEntry(key: key, dir: true, object: .null)
        }
        let objects = data["objects"].items.compactMap { o -> StorageEntry? in
            guard let key = o["key"].string else { return nil }
            return StorageEntry(key: key, dir: false, object: o)
        }
        return dirs + objects
    }

    private func reload() async {
        if rows.isEmpty { phase = .loading }
        moreError = nil
        let asked = prefix
        do {
            let data = try await api.op("storage_list_objects", args: args(token: nil))
            guard asked == prefix else { return }
            rows = Self.entries(data)
            next = data["next"].string
            phase = .loaded(true)
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            guard asked == prefix else { return }
            rows = []
            next = nil
            phase = .failed(error.localizedDescription)
        }
    }

    private func loadMore() async {
        guard let token = next, !loadingMore else { return }
        loadingMore = true
        moreError = nil
        defer { loadingMore = false }
        let asked = prefix
        do {
            let data = try await api.op("storage_list_objects", args: args(token: token))
            guard asked == prefix else { return }
            rows += Self.entries(data)
            next = data["next"].string
        } catch is CancellationError {
        } catch {
            moreError = error.localizedDescription
        }
    }
}
