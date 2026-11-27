import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// The formats of Email › Mailboxes, as the web's `CHECKS`, `person` and `addresses`
/// (`views/data.js`).
private enum MailFormat {
    /// A message's sender check as a pill.
    static func senderPill(_ check: String?) -> StatusPill {
        switch check {
        case "pass": return StatusPill("Sender verified", tone: .ok)
        case "partial": return StatusPill("Partly verified", tone: .warn)
        case "fail": return StatusPill("Sender check failed", tone: .bad)
        default: return StatusPill("Not verified", tone: .neutral)
        }
    }

    /// `s` when it has text, else nil.
    static func nonEmpty(_ s: String?) -> String? {
        guard let s, !s.isEmpty else { return nil }
        return s
    }

    /// A sender's name, else their address.
    static func person(_ a: JSON) -> String? {
        nonEmpty(a["name"].string) ?? nonEmpty(a["address"].string)
    }

    /// The names of a list of addresses (each address when it has no name), or "—".
    static func names(_ list: JSON) -> String {
        let all = list.items.compactMap { person($0) }
        return all.isEmpty ? "—" : all.joined(separator: ", ")
    }

    /// The addresses of a list of addresses, or "—".
    static func addresses(_ list: JSON) -> String {
        let all = list.items.compactMap { nonEmpty($0["address"].string) }
        return all.isEmpty ? "—" : all.joined(separator: ", ")
    }

    /// A message's subject, or "(no subject)".
    static func subject(_ m: JSON) -> String { nonEmpty(m["subject"].string) ?? "(no subject)" }
}

/// A mailbox in the picker: its name and its count.
private struct MailboxOption: Hashable {
    let name: String
    let count: String
}

/// Email › Mailboxes: everything sent to the instance's domain (`email_list`, a page at a time),
/// all of it or one mailbox (`email_mailboxes`); a message opens it (`email_get`). Read-only.
struct EmailScreen: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var mailboxes = OpResource<JSON>("email_mailboxes")
    /// The picked mailbox; empty for all mail.
    @State private var mailbox = ""
    /// The mailbox `rows` were read for.
    @State private var shown: String?
    @State private var rows: [JSON] = []
    @State private var next: String?
    @State private var phase: Loadable<Bool> = .loading
    @State private var loadingMore = false
    @State private var moreError: String?

    private var id: String { context.instance.id }

    private var options: [MailboxOption] {
        (mailboxes.value?["mailboxes"].items ?? []).compactMap { m in
            guard let name = MailFormat.nonEmpty(m["mailbox"].string) else { return nil }
            return MailboxOption(name: name, count: ConsoleFormat.count(m["count"]))
        }
    }

    /// The same page on the web console, as `AreaPlaceholder` builds it.
    private var webURL: URL? {
        var segment = CharacterSet.urlPathAllowed
        segment.remove("/")
        let ids = [context.instance.id, context.area.id] + (context.view.map { [$0.id] } ?? [])
        let path = ids.map { $0.addingPercentEncoding(withAllowedCharacters: segment) ?? $0 }
        var parts = URLComponents(url: OAuth.base, resolvingAgainstBaseURL: false)
        parts?.percentEncodedPath = "/app/" + path.joined(separator: "/")
        return parts?.url
    }

    var body: some View {
        List {
            ViewPickerSection(context: context)
            UpgradeSection(context: context)
            Section {
                Picker("Mailbox", selection: $mailbox) {
                    Text("All mail").tag("")
                    ForEach(options, id: \.self) { o in
                        Text("\(o.name) · \(o.count)").tag(o.name)
                    }
                }
                .pickerStyle(.menu)
                .font(Theme.Fonts.body)
            } footer: {
                Text("Read-only. Everything sent to this instance’s domain is kept here; listed addresses have mailboxes of their own.")
            }
            switch phase {
            case .loading:
                Section { LoadingRows(count: 5) }
            case .failed(let message):
                Section { ErrorState(message: message) { await reload() } }
            case .loaded:
                if rows.isEmpty {
                    Section {
                        EmptyState(title: "No mail here yet", systemImage: "envelope")
                    }
                } else {
                    Section {
                        ForEach(rows, id: \.self) { m in
                            NavigationLink {
                                EmailMessageView(instance: id, row: m, webURL: webURL)
                            } label: {
                                messageRow(m)
                            }
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
        .refreshable {
            await mailboxes.load(api, args: ["instance": .string(id)])
            await reload()
        }
        .task { await mailboxes.load(api, args: ["instance": .string(id)]) }
        .task(id: mailbox) { await reload() }
    }

    private func messageRow(_ m: JSON) -> some View {
        let date = ConsoleFormat.ms(m["date"]) ?? ConsoleFormat.ms(m["seen_at"])
        let meta = [
            MailFormat.person(m["from"]),
            MailFormat.nonEmpty(m["mailbox"].string),
            ConsoleFormat.when(date),
        ].compactMap { $0 }
        let attachments = m["attachments"].int ?? 0
        return VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Text(MailFormat.subject(m))
                    .font(Theme.Fonts.body)
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                Spacer(minLength: 8)
                MailFormat.senderPill(m["sender_check"].string)
            }
            HStack(spacing: 4) {
                Text(meta.joined(separator: " · "))
                    .font(Theme.Fonts.meta)
                    .foregroundStyle(Theme.muted)
                    .lineLimit(1)
                if attachments > 0 {
                    Image(systemName: "paperclip")
                        .font(Theme.Fonts.meta)
                        .foregroundStyle(Theme.muted)
                        .accessibilityLabel(attachments == 1 ? "1 attachment" : "\(attachments) attachments")
                }
            }
        }
        .padding(.vertical, 2)
    }

    private func args(cursor: String?) -> JSON {
        var a: JSON = ["instance": .string(id)]
        if !mailbox.isEmpty { a = RecordDiff.setting(a, "mailbox", .string(mailbox)) }
        if let cursor { a = RecordDiff.setting(a, "cursor", .string(cursor)) }
        return a
    }

    /// Reads the first page of the picked mailbox, showing the skeleton when it changed.
    private func reload() async {
        let asked = mailbox
        if shown != asked || rows.isEmpty {
            rows = []
            next = nil
            phase = .loading
        }
        moreError = nil
        do {
            let data = try await api.op("email_list", args: args(cursor: nil))
            guard asked == mailbox else { return }
            rows = data["messages"].items
            next = MailFormat.nonEmpty(data["next"].string)
            shown = asked
            phase = .loaded(true)
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            guard asked == mailbox else { return }
            rows = []
            next = nil
            shown = nil
            phase = .failed(error.localizedDescription)
        }
    }

    private func loadMore() async {
        guard let cursor = next, !loadingMore else { return }
        loadingMore = true
        moreError = nil
        defer { loadingMore = false }
        let asked = mailbox
        do {
            let data = try await api.op("email_list", args: args(cursor: cursor))
            guard asked == mailbox else { return }
            let have = Set(rows.compactMap { $0["id"].string })
            rows += data["messages"].items.filter { m in
                guard let id = m["id"].string else { return true }
                return !have.contains(id)
            }
            next = MailFormat.nonEmpty(data["next"].string)
        } catch is CancellationError {
        } catch {
            moreError = error.localizedDescription
        }
    }
}

/// One message (`email_get`): its sender, the plain text (never the sender's HTML), its
/// attachments, the original and its headers. Files are saved through `storage_get_object` on
/// the data store and handed to the share sheet; nothing is opened here.
private struct EmailMessageView: View {
    let instance: String
    let row: JSON
    /// The page on the web console, for a message that has only an HTML version.
    let webURL: URL?
    @Environment(ConsoleAPI.self) private var api
    @Environment(ToastCenter.self) private var toasts
    @State private var message = OpResource<JSON>("email_get")
    /// The key of the file being read, while one is.
    @State private var saving: String?
    @State private var shared: SharedFile?

    /// Text longer than this is cut, so a long message does not stall the list.
    private static let shownLimit = 60_000

    var body: some View {
        List {
            switch message.state {
            case .loading:
                Section { LoadingRows(count: 6) }
            case .failed(let text):
                Section { ErrorState(message: text) { await load() } }
            case .loaded(let m):
                details(m)
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(MailFormat.subject(message.value ?? row))
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await load() }
        .task { await load() }
        .sheet(item: $shared) { file in
            ActivitySheet(items: [file.url])
                .presentationDetents([.medium, .large])
                .ignoresSafeArea()
        }
    }

    private func load() async {
        await message.load(api, args: ["instance": .string(instance), "id": row["id"]])
    }

    @ViewBuilder
    private func details(_ m: JSON) -> some View {
        let check = m["sightings"][0]["sender_check"].string ?? row["sender_check"].string
        Section {
            KeyValueRow("From", MailFormat.names(m["from"]))
            KeyValueRow("Address", MailFormat.addresses(m["from"]), mono: true)
            KeyValueRow("To", MailFormat.addresses(m["to"]), mono: true)
            if !m["cc"].items.isEmpty {
                KeyValueRow("Cc", MailFormat.addresses(m["cc"]), mono: true)
            }
            KeyValueRow("Date", ConsoleFormat.when(ConsoleFormat.ms(m["date"]) ?? ConsoleFormat.ms(m["first_seen"])))
            ListRow(title: "Sender") { MailFormat.senderPill(check) }
            if m["status"].string == "failed" {
                ListRow(title: "Parsed") { StatusPill("Failed: kept as received", tone: .warn) }
            }
        }
        Section("Message") {
            if let text = MailFormat.nonEmpty(m["text"].string) {
                Text(text.count > Self.shownLimit ? String(text.prefix(Self.shownLimit)) : text)
                    .font(Theme.Fonts.body)
                    .foregroundStyle(Theme.ink)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if text.count > Self.shownLimit {
                    MutedNoteRow("Showing the first \(Self.shownLimit) characters.")
                }
                MutedNoteRow("Shown sandboxed; remote images are blocked.")
            } else if MailFormat.nonEmpty(m["html"]["body"].string) != nil {
                MutedNoteRow("This message has only an HTML version. The web console shows it, sandboxed, with remote images blocked.")
                if let webURL {
                    Link("Open in the web console", destination: webURL)
                        .font(Theme.Fonts.body)
                }
            } else {
                MutedNoteRow("This message has no text.")
            }
        }
        let files = m["attachments"].items
        if !files.isEmpty {
            Section("Attachments") {
                ForEach(files, id: \.self) { a in
                    let name = MailFormat.nonEmpty(a["filename"].string)
                    ListRow(
                        title: name ?? ConsoleFormat.short(a["id"].string, 12),
                        subtitle: "\(MailFormat.nonEmpty(a["media_type"].string) ?? "File") · \(ConsoleFormat.bytes(a["size"]))"
                    ) {
                        if let key = a["key"].string {
                            saveButton(key: key, name: name ?? a["id"].string ?? "attachment", type: a["media_type"].string)
                        }
                    }
                }
            }
        }
        Section {
            if let key = m["key"].string {
                Button {
                    Task { await save(key: key, name: "\(m["id"].string ?? "message").eml", type: "message/rfc822") }
                } label: {
                    HStack {
                        Label("Download original (.eml)", systemImage: "square.and.arrow.down")
                            .font(Theme.Fonts.body)
                        Spacer()
                        if saving == key { ProgressView() }
                    }
                }
                .disabled(saving != nil)
            }
            DisclosureGroup("All headers") {
                Text(Self.headers(m))
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(Theme.ink2)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .font(Theme.Fonts.body)
        }
    }

    private func saveButton(key: String, name: String, type: String?) -> some View {
        Button {
            Task { await save(key: key, name: name, type: type) }
        } label: {
            if saving == key {
                ProgressView().controlSize(.small)
            } else {
                Text("Save")
            }
        }
        .buttonStyle(RowButtonStyle(.quiet))
        .disabled(saving != nil)
        .accessibilityLabel("Save \(name)")
    }

    /// Every header, one per line, decoded where the platform decoded it.
    private static func headers(_ m: JSON) -> String {
        let lines = m["headers"].items.map { h in
            "\(h["name"].string ?? ""): \(h["decoded"].string ?? h["value"].string ?? "")"
        }
        return lines.isEmpty ? "—" : lines.joined(separator: "\n")
    }

    /// Reads the object at `key` from the data store and offers it to the share sheet.
    private func save(key: String, name: String, type: String?) async {
        guard saving == nil else { return }
        saving = key
        defer { saving = nil }
        do {
            let object = try await api.op(
                "storage_get_object",
                args: ["instance": .string(instance), "key": .string(key), "source": "data"])
            shared = try SharedFile.write(object, name: name, type: type)
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            toasts.show("Couldn’t save \(name): \(error.localizedDescription)", tone: .bad)
        }
    }
}

/// A file read through `storage_get_object`, written to a temporary directory for the share
/// sheet.
private struct SharedFile: Identifiable {
    let url: URL
    var id: URL { url }

    /// An answer whose body could not be decoded.
    struct BadBody: LocalizedError {
        var errorDescription: String? { "the platform answered a body this app cannot read" }
    }

    /// Writes `object`'s body (`base64` or text) under `name`, with the extension of its media
    /// type when `name` has none.
    static func write(_ object: JSON, name: String, type: String?) throws -> SharedFile {
        let body = object["body"].string ?? ""
        let data: Data
        if object["encoding"].string == "base64" {
            guard let d = Data(base64Encoded: body, options: .ignoreUnknownCharacters) else { throw BadBody() }
            data = d
        } else {
            data = Data(body.utf8)
        }
        var file = name.replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "\\", with: "_")
        if file.isEmpty || file == "." || file == ".." { file = "file" }
        let mime = MailFormat.nonEmpty(object["content_type"].string) ?? type
        if (file as NSString).pathExtension.isEmpty, let mime,
            let ext = UTType(mimeType: mime)?.preferredFilenameExtension
        {
            file += ".\(ext)"
        }
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent(file, isDirectory: false)
        try data.write(to: url, options: .atomic)
        return SharedFile(url: url)
    }
}

/// The system share sheet for `items`, where the person saves a file to Files or sends it on.
private struct ActivitySheet: UIViewControllerRepresentable {
    let items: [Any]

    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }

    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}
