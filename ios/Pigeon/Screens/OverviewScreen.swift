import SwiftUI

/// What Overview reads: the record (`instances_list`) and, for a linked instance, its link
/// (`app_linked_details`; a failure there shows as the link's error).
struct OverviewData {
    let record: JSON?
    let details: JSON
}

/// Overview (C2): the record's facts, the Cloudflare link (Unlink through `linked-unlink`), and
/// an editor for the record's fields that are not capabilities (title, zones, repository,
/// Worker), applied through `instance-put`. The web's JSON editor (sites) stays on the web.
struct OverviewScreen: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @Environment(ManifestStore.self) private var store
    @Environment(\.openArea) private var openArea
    @State private var state: Loadable<OverviewData> = .loading
    @State private var request: WriteRequest?
    @State private var editing: RecordEdit?

    private var instance: ConsoleManifest.Instance { context.instance }

    var body: some View {
        List {
            ViewPickerSection(context: context)
            UpgradeSection(context: context)
            switch state {
            case .loading:
                Section { LoadingRows(count: 6) }
            case .failed(let message):
                Section { ErrorState(title: "Couldn't load the record", message: message) { await load() } }
            case .loaded(let data):
                if let record = data.record {
                    facts(record)
                    link(data.details)
                    Section {
                        Button {
                            editing = RecordEdit(record: record)
                        } label: {
                            ListRow(title: "Edit title, zones, repository, Worker", subtitle: "One change, approved with your passkey") {
                                Image(systemName: "chevron.right")
                                    .font(.footnote.weight(.semibold))
                                    .foregroundStyle(Theme.lineStrong)
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                    } header: {
                        Text("Record")
                    } footer: {
                        Text("Capabilities change in Capabilities. Sites (\(record["sites"].items.count)) are edited as JSON in the web console.")
                    }
                } else {
                    Section {
                        EmptyState(
                            title: "No record for \(instance.title)", systemImage: "questionmark.folder",
                            message: "The instance list doesn’t include it. It may have been retired.")
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
        .confirmSheet($request)
        .sheet(item: $editing) { edit in
            RecordEditSheet(record: edit.record) {
                Task { await reloadAll() }
            }
        }
    }

    private func load() async {
        do {
            async let record = InstanceRecordLoader.load(api, id: instance.id)
            async let details = linkDetails()
            let r = try await record
            let d = await details
            state = .loaded(OverviewData(record: r, details: d))
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            if state.value == nil { state = .failed(error.localizedDescription) }
        }
    }

    private func linkDetails() async -> JSON {
        if instance.home { return .null }
        do {
            return try await api.op("app_linked_details", args: ["instance": .string(instance.id)])
        } catch {
            return ["link": ["status": "unknown", "error": .string("Couldn’t read the link: \(error.localizedDescription)")]]
        }
    }

    private func reloadAll() async {
        await load()
        await store.refresh(api)
    }

    // MARK: Sections

    @ViewBuilder
    private func facts(_ r: JSON) -> some View {
        let enabled = CapabilityCatalog.all.filter { CapabilityCatalog.isOn(r, $0) }
        let zones = r["zones"].items.compactMap(\.string)
        let sections = (store.manifest?.areas ?? [])
            .filter { $0.group == "instance" && $0.id != "overview" && $0.id != "capabilities" }
            .map(\.title)
        Section("Instance") {
            KeyValueRow("Id", r["id"].text ?? instance.id, mono: true)
            KeyValueRow("Account", instance.home ? "Home account" : "Linked account")
            KeyValueRow("Host", instance.host ?? "—", mono: true)
            KeyValueRow("Zones", zones.isEmpty ? "none" : zones.joined(separator: ", "), mono: !zones.isEmpty)
            KeyValueRow("Repository", RecordEditSheet.repoText(r["repo"]).isEmpty ? "none" : RecordEditSheet.repoText(r["repo"]), mono: !RecordDiff.isNone(r["repo"]))
            KeyValueRow("Worker", r["worker"].string ?? "none (no sites yet)", mono: r["worker"].string != nil)
            KeyValueRow("Record", "v\(r["version"].int ?? 0)", mono: true)
            Button {
                openArea("capabilities")
            } label: {
                ListRow(
                    title: "Capabilities",
                    subtitle: enabled.isEmpty ? "none · Enable one" : enabled.map(\.cap).joined(separator: ", ")
                ) {
                    Image(systemName: "chevron.right")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(Theme.lineStrong)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        }
        Section("Areas") {
            ListRow(
                title: sections.isEmpty ? "Only Overview and Capabilities" : sections.joined(separator: " · "),
                subtitle: "From console_manifest")
        }
    }

    @ViewBuilder
    private func link(_ d: JSON) -> some View {
        if instance.home {
            Section("Cloudflare link") {
                MutedNoteRow("The home account runs the platform itself. It needs no link.")
            }
        } else {
            let l = d["link"]
            let status = l["status"].string ?? "unknown"
            let tone: Tone = status == "linked" ? .ok : status == "not linked" ? .warn : .bad
            let linked = status != "not linked" && l["error"].isNull
            Section {
                ListRow(title: "Status") { StatusPill(status, tone: tone) }
                if let name = l["account_name"].string { KeyValueRow("Account", name) }
                if let id = l["account_id"].string { KeyValueRow("Account id", id, mono: true) }
                if let ms = ConsoleFormat.ms(l["linked_ms"]) { KeyValueRow("Linked", ConsoleFormat.when(ms)) }
                if let ms = ConsoleFormat.ms(l["access_expires_ms"]) {
                    KeyValueRow("Access expires", ConsoleFormat.when(ms))
                }
                if let scope = l["scope"].text { KeyValueRow("Scopes", scope, mono: true) }
                if let worker = d["worker"].string { KeyValueRow("Worker", worker, mono: true) }
                if status == "refresh failed" {
                    Text("The platform couldn’t refresh this account’s access. Link again in the web console to restore it.")
                        .font(Theme.Fonts.meta).foregroundStyle(Theme.bad)
                }
                if let error = l["error"].string {
                    Text(error).font(Theme.Fonts.meta).foregroundStyle(Theme.bad)
                }
                if linked {
                    Button("Unlink…") { unlink(l) }
                        .buttonStyle(RowButtonStyle(.danger))
                }
            } header: {
                Text("Cloudflare link")
            } footer: {
                Text("Linking opens Cloudflare’s consent screen, which is on the web console.")
            }
        }
    }

    private func unlink(_ l: JSON) {
        var facts = [Fact("Instance", instance.id, mono: true)]
        if let name = l["account_name"].string { facts.append(Fact("Account", name)) }
        if let id = l["account_id"].string { facts.append(Fact("Account id", id, mono: true)) }
        let what = "link: \(l["account_name"].string ?? "account") \(l["account_id"].string ?? "")"
            .trimmingCharacters(in: .whitespaces)
        request = WriteRequest(
            title: "Unlink \(instance.title)",
            subtitle: "The platform forgets this account’s access. Nothing in the Cloudflare account is deleted, and its sites keep serving; the platform can’t deploy to it or read it until you link again.",
            facts: facts,
            lines: [.remove(what)],
            action: "linked-unlink",
            target: .string(instance.id),
            destructive: true,
            confirmLabel: "Unlink with passkey",
            onDone: { _ in Task { await reloadAll() } })
    }
}

/// The record to edit in the record sheet.
struct RecordEdit: Identifiable {
    let record: JSON
    var id: String { record["id"].text ?? "record" }
}

/// The record's own fields (title, zones, repository, Worker) as a form, the diff as they are
/// typed, then the passkey through `instance-put`. Capabilities and pins stay as they are.
struct RecordEditSheet: View {
    let record: JSON
    let onDone: () -> Void
    @State private var title: String
    @State private var zones: String
    @State private var repo: String
    @State private var worker: String
    @State private var override: Bool

    init(record: JSON, onDone: @escaping () -> Void) {
        self.record = record
        self.onDone = onDone
        _title = State(initialValue: record["title"].string ?? "")
        _zones = State(initialValue: record["zones"].items.compactMap(\.string).joined(separator: ", "))
        _repo = State(initialValue: Self.repoText(record["repo"]))
        _worker = State(initialValue: record["worker"].string ?? "")
        _override = State(initialValue: record["worker_override"].bool ?? false)
    }

    /// A repository as the web writes it: `github:owner/name` or `artifacts:namespace/repo`.
    static func repoText(_ repo: JSON) -> String {
        if RecordDiff.isNone(repo) { return "" }
        switch repo["kind"].string {
        case "github": return "github:\(repo["full_name"].string ?? "")"
        case "artifacts": return "artifacts:\(repo["namespace"].string ?? "")/\(repo["repo"].string ?? "")"
        default: return repo.compactText
        }
    }

    private static func validPart(_ s: Substring) -> Bool {
        !s.isEmpty && s.allSatisfy { $0.isLetter || $0.isNumber || $0 == "_" || $0 == "." || $0 == "-" }
    }

    /// The repository for `text`, keeping the GitHub installation when the repository is the same.
    static func repoFrom(_ text: String, was: JSON) throws -> JSON {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if t.isEmpty { return .null }
        let problem = InputError("A repository is github:owner/name or artifacts:namespace/repo, or empty for none.")
        guard let colon = t.firstIndex(of: ":") else { throw problem }
        let kind = String(t[..<colon])
        let rest = String(t[t.index(after: colon)...])
        let parts = rest.split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count == 2 else { throw problem }
        if kind == "github", parts.allSatisfy(validPart) {
            let same = was["kind"].string == "github" && was["full_name"].string == rest
            return ["kind": "github", "full_name": .string(rest), "installation": same ? was["installation"] : .null]
        }
        if kind == "artifacts", parts.allSatisfy({ !$0.isEmpty }) {
            return ["kind": "artifacts", "namespace": .string(String(parts[0])), "repo": .string(String(parts[1]))]
        }
        throw problem
    }

    private func next() throws -> JSON {
        var o = record.object ?? [:]
        o["title"] = .string(title.trimmingCharacters(in: .whitespacesAndNewlines))
        o["zones"] = .array(ConsoleFormat.list(zones).map { JSON.string($0) })
        o["repo"] = try Self.repoFrom(repo, was: record["repo"])
        let w = worker.trimmingCharacters(in: .whitespacesAndNewlines)
        o["worker"] = w.isEmpty ? JSON.null : JSON.string(w)
        o["worker_override"] = .bool(override)
        return .object(o)
    }

    private var request: WriteRequest {
        let done = onDone
        return WriteRequest(
            title: "Save \(record["title"].string ?? record["id"].text ?? "the record")",
            subtitle: "Saves the record a version on. Capabilities and pins stay as they are.",
            facts: [Fact("Instance", record["id"].text ?? "—", mono: true)],
            action: "instance-put",
            target: .string(record["id"].text ?? ""),
            confirmLabel: "Save with passkey",
            onDone: { _ in done() })
    }

    var body: some View {
        ConfirmSheet(request, prepare: { try change() }) {
            VStack(alignment: .leading, spacing: 12) {
                field("Title", text: $title, mono: false, hint: nil)
                field("Zones", text: $zones, mono: true, hint: "Comma-separated DNS zones this instance may serve hostnames in.")
                field("Repository", text: $repo, mono: true, hint: "github:owner/name or artifacts:namespace/repo; empty for none.")
                field("Worker", text: $worker, mono: true, hint: "The one Worker that serves the instance’s sites; empty for the default.")
                Toggle("The Worker’s name may differ from the default", isOn: $override)
                    .font(Theme.Fonts.meta)
                VStack(alignment: .leading, spacing: 6) {
                    Text("Diff")
                        .font(Theme.Fonts.caption).textCase(.uppercase)
                        .foregroundStyle(Theme.muted)
                    preview
                }
            }
        }
    }

    @ViewBuilder
    private var preview: some View {
        let result = Result { try next() }
        switch result {
        case .success(let to):
            let list = RecordDiff.changes(record, to)
            if list.isEmpty {
                Text("No changes yet.").font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            } else {
                DiffBlock(lines: RecordDiff.lines(record, list))
            }
        case .failure(let error):
            Text(error.localizedDescription).font(Theme.Fonts.meta).foregroundStyle(Theme.bad)
        }
    }

    private func field(_ label: String, text: Binding<String>, mono: Bool, hint: String?) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label).font(Theme.Fonts.meta.weight(.semibold)).foregroundStyle(Theme.ink)
            TextField(label, text: text)
                .font(mono ? Theme.Fonts.idBody : Theme.Fonts.body)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .textFieldStyle(.roundedBorder)
            if let hint {
                Text(hint).font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            }
        }
    }

    private func change() throws -> JSON {
        let to = try next()
        if RecordDiff.changes(record, to).isEmpty { throw InputError("Nothing changes.") }
        return RecordDiff.putChange(to)
    }
}
