import SwiftUI

/// One request that waits for the person's passkey, as the web's `inboxItems` builds it.
struct InboxItem: Identifiable {
    let id: String
    /// "Tool call", "Instance change", "Linked plan", "Landing", or the request's own kind.
    let kind: String
    let instance: String
    /// The verb before the subject: "Run", "Change", "Plan for", "Land".
    let verb: String
    /// The subject, in SF Mono when `monoSubject`.
    let subject: String
    let monoSubject: Bool
    /// The request in plain words, for the confirm sheet.
    let plain: String
    let by: String
    let since: Double?
    let expires: Double?
    let action: String
    let deny: Bool
    let lines: [DiffLine]
    let effect: String?
    /// Facts the web shows when the row is expanded.
    let detail: [Fact]

    /// Reads `app_inbox`'s `calls`, `drafts`, `plans`, `other` and `landings`; `home` is the
    /// instance the list is for (landings and requests without one belong to it).
    static func items(_ data: JSON, home: String) -> [InboxItem] {
        func by(_ caller: JSON, _ actingFor: JSON) -> String {
            if let a = actingFor.string, !a.isEmpty { return "\(caller.string ?? "—") for \(a)" }
            return caller.string ?? "—"
        }
        /// A request a run asked for says which approval's run it follows.
        func follows(_ item: JSON) -> [DiffLine] {
            guard let after = item["after"].string else { return [] }
            return [.note("asked by the run of approval \(after)")]
        }
        var out: [InboxItem] = []
        for c in data["calls"].items {
            let tool = c["tool"].string ?? "tool"
            let inst = c["args"]["instance"].string ?? c["instance"].string
                ?? c["summary"]["instance"].string ?? home
            var detail: [Fact] = []
            if let run = c["run"].string { detail.append(Fact("Run", run, mono: true)) }
            if let perms = c["grants"]["permissions"].array {
                detail.append(Fact("Token permissions", perms.compactMap(\.string).joined(separator: ", ")))
            }
            if let ttl = c["grants"]["ttl_secs"].text { detail.append(Fact("Token lifetime", "\(ttl) s")) }
            var lines: [DiffLine] = follows(c)
            if let args = c["args"].object {
                for key in args.keys.sorted() { lines.append(.note("\(key): \(args[key]?.compactText ?? "null")")) }
            }
            lines.append(.note("call digest \(c["digest"].string ?? "—")"))
            out.append(InboxItem(
                id: c["id"].text ?? "", kind: "Tool call", instance: inst, verb: "Run", subject: tool,
                monoSubject: true, plain: tool, by: by(c["caller"], c["acting_for"]),
                since: ConsoleFormat.ms(c["created_ms"]), expires: ConsoleFormat.ms(c["expires_ms"]),
                action: "approval-approve", deny: true, lines: lines,
                effect: tool == "source_token"
                    ? "Approving lets this exact call run once, if its caller calls again within 10 minutes."
                    : "Approving runs this exact call once, now, as its caller.",
                detail: detail))
        }
        for d in data["drafts"].items {
            let inst = d["instance"].string ?? home
            let from = d["summary"]["version_from"].int ?? 0
            var lines: [DiffLine] = follows(d) + [.note("record version \(from) → \(from + 1)")]
            for c in d["summary"]["changes"].items {
                let path = c["path"].text ?? ""
                if !c["from"].isNull { lines.append(.remove("\(path): \(c["from"].compactText)")) }
                if !c["to"].isNull { lines.append(.add("\(path): \(c["to"].compactText)")) }
            }
            out.append(InboxItem(
                id: d["id"].text ?? "", kind: "Instance change", instance: inst, verb: "Change",
                subject: inst, monoSubject: false, plain: "change to \(inst)",
                by: by(d["caller"], d["acting_for"]), since: ConsoleFormat.ms(d["created_ms"]),
                expires: ConsoleFormat.ms(d["expires_ms"]), action: "approval-approve", deny: true,
                lines: lines,
                effect: "Approving applies exactly these changes now, unless the instance changed since this was proposed. Then the instance is planned: a plan with something to do comes back here for your approval.",
                detail: []))
        }
        for p in data["plans"].items {
            let inst = p["instance"].string ?? home
            let steps = p["summary"]["plan"].items.map { $0.text ?? $0.compactText }
            var detail: [Fact] = []
            if let account = p["summary"]["account_id"].string { detail.append(Fact("Account", account, mono: true)) }
            if let digest = p["digest"].string { detail.append(Fact("Plan digest", digest, mono: true)) }
            out.append(InboxItem(
                id: p["id"].text ?? "", kind: "Linked plan", instance: inst, verb: "Plan for",
                subject: inst, monoSubject: false, plain: "plan for \(inst)",
                by: by(p["caller"], p["acting_for"]), since: ConsoleFormat.ms(p["created_ms"]),
                expires: ConsoleFormat.ms(p["expires_ms"]), action: "approval-approve", deny: true,
                lines: follows(p) + steps.map { .add($0) },
                effect: "Approving runs exactly these steps once, now. If the account no longer gives the same plan, nothing runs. Nothing is deleted.",
                detail: detail))
        }
        for o in data["other"].items {
            let kind = o["kind"].string ?? "Request"
            let title = o["title"].string ?? kind
            let steps = o["summary"]["plan"].items.map { $0.text ?? $0.compactText }
            var detail: [Fact] = []
            if let digest = o["digest"].string { detail.append(Fact("Digest", digest, mono: true)) }
            out.append(InboxItem(
                id: o["id"].text ?? "", kind: kind, instance: o["instance"].string ?? home, verb: "",
                subject: title, monoSubject: false, plain: title, by: by(o["caller"], o["acting_for"]),
                since: ConsoleFormat.ms(o["created_ms"]), expires: ConsoleFormat.ms(o["expires_ms"]),
                action: "approval-approve", deny: true, lines: follows(o) + steps.map { .add($0) },
                effect: "Approving runs exactly this once, now, as the caller that asked, if it still holds.",
                detail: detail))
        }
        for l in data["landings"].items {
            let branch = ConsoleFormat.branch(l["branch"].string)
            let commit = ConsoleFormat.short(l["commit"].string)
            var detail: [Fact] = []
            if let why = l["needs_approval"].string { detail.append(Fact("Why", why)) }
            detail.append(Fact("Commit", commit, mono: true))
            if let started = ConsoleFormat.ms(l["created_at"]) {
                detail.append(Fact("Started", ConsoleFormat.when(started)))
            }
            if let more = l["detail"].string { detail.append(Fact("Detail", more)) }
            let since = ConsoleFormat.ms(l["awaiting_since"]) ?? ConsoleFormat.ms(l["created_at"])
            out.append(InboxItem(
                id: l["id"].text ?? "", kind: "Landing", instance: home, verb: "Land", subject: branch,
                monoSubject: true, plain: branch, by: l["by"].string ?? "—", since: since, expires: nil,
                action: "land-approve", deny: false,
                lines: [.note("commit \(commit)"), .add("land \(branch) on main")],
                effect: l["needs_approval"].string.map { "Why it waits: \($0)" }, detail: detail))
        }
        // Soonest to expire first; landings, which do not expire, after them, longest waiting first.
        return out.sorted { a, b in
            let ea = a.expires ?? .infinity
            let eb = b.expires ?? .infinity
            if ea != eb { return ea < eb }
            return (a.since ?? 0) < (b.since ?? 0)
        }
    }
}

/// Inbox: every request that waits for the person's passkey (approvals and landings), as the
/// web's Inbox. Home shows all of them; another instance only its own. Approve and Deny open
/// the confirm sheet (`approval-approve`, `approval-deny`, `land-approve`); a landing has no deny.
struct InboxScreen: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @Environment(ManifestStore.self) private var store
    @State private var inbox = OpResource<JSON>("app_inbox")
    @State private var request: WriteRequest?

    private var all: Bool { context.instance.home }

    var body: some View {
        List {
            ViewPickerSection(context: context)
            UpgradeSection(context: context)
            Section {
                MutedNoteRow(all ? "Every request that waits for your passkey." : "Requests for \(context.instance.title).")
            }
            LoadableSection(
                state: inbox.state,
                emptyTitle: "Nothing waits for you here",
                emptyMessage: all ? nil : "Requests about \(context.instance.title) appear here.",
                isEmpty: { InboxItem.items($0, home: context.instance.id).isEmpty },
                retry: { await load() }
            ) { data in
                Section {
                    ForEach(InboxItem.items(data, home: context.instance.id)) { item in
                        row(item)
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task {
            await load()
            reviewNotifiedApproval()
        }
        .onChange(of: NotificationRoute.shared.serial) { _, _ in
            Task {
                await load()
                reviewNotifiedApproval()
            }
        }
        .confirmSheet($request)
    }

    private func load() async {
        let args: JSON = all ? [:] : ["instance": .string(context.instance.id)]
        await inbox.load(api, args: args)
    }

    /// A tapped approval notification names an approval; open its confirm sheet (approve), so
    /// the person still decides here with the passkey. Nothing is decided from the notification.
    private func reviewNotifiedApproval() {
        guard request == nil, let data = inbox.value else { return }
        guard let id = NotificationRoute.shared.takeApproval() else { return }
        if let item = InboxItem.items(data, home: context.instance.id).first(where: { $0.id == id }) {
            decide(item, approve: true)
        }
    }

    private func row(_ item: InboxItem) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                TagLabel(item.kind)
                if all {
                    Text(item.instance).idStyle(.system(.caption2, design: .monospaced)).foregroundStyle(Theme.muted)
                }
            }
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                if !item.verb.isEmpty {
                    Text(item.verb).font(Theme.Fonts.body).foregroundStyle(Theme.ink)
                }
                Text(item.subject)
                    .font(item.monoSubject ? Theme.Fonts.idBody : Theme.Fonts.body.weight(.semibold))
                    .foregroundStyle(Theme.ink)
                    .lineLimit(2)
                    .truncationMode(.middle)
            }
            Text("\(item.by) · waiting \(ConsoleFormat.ago(item.since))")
                .font(Theme.Fonts.meta)
                .foregroundStyle(Theme.muted)
                .lineLimit(2)
            HStack(spacing: 8) {
                Button("Approve") { decide(item, approve: true) }
                    .buttonStyle(RowButtonStyle(.primary))
                if item.deny {
                    Button("Deny") { decide(item, approve: false) }
                        .buttonStyle(RowButtonStyle(.quiet))
                }
                Spacer(minLength: 8)
                if let expires = item.expires, let left = ConsoleFormat.left(expires) {
                    StatusPill(left, tone: ConsoleFormat.minutesLeft(expires) < 10 ? .bad : .warn)
                        .accessibilityLabel("Expires in \(left)")
                }
            }
            .padding(.top, 4)
        }
        .padding(.vertical, 4)
    }

    private func decide(_ item: InboxItem, approve: Bool) {
        var facts: [Fact] = [
            Fact("Request", item.plain, mono: item.monoSubject),
            Fact("Instance", item.instance, mono: true),
            Fact("Requested by", item.by),
        ]
        if approve { facts.append(contentsOf: item.detail) }
        if let expires = item.expires, let left = ConsoleFormat.left(expires) {
            facts.append(Fact("Expires", "in \(left)"))
        }
        facts.append(Fact("Id", item.id, mono: true))
        let kind = item.kind.lowercased()
        request = WriteRequest(
            title: approve ? "Approve \(kind)" : "Deny \(kind)",
            subtitle: approve ? item.effect : "The caller is told it was denied. Nothing runs.",
            facts: facts,
            lines: approve ? item.lines : [],
            action: approve ? item.action : "approval-deny",
            target: .string(item.id),
            destructive: !approve,
            confirmLabel: approve ? "Approve with passkey" : "Deny with passkey",
            onDone: { _ in
                Task {
                    await load()
                    await store.refresh(api)
                }
            })
    }
}
