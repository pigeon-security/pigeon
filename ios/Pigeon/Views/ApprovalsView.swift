import SwiftUI

/// What an `approvals_list` item is, which fixes the writes that decide it.
enum ApprovalKind: String {
    case call = "Tool call"
    case draft = "Instance draft"
    case plan = "Linked plan"
    case other = "Other"
    case landing = "Landing"

    /// The console write that approves it.
    var approve: String { self == .landing ? "land-approve" : "approval-approve" }
    /// The console write that denies it; landings cannot be denied here.
    var deny: String? { self == .landing ? nil : "approval-deny" }
}

/// One thing waiting on the person: an approval, an agent's question or an assumed notice.
struct Entry: Identifiable {
    /// What the person can do about it.
    enum Body {
        case approval(ApprovalKind)
        case question(run: String, options: [String])
        case assumed(run: String, at: Double)
    }

    let id: String
    /// The kind, as a muted label.
    let label: String
    let title: String
    /// One line: who asked, or which run.
    let sub: String
    /// When it expires (ms), if it does.
    let expiresMs: Double?
    /// When it was made (ms), if known.
    let createdMs: Double?
    let body: Body
}

/// Loads `approvals_list` and `ai_agents_inbox`, and decides what waits.
@MainActor
@Observable
final class ApprovalsModel {
    private(set) var items: [Entry] = []
    private(set) var loadError: String?
    private(set) var inboxError: String?
    private(set) var actionError: String?
    private(set) var notice: String?
    private(set) var busy: String?
    @ObservationIgnored private let approver = Approver()
    @ObservationIgnored private var approvalItems: [Entry] = []
    @ObservationIgnored private var inboxItems: [Entry] = []
    @ObservationIgnored private static let dismissedKey = "dismissedNotices"

    /// How many items wait, for the tab's badge.
    var count: Int { items.count }

    /// Fetches both lists. A failure in one keeps the other's items.
    func load(_ client: MCPClient) async {
        await loadApprovals(client)
        await loadInbox(client)
        merge()
    }

    /// Puts the questions first, then the approvals, then the assumed notices.
    private func merge() {
        var questions: [Entry] = []
        var notices: [Entry] = []
        for e in inboxItems {
            if case .assumed = e.body { notices.append(e) } else { questions.append(e) }
        }
        items = questions + approvalItems + notices
    }

    /// Fetches the list with the MCP tool `approvals_list`.
    private func loadApprovals(_ client: MCPClient) async {
        do {
            let data = try await client.call("approvals_list")
            guard let dict = data as? [String: Any] else {
                throw PlatformError.badResponse("approvals_list shape")
            }
            // Every list is optional: an older or newer server may leave one out.
            func list(_ key: String) -> [[String: Any]] { dict[key] as? [[String: Any]] ?? [] }
            var found: [Entry] = []
            var skipped = 0
            for c in list("calls") {
                guard let id = c["id"] as? String else { skipped += 1; continue }
                var sub = "Asked by \(jsonText(c["caller"]))"
                if let f = c["acting_for"], !(f is NSNull) { sub += ", for \(jsonText(f))" }
                found.append(
                    entry(id, .call, c, title: c["tool"] as? String ?? "Tool call", sub: sub))
            }
            for (key, kind) in [("drafts", ApprovalKind.draft), ("plans", .plan)] {
                for d in list(key) {
                    guard let id = d["id"] as? String else { skipped += 1; continue }
                    let instance = d["instance"] as? String ?? kind.rawValue
                    found.append(entry(id, kind, d, title: instance, sub: "For \(instance)"))
                }
            }
            for o in list("other") {
                guard let id = o["id"] as? String else { skipped += 1; continue }
                let name = o["title"] as? String ?? id
                found.append(
                    entry(
                        id, .other, o, title: "\(o["kind"] as? String ?? "other"): \(name)",
                        sub: "Asked by \(jsonText(o["caller"]))"))
            }
            for l in list("landings") {
                guard let id = l["id"] as? String else { skipped += 1; continue }
                found.append(
                    entry(
                        id, .landing, l, title: l["branch"] as? String ?? "Landing",
                        sub: "Commit \(String(jsonText(l["commit"]).prefix(7)))"))
            }
            approvalItems = found
            if skipped > 0 {
                throw PlatformError.badResponse("\(skipped) waiting item(s) had no id")
            }
            loadError = nil
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            loadError = "approvals_list failed: \(error.localizedDescription)"
        }
    }

    /// Builds an approval's entry; the server's `plain` sentence is the title when it sends one.
    private func entry(
        _ id: String, _ kind: ApprovalKind, _ json: [String: Any], title: String, sub: String
    ) -> Entry {
        var shown = title
        if let plain = json["plain"] as? String, !plain.isEmpty { shown = plain }
        return Entry(
            id: id, label: kind.rawValue, title: shown, sub: sub,
            expiresMs: num(json["expires_ms"]), createdMs: num(json["created_ms"]),
            body: .approval(kind))
    }

    /// Fetches the agents' questions and assumed notices with `ai_agents_inbox`.
    private func loadInbox(_ client: MCPClient) async {
        do {
            let data = try await client.call("ai_agents_inbox", ["since": 0])
            guard let dict = data as? [String: Any] else {
                throw PlatformError.badResponse("ai_agents_inbox shape")
            }
            let dismissed = Set(UserDefaults.standard.stringArray(forKey: Self.dismissedKey) ?? [])
            var found: [Entry] = []
            var titles: [String: String] = [:]
            for run in dict["waiting"] as? [[String: Any]] ?? [] {
                guard let id = run["run"] as? String, let wait = run["waiting"] as? [String: Any],
                    wait["for"] as? String == "answer",
                    let question = wait["question"] as? String
                else { continue }
                titles[id] = runTitle(run)
                found.append(
                    Entry(
                        id: "question:\(id)", label: "Question", title: question,
                        sub: runTitle(run), expiresMs: nil, createdMs: num(wait["since"]),
                        body: .question(run: id, options: wait["options"] as? [String] ?? [])))
            }
            for note in dict["assumed"] as? [[String: Any]] ?? [] {
                guard let run = note["run"] as? String, let question = note["question"] as? String,
                    let at = num(note["at"])
                else { continue }
                if dismissed.contains(Self.mark(run, at)) { continue }
                found.append(
                    Entry(
                        id: "assumed:\(run):\(Int(at))", label: "Went ahead",
                        title: "Went ahead: \(question)", sub: titles[run] ?? "Run \(run)",
                        expiresMs: nil, createdMs: at, body: .assumed(run: run, at: at)))
            }
            inboxItems = found
            inboxError = nil
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            inboxError = "ai_agents_inbox failed: \(error.localizedDescription)"
        }
    }

    /// The key kept for a dismissed notice.
    private static func mark(_ run: String, _ at: Double) -> String { "\(run)|\(Int(at))" }

    /// Approves (or denies) `item` with a passkey, then reloads.
    func decide(_ item: Entry, approve: Bool, client: MCPClient) async {
        guard busy == nil, case .approval(let kind) = item.body else { return }
        guard let action = approve ? kind.approve : kind.deny else { return }
        busy = item.id
        defer { busy = nil }
        do {
            notice = try await approver.run(action: action, target: item.id, client: client)
            actionError = nil
            await load(client)
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            actionError = "\(action) failed: \(error.localizedDescription)"
        }
    }

    /// Sends an answer to an agent's question with `ai_agents_answer`, then reloads.
    func answer(run: String, text: String, client: MCPClient) async {
        await act("ai_agents_answer", ["run": run, "answer": text], client)
    }

    /// Stops a run with `ai_agents_stop`, then reloads.
    func stop(run: String, client: MCPClient) async {
        await act("ai_agents_stop", ["run": run], client)
    }

    /// Hides an assumed notice on this device and remembers it.
    func dismiss(run: String, at: Double) {
        var kept = UserDefaults.standard.stringArray(forKey: Self.dismissedKey) ?? []
        kept.append(Self.mark(run, at))
        UserDefaults.standard.set(Array(kept.suffix(200)), forKey: Self.dismissedKey)
        let id = "assumed:\(run):\(Int(at))"
        inboxItems.removeAll { $0.id == id }
        merge()
    }

    /// Calls a write tool for one item and reloads; its error stays until the next success.
    private func act(_ tool: String, _ args: [String: Any], _ client: MCPClient) async {
        guard busy == nil else { return }
        busy = tool
        defer { busy = nil }
        do {
            _ = try await client.call(tool, args)
            actionError = nil
            await load(client)
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            actionError = "\(tool) failed: \(error.localizedDescription)"
        }
    }
}

/// Everything waiting on the person: passkey decisions, agent questions and assumed notices.
struct ApprovalsView: View {
    let session: Session
    let model: ApprovalsModel

    var body: some View {
        Screen(
            errors: [model.loadError, model.inboxError, model.actionError].compactMap { $0 },
            refresh: { await model.load(session.client) }
        ) {
            TitleRow(title: "Approvals", count: model.count)
            if let notice = model.notice {
                PlainCard { Text(notice).font(AppFont.body).foregroundStyle(Tokens.good) }
            }
            if model.items.isEmpty {
                PlainCard {
                    Text("Nothing waits for you.").font(AppFont.body).foregroundStyle(Tokens.muted)
                }
            }
            ForEach(model.items) { item in
                EntryCard(item: item, model: model, session: session)
            }
        }
        .toolbar(.hidden, for: .navigationBar)
        .task { await model.load(session.client) }
    }
}

/// One card of the Approvals list, with the buttons that fit its kind.
struct EntryCard: View {
    let item: Entry
    let model: ApprovalsModel
    let session: Session
    @State private var typing = false
    @State private var text = ""

    var body: some View {
        PlainCard {
            HStack {
                Text(item.label).font(AppFont.label).foregroundStyle(Tokens.muted)
                Spacer()
                timeView
            }
            Text(item.title).font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
            Text(item.sub).font(AppFont.meta).foregroundStyle(Tokens.muted)
            buttons.padding(.top, 4)
        }
    }

    /// The time left (orange under 15 minutes) or the time since it was made.
    @ViewBuilder
    private var timeView: some View {
        if let expires = item.expiresMs {
            Text(timeLeftText(expires)).font(AppFont.meta)
                .foregroundStyle(expires - nowMs() < 15 * 60_000 ? Tokens.warn : Tokens.muted)
        } else if let created = item.createdMs {
            Text(timeAgoText(created)).font(AppFont.meta).foregroundStyle(Tokens.muted)
        }
    }

    @ViewBuilder
    private var buttons: some View {
        switch item.body {
        case .approval(let kind):
            HStack {
                Button("Approve") {
                    Task { await model.decide(item, approve: true, client: session.client) }
                }
                .buttonStyle(PrimaryButtonStyle())
                if kind.deny != nil {
                    Button("Deny") {
                        Task { await model.decide(item, approve: false, client: session.client) }
                    }
                    .buttonStyle(OutlinedButtonStyle(colour: Tokens.warn))
                }
            }
            .disabled(model.busy != nil)
        case .question(let run, let options):
            VStack(alignment: .leading, spacing: 8) {
                ForEach(options, id: \.self) { option in
                    Button(option) {
                        Task { await model.answer(run: run, text: option, client: session.client) }
                    }
                    .buttonStyle(OutlinedButtonStyle())
                }
                Button("Something else…") { typing.toggle() }
                    .buttonStyle(OutlinedButtonStyle())
                if typing {
                    TextField("Your answer", text: $text, axis: .vertical)
                        .font(AppFont.body)
                        .padding(10)
                        .background(RoundedRectangle(cornerRadius: 8).fill(Tokens.chip))
                    Button("Send") {
                        let answer = text.trimmingCharacters(in: .whitespacesAndNewlines)
                        guard !answer.isEmpty else { return }
                        Task { await model.answer(run: run, text: answer, client: session.client) }
                    }
                    .buttonStyle(PrimaryButtonStyle())
                }
            }
            .disabled(model.busy != nil)
        case .assumed(let run, let at):
            HStack {
                Button("Fine, carry on") { model.dismiss(run: run, at: at) }
                    .buttonStyle(PrimaryButtonStyle())
                Button("Stop the run") {
                    Task { await model.stop(run: run, client: session.client) }
                }
                .buttonStyle(OutlinedButtonStyle(colour: Tokens.warn))
            }
            .disabled(model.busy != nil)
        }
    }
}
