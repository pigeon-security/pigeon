import SwiftUI

/// The formats of Jobs › Runs, as the web's `estimatedUsd` and `auditStatus` (`views/data.js`).
private enum JobFormat {
    /// An estimated cost: "<0.01 USD", "0.02 USD"; "—" when unknown.
    static func estimatedUsd(_ value: JSON) -> String {
        guard let amount = value.double, amount.isFinite else { return "—" }
        if amount > 0 && amount < 0.01 { return "<0.01 USD" }
        return String(format: "%.2f USD", amount)
    }

    /// The audit export's state in words.
    static func auditStatus(_ s: String) -> String {
        switch s {
        case "not_configured": return "No audit bucket"
        case "retrying": return "Delivery failed · retry scheduled"
        case "pending": return "Pending"
        case "stored": return "Stored in R2"
        case "verified": return "Verified in R2"
        default: return s
        }
    }

    static func auditTone(_ s: String) -> Tone {
        switch s {
        case "stored", "verified": return .ok
        case "retrying": return .warn
        case "pending": return .info
        default: return .neutral
        }
    }

    static func stateTone(_ s: String?) -> Tone {
        switch s {
        case "waiting", "creating", "cleanup": return .info
        case "done": return .ok
        case "failed", "rejected": return .bad
        default: return .neutral
        }
    }

    /// A job's state as a pill.
    static func statePill(_ s: String?) -> StatusPill {
        StatusPill(ConsoleFormat.words(s ?? "unknown"), tone: stateTone(s))
    }

    static func outcomeTone(_ s: String?) -> Tone {
        switch s {
        case "succeeded", "confirmed", "ok": return .ok
        case "failed", "timed_out": return .bad
        case "retrying", "uncertain": return .warn
        case "requested", "started": return .info
        default: return .neutral
        }
    }

    /// A job's id as the list shows it.
    static func shortID(_ job: JSON) -> String {
        let id = ConsoleFormat.short(job["id"].string, 12)
        return id.isEmpty ? "—" : id
    }
}

/// Jobs › Runs: the instance's runner checks (`digitalocean_droplet_jobs`), newest first; a run
/// opens its history (`digitalocean_droplet_job`). The history is durable, so it stays when the
/// provider's connection is gone. Read-only.
struct JobsScreen: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var jobs = OpResource<JSON>("digitalocean_droplet_jobs")

    var body: some View {
        List {
            ViewPickerSection(context: context)
            UpgradeSection(context: context)
            LoadableSection(
                state: jobs.state,
                emptyTitle: "No runs yet",
                emptyMessage: "No runs have been recorded for this instance.",
                isEmpty: { $0["jobs"].items.isEmpty },
                retry: { await load() }
            ) { data in
                Section {
                    ForEach(Self.sorted(data), id: \.self) { j in
                        NavigationLink {
                            JobRunView(instance: context.instance.id, job: j)
                        } label: {
                            jobRow(j)
                        }
                    }
                } footer: {
                    Text("Read-only. Runner checks on this instance, newest first. The history stays when the provider’s connection is gone.")
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
    }

    private static func sorted(_ data: JSON) -> [JSON] {
        data["jobs"].items.sorted {
            (ConsoleFormat.ms($0["started_ms"]) ?? 0) > (ConsoleFormat.ms($1["started_ms"]) ?? 0)
        }
    }

    private func load() async {
        await jobs.load(api, args: ["instance": .string(context.instance.id)])
    }

    private func jobRow(_ j: JSON) -> some View {
        let parts = [
            j["stage"].string.map { ConsoleFormat.words($0) },
            j["provider_state"].string.map { "provider \(ConsoleFormat.words($0))" },
            JobFormat.estimatedUsd(j["estimated_usd"]),
            ConsoleFormat.agoPhrase(ConsoleFormat.ms(j["started_ms"])),
            j["audit_status"].string.map { JobFormat.auditStatus($0) },
        ].compactMap { $0 }
        return VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Text(JobFormat.shortID(j)).font(Theme.Fonts.idBody).foregroundStyle(Theme.ink)
                Text("·").foregroundStyle(Theme.muted)
                Text("DigitalOcean").font(Theme.Fonts.body).foregroundStyle(Theme.ink2)
                Spacer(minLength: 8)
                JobFormat.statePill(j["state"].string)
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

/// One run: its job, provider and cost, and the runner's stages (`digitalocean_droplet_job`).
private struct JobRunView: View {
    let instance: String
    let job: JSON
    @Environment(ConsoleAPI.self) private var api
    @State private var history = OpResource<JSON>("digitalocean_droplet_job")

    var body: some View {
        List {
            switch history.state {
            case .loading:
                Section { LoadingRows(count: 6) }
            case .failed(let message):
                Section { ErrorState(message: message) { await load() } }
            case .loaded(let data):
                details(data["job"])
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(JobFormat.shortID(job))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                Text(JobFormat.shortID(job))
                    .font(Theme.Fonts.idBody.weight(.semibold))
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
            }
        }
        .refreshable { await load() }
        .task { await load() }
    }

    private func load() async {
        await history.load(api, args: ["instance": .string(instance), "id": job["id"]])
    }

    @ViewBuilder
    private func details(_ j: JSON) -> some View {
        Section {
            KeyValueRow("Job", j["id"].text ?? "—", mono: true)
            ListRow(title: "Job state") { JobFormat.statePill(j["state"].string) }
            KeyValueRow("Provider", "DigitalOcean")
            KeyValueRow("Provider state", j["provider_state"].string.map { ConsoleFormat.words($0) } ?? "—")
            if let ms = ConsoleFormat.ms(j["provider_observed_ms"]) {
                KeyValueRow("Provider checked", ConsoleFormat.when(ms))
            }
            KeyValueRow("Runner stage", j["stage"].string ?? "—", mono: true)
            KeyValueRow("Estimated cost", JobFormat.estimatedUsd(j["estimated_usd"]))
            if let error = j["error"].string, !error.isEmpty {
                Text(error)
                    .font(Theme.Fonts.meta)
                    .foregroundStyle(Theme.bad)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        Section {
            KeyValueRow("Started", ConsoleFormat.when(ConsoleFormat.ms(j["started_ms"])))
            KeyValueRow("Deadline", ConsoleFormat.when(ConsoleFormat.ms(j["deadline_ms"])))
            if let audit = j["audit_status"].string {
                ListRow(title: "Audit export") {
                    StatusPill(JobFormat.auditStatus(audit), tone: JobFormat.auditTone(audit))
                }
            }
        }
        Section("Job events") {
            let events = j["events"].items
            if events.isEmpty {
                MutedNoteRow("No runner stages have been recorded.")
            } else {
                ForEach(events, id: \.self) { e in
                    JobEventRow(event: e)
                }
            }
        }
    }
}

/// One runner stage: its time, the stage in SF Mono, and its outcome as a pill.
private struct JobEventRow: View {
    let event: JSON

    var body: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 1) {
                Text(ConsoleFormat.when(ConsoleFormat.ms(event["at_ms"])))
                    .font(Theme.Fonts.body)
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                if let stage = event["stage"].string, !stage.isEmpty {
                    Text(stage).idStyle().foregroundStyle(Theme.muted)
                }
            }
            Spacer(minLength: 8)
            StatusPill(
                ConsoleFormat.words(event["outcome"].string ?? "unknown"),
                tone: JobFormat.outcomeTone(event["outcome"].string))
        }
        .frame(minHeight: 32)
    }
}
