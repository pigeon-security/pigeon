import SwiftUI

/// One thing waiting for the person's passkey.
struct Waiting: Identifiable {
    /// What it is, which fixes the writes that decide it.
    enum Kind: String {
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

    let id: String
    let kind: Kind
    let title: String
    let lines: [String]
}

/// Loads `approvals_list` and decides items with a passkey.
@MainActor
@Observable
final class ApprovalsModel {
    private(set) var items: [Waiting] = []
    private(set) var loadError: String?
    private(set) var actionError: String?
    private(set) var notice: String?
    private(set) var busy: String?
    @ObservationIgnored private let approver = Approver()

    /// How many items wait, for the tab's badge.
    var count: Int { items.count }

    /// Fetches the list with the MCP tool `approvals_list`.
    func load(_ client: MCPClient) async {
        do {
            let data = try await client.call("approvals_list")
            guard let dict = data as? [String: Any],
                let calls = dict["calls"] as? [[String: Any]],
                let drafts = dict["drafts"] as? [[String: Any]],
                let plans = dict["plans"] as? [[String: Any]],
                let landings = dict["landings"] as? [[String: Any]]
            else { throw PlatformError.badResponse("approvals_list shape") }
            // An older server sends no `other`; that is an empty list.
            let other = dict["other"] as? [[String: Any]] ?? []
            var found: [Waiting] = []
            var skipped = 0
            for c in calls {
                guard let id = c["id"] as? String else { skipped += 1; continue }
                found.append(
                    Waiting(
                        id: id, kind: .call, title: c["tool"] as? String ?? "Tool call",
                        lines: [
                            "Asked by \(jsonText(c["caller"]))",
                            "Acting for \(jsonText(c["acting_for"]))",
                            "Arguments: \(jsonText(c["args"]))",
                            "Expires \(whenText(c["expires_ms"] as? Double))",
                            "Digest \(jsonText(c["digest"]))",
                        ]))
            }
            for (list, kind) in [(drafts, Waiting.Kind.draft), (plans, .plan)] {
                for d in list {
                    guard let id = d["id"] as? String else { skipped += 1; continue }
                    found.append(
                        Waiting(
                            id: id, kind: kind, title: d["instance"] as? String ?? kind.rawValue,
                            lines: [
                                jsonText(d["summary"]),
                                "Expires \(whenText(d["expires_ms"] as? Double))",
                                "Digest \(jsonText(d["digest"]))",
                            ]))
                }
            }
            for o in other {
                guard let id = o["id"] as? String else { skipped += 1; continue }
                let summary = o["summary"]
                let plan = ((summary as? [String: Any])?["plan"] as? [Any] ?? []).map {
                    jsonText($0)
                }
                var lines = plan
                if let summary, !(summary is NSNull) { lines.append(jsonText(summary)) }
                lines += [
                    "Asked \(whenText(o["created_ms"] as? Double))",
                    "Expires \(whenText(o["expires_ms"] as? Double))",
                    "Digest \(jsonText(o["digest"]))",
                ]
                found.append(
                    Waiting(
                        id: id, kind: .other,
                        title: "\(o["kind"] as? String ?? "other"): \(o["title"] as? String ?? id)",
                        lines: lines))
            }
            for l in landings {
                guard let id = l["id"] as? String else { skipped += 1; continue }
                found.append(
                    Waiting(
                        id: id, kind: .landing, title: l["branch"] as? String ?? "Landing",
                        lines: [
                            "Commit \(jsonText(l["commit"]))", "State \(jsonText(l["state"]))",
                        ]))
            }
            items = found
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

    /// Approves (or denies) `item` with a passkey, then reloads.
    func decide(_ item: Waiting, approve: Bool, client: MCPClient) async {
        guard busy == nil else { return }
        guard let action = approve ? item.kind.approve : item.kind.deny else { return }
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
}

/// What waits for the person's passkey, with a button to approve or deny each.
struct ApprovalsView: View {
    let session: Session
    let model: ApprovalsModel

    var body: some View {
        Screen(
            errors: [model.loadError, model.actionError].compactMap { $0 },
            refresh: { await model.load(session.client) }
        ) {
            Eyebrow(text: "Waiting for you")
            if let notice = model.notice {
                Card { Text(notice).foregroundStyle(Tokens.good) }
            }
            if model.items.isEmpty {
                Card { Text("Nothing waits for your passkey.") }
            }
            ForEach(model.items) { item in
                Card {
                    Eyebrow(text: item.kind.rawValue)
                    CardTitle(text: item.title)
                    ForEach(Array(item.lines.enumerated()), id: \.offset) { _, line in
                        Text(line)
                    }
                    HStack {
                        Button("Approve") {
                            Task { await model.decide(item, approve: true, client: session.client) }
                        }
                        .buttonStyle(PrimaryButtonStyle())
                        if item.kind.deny != nil {
                            Button("Deny") {
                                Task {
                                    await model.decide(item, approve: false, client: session.client)
                                }
                            }
                            .font(.token(Tokens.typeButton))
                            .foregroundStyle(Tokens.warn)
                        }
                    }
                    .disabled(model.busy != nil)
                }
            }
        }
        .navigationTitle("Approvals")
        .navigationBarTitleDisplayMode(.inline)
        .task { await model.load(session.client) }
    }
}
