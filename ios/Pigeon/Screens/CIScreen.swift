import SwiftUI

/// The CI states the jobs filter offers, as the web's `CI_STATES`.
enum CIState {
    static let all = ["queued", "dispatched", "running", "succeeded", "failed", "cancelled", "timed_out", "error"]

    static func tone(_ s: String?) -> Tone {
        switch s {
        case "succeeded": return .ok
        case "failed", "error", "timed_out": return .bad
        case "running", "dispatched": return .info
        case "queued": return .warn
        default: return .neutral
        }
    }

    static func pill(_ s: String?) -> StatusPill {
        StatusPill(ConsoleFormat.words(s ?? "unknown"), tone: tone(s))
    }
}

/// CI › Jobs: the instance's jobs (its repository and approved job names only, through
/// `ci_jobs` bound to the instance), newest first, with a state filter; a job opens its status,
/// changes and output (`ci_status`). Read-only.
struct CIScreen: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var jobs = OpResource<JSON>("ci_jobs")
    @State private var filter = ""

    var body: some View {
        List {
            ViewPickerSection(context: context)
            UpgradeSection(context: context)
            Section {
                Picker("State", selection: $filter) {
                    Text("Every state").tag("")
                    ForEach(CIState.all, id: \.self) { s in
                        Text(ConsoleFormat.words(s).capitalized).tag(s)
                    }
                }
                .pickerStyle(.menu)
                .font(Theme.Fonts.body)
            } footer: {
                Text("Read-only. Jobs of this instance’s repository and approved job names, newest first.")
            }
            LoadableSection(
                state: jobs.state,
                emptyTitle: filter.isEmpty ? "No jobs yet" : "No \(ConsoleFormat.words(filter)) jobs",
                emptyMessage: filter.isEmpty ? "No jobs for this instance yet." : nil,
                isEmpty: { $0["jobs"].items.isEmpty },
                retry: { await load() }
            ) { data in
                Section {
                    ForEach(data["jobs"].items, id: \.self) { j in
                        NavigationLink {
                            CIJobView(instance: context.instance.id, job: j)
                        } label: {
                            jobRow(j)
                        }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task(id: filter) { await load() }
    }

    private func load() async {
        var args: JSON = ["instance": .string(context.instance.id), "limit": 50]
        if !filter.isEmpty { args = RecordDiff.setting(args, "state", .string(filter)) }
        await jobs.load(api, args: args)
    }

    private func jobRow(_ j: JSON) -> some View {
        let caller = j["caller_name"].string ?? j["caller"].string
        let parts = [
            j["task"].string,
            caller,
            "\(ConsoleFormat.agoPhrase(ConsoleFormat.ms(j["created_at"]))) · \(ConsoleFormat.duration(j["duration_ms"]))",
        ].compactMap { $0 }
        return VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Text(j["job"].text ?? "—").font(Theme.Fonts.idBody).foregroundStyle(Theme.ink)
                Text("·").foregroundStyle(Theme.muted)
                Text(ConsoleFormat.short(j["commit"].string, 9)).idStyle().foregroundStyle(Theme.ink2)
                Spacer(minLength: 8)
                CIState.pill(j["state"].string)
            }
            .lineLimit(1)
            Text(parts.joined(separator: " · "))
                .font(Theme.Fonts.meta)
                .foregroundStyle(Theme.muted)
                .lineLimit(2)
        }
        .padding(.vertical, 2)
    }
}

/// One CI job: its status, changes and output (`ci_status`).
struct CIJobView: View {
    let instance: String
    let job: JSON
    @Environment(ConsoleAPI.self) private var api
    @State private var status = OpResource<JSON>("ci_status")

    /// Output longer than this is cut, so a long log does not stall the list.
    private static let shownLimit = 60_000

    var body: some View {
        List {
            switch status.state {
            case .loading:
                Section { LoadingRows(count: 6) }
            case .failed(let message):
                Section { ErrorState(message: message) { await load() } }
            case .loaded(let j):
                details(j)
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(job["job"].text ?? "Job")
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await load() }
        .task { await load() }
    }

    private func load() async {
        await status.load(api, args: ["instance": .string(instance), "id": job["id"]])
    }

    @ViewBuilder
    private func details(_ j: JSON) -> some View {
        Section {
            KeyValueRow("Job", j["job"].text ?? "—", mono: true)
            KeyValueRow("Id", j["id"].text ?? "—", mono: true)
            ListRow(title: "State") { CIState.pill(j["state"].string) }
            KeyValueRow("Commit", j["commit"].text ?? "—", mono: true)
            if let task = j["task"].string { KeyValueRow("Task", task) }
            KeyValueRow("Attempt", j["attempt"].text ?? "—")
            if let url = j["url"].string, url.hasPrefix("https://"), let link = URL(string: url) {
                Link(destination: link) {
                    ListRow(title: "Connector") {
                        Text(j["connector"].string ?? "Open").font(Theme.Fonts.body).foregroundStyle(Theme.accent)
                    }
                }
            } else {
                KeyValueRow("Connector", j["connector"].text ?? "—")
            }
            if let image = j["image"].string { KeyValueRow("Image", image, mono: true) }
        }
        Section {
            KeyValueRow("Created", ConsoleFormat.when(ConsoleFormat.ms(j["created_at"])))
            if let ms = ConsoleFormat.ms(j["started_at"]) { KeyValueRow("Started", ConsoleFormat.when(ms)) }
            if let ms = ConsoleFormat.ms(j["finished_at"]) { KeyValueRow("Finished", ConsoleFormat.when(ms)) }
            KeyValueRow("Duration", ConsoleFormat.duration(j["duration_ms"]))
            if let code = j["exit_code"].text { KeyValueRow("Exit code", code, mono: true) }
            if let detail = j["detail"].string {
                Text(detail).font(Theme.Fonts.meta).foregroundStyle(Theme.ink2).textSelection(.enabled)
            }
            let excluded = j["excluded"].items.compactMap(\.string)
            if !excluded.isEmpty { KeyValueRow("Excluded connectors", excluded.joined(separator: ", ")) }
        }
        let events = j["events"].items
        if !events.isEmpty {
            Section("Changes") {
                ForEach(events, id: \.self) { e in
                    ListRow(title: ConsoleFormat.when(ConsoleFormat.ms(e["at"])), subtitle: e["detail"].string) {
                        CIState.pill(e["state"].string)
                    }
                }
            }
        }
        let output = Self.outputText(j["output"])
        if !output.isEmpty {
            Section("Output") {
                Text(output.count > Self.shownLimit ? String(output.suffix(Self.shownLimit)) : output)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(Theme.ink2)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if output.count > Self.shownLimit {
                    MutedNoteRow("Showing the last \(Self.shownLimit) characters.")
                }
            }
        }
    }

    private static func outputText(_ v: JSON) -> String {
        if v.isNull { return "" }
        if let s = v.string { return s }
        return v.compactText
    }
}
