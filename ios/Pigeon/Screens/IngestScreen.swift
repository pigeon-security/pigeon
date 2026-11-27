import SwiftUI

/// Ingest › Runs: the instance's runs (`app_ingest_runs`, which never deletes a VM, unlike
/// `data_ingest_status`), newest first; a run opens its report once it has finished
/// (`data_ingest_report`). Read-only: starting and stopping a run stay in MCP.
struct IngestScreen: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var runs = OpResource<JSON>("app_ingest_runs")

    static func tone(_ s: String?) -> Tone {
        switch s {
        case "done": return .ok
        case "failed": return .bad
        case "stopped": return .warn
        case "running", "listing", "starting": return .info
        default: return .neutral
        }
    }

    var body: some View {
        List {
            ViewPickerSection(context: context)
            UpgradeSection(context: context)
            Section {
                MutedNoteRow("Read-only. Runs of this instance, newest first. Starting and stopping a run stay in MCP.")
            }
            LoadableSection(
                state: runs.state,
                emptyTitle: "No ingest runs yet",
                isEmpty: { $0["runs"].items.isEmpty },
                retry: { await load() }
            ) { data in
                Section {
                    ForEach(Self.sorted(data), id: \.self) { r in
                        NavigationLink {
                            IngestRunView(instance: context.instance.id, run: r)
                        } label: {
                            runRow(r)
                        }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
    }

    private static func sorted(_ data: JSON) -> [JSON] {
        data["runs"].items.sorted {
            (ConsoleFormat.ms($0["started_ms"]) ?? 0) > (ConsoleFormat.ms($1["started_ms"]) ?? 0)
        }
    }

    private func load() async {
        await runs.load(api, args: ["instance": .string(context.instance.id)])
    }

    private func runRow(_ r: JSON) -> some View {
        let machine: String
        if r["host"].string == "cloudflare" {
            machine = "container"
        } else if r["vm_deleted"].bool == true {
            machine = "VM deleted"
        } else if !r["vm"].isNull {
            machine = "VM running"
        } else {
            machine = ""
        }
        let parts = [
            r["dry_run"].bool == true ? "dry run" : "copy",
            r["bucket"].string,
            [r["host"].string, r["region"].string].compactMap { $0 }.joined(separator: " "),
            machine,
            "started \(ConsoleFormat.agoPhrase(ConsoleFormat.ms(r["started_ms"])))",
        ].compactMap { $0 }.filter { !$0.isEmpty }
        return ListRow(title: r["run"].text ?? "—", subtitle: parts.joined(separator: " · "), monoTitle: true) {
            if let state = r["state"].string {
                StatusPill(ConsoleFormat.words(state), tone: Self.tone(state))
            } else if let commit = r["commit"].string {
                Text(ConsoleFormat.short(commit, 9)).idStyle().foregroundStyle(Theme.muted)
            }
        }
    }
}

/// One ingest run and its report (`data_ingest_report`).
struct IngestRunView: View {
    let instance: String
    let run: JSON
    @Environment(ConsoleAPI.self) private var api
    @State private var report = OpResource<JSON>("data_ingest_report")

    var body: some View {
        List {
            Section {
                KeyValueRow("Run", run["run"].text ?? "—", mono: true)
                if let commit = run["commit"].string { KeyValueRow("Commit", ConsoleFormat.short(commit, 12), mono: true) }
                KeyValueRow("Kind", run["dry_run"].bool == true ? "Dry run" : "Copy")
                if let bucket = run["bucket"].string { KeyValueRow("Source", bucket, mono: true) }
                if let host = run["host"].string { KeyValueRow("Host", [host, run["region"].string].compactMap { $0 }.joined(separator: " · ")) }
                KeyValueRow("Started", ConsoleFormat.when(ConsoleFormat.ms(run["started_ms"])))
            }
            switch report.state {
            case .loading:
                Section { LoadingRows(count: 4) }
            case .failed(let message):
                Section { ErrorState(message: message) { await load() } }
            case .loaded(let r):
                reportSections(r)
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(run["run"].text ?? "Run")
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await load() }
        .task { await load() }
    }

    private func load() async {
        await report.load(api, args: ["instance": .string(instance), "run": run["run"]])
    }

    private static func pair(_ n: JSON, _ b: JSON) -> String {
        "\(ConsoleFormat.count(n)) · \(ConsoleFormat.bytes(b))"
    }

    @ViewBuilder
    private func reportSections(_ r: JSON) -> some View {
        let c = r["counters"]
        let noReport = r.isNull || r.object?["report"] == JSON.null || c.isNull
        if noReport {
            Section {
                MutedNoteRow(r["note"].string ?? "There is no report yet: the run has not finished.")
            }
        } else {
            let dry = r["dry_run"].bool == true
            Section {
                ListRow(title: "State") {
                    HStack(spacing: 6) {
                        if dry { TagLabel("Dry run") }
                        StatusPill(ConsoleFormat.words(r["state"].string ?? "unknown"), tone: IngestScreen.tone(r["state"].string))
                    }
                }
                KeyValueRow("Started", ConsoleFormat.when(ConsoleFormat.ms(r["started_ms"])))
                KeyValueRow("Finished", ConsoleFormat.when(ConsoleFormat.ms(r["finished_ms"])))
            }
            Section("Counters") {
                KeyValueRow("Listed", Self.pair(c["listed"], c["listed_bytes"]))
                KeyValueRow("Read", ConsoleFormat.bytes(c["bytes_read"]))
                if dry {
                    KeyValueRow("Would copy", Self.pair(c["would_upload"], c["would_upload_bytes"]))
                } else {
                    KeyValueRow("Copied", Self.pair(c["uploaded"], c["uploaded_bytes"]))
                }
                KeyValueRow("Already in R2", Self.pair(c["duplicates"], c["duplicate_bytes"]))
                ListRow(title: "Conflicts") { countPill(c["conflicts"], tone: .warn) }
                ListRow(title: "Failed") { countPill(c["failed"], tone: .bad) }
                KeyValueRow("Skipped", ConsoleFormat.count(c["skipped"]))
                KeyValueRow("Archives", "\(ConsoleFormat.count(c["archives_opened"])) opened, \(ConsoleFormat.count(c["archives_not_opened"])) not")
            }
            let flags = r["flags"].items
            if !flags.isEmpty {
                Section("Flags") {
                    ForEach(flags, id: \.self) { f in
                        KeyValueRow(ConsoleFormat.words(f[0].text ?? "flag"), ConsoleFormat.count(f[1]))
                    }
                }
            }
            lines(r["failures"].items, "Failures")
            lines(r["conflicts"].items, "Conflicts")
            lines(r["not_opened"].items, "Not opened")
            let truncated = (r["truncated_from"].object ?? [:]).keys.sorted().map { k in
                "\(ConsoleFormat.words(k)) (\(ConsoleFormat.count(r["truncated_from"][k])))"
            }
            if !truncated.isEmpty {
                Section { MutedNoteRow("Lists cut short; in full: \(truncated.joined(separator: ", ")).") }
            }
        }
    }

    @ViewBuilder
    private func countPill(_ n: JSON, tone: Tone) -> some View {
        if (n.double ?? 0) > 0 {
            StatusPill(ConsoleFormat.count(n), tone: tone)
        } else {
            Text("0").font(Theme.Fonts.body.monospacedDigit()).foregroundStyle(Theme.ink2)
        }
    }

    @ViewBuilder
    private func lines(_ rows: [JSON], _ label: String) -> some View {
        if !rows.isEmpty {
            Section(label) {
                ForEach(rows, id: \.self) { l in
                    let key = l["origin"]["key"].string ?? l["norm_name"].string ?? l["id"].text ?? "—"
                    let why = l["error"].string ?? ConsoleFormat.words(l["reason"].text ?? "—")
                    let member = l["member"]["path"].string
                    ListRow(
                        title: key,
                        subtitle: [member, why].compactMap { $0 }.joined(separator: " · "),
                        monoTitle: true
                    ) {
                        Text(ConsoleFormat.bytes(l["size"])).font(Theme.Fonts.meta.monospacedDigit()).foregroundStyle(Theme.ink2)
                    }
                }
            }
        }
    }
}
