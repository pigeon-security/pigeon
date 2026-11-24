import SwiftUI

/// A read of the declarations, instance record and server upgrade plan.
struct CapabilitySnapshot {
    struct Work: Decodable, Equatable {
        let version: String
        let summary: String
        let migration: String?
    }

    struct Step: Decodable, Equatable {
        let component: String
        let pin: String
        let from: String?
        let to: String
        let suggested: String
        let work: [Work]

        /// Held pins have no selected version; compare the target with the pin's major.
        var breaking: Bool {
            func major(_ text: String) -> UInt64? {
                let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
                let version = trimmed.trimmingCharacters(in: CharacterSet(charactersIn: "^~="))
                return UInt64(version.split(separator: ".").first?.trimmingCharacters(in: .whitespaces) ?? "")
            }
            return !work.isEmpty || major(from ?? pin) != major(to)
        }
    }

    struct Row: Identifiable {
        let id: String
        let version: String?
        let step: Step?
        let issue: String?
    }

    private struct Versions: Decodable {
        struct Component: Decodable {
            let name: String
            let kind: String
            let version: String
            let problems: [String]
        }
        struct Instance: Decodable {
            struct Pin: Decodable {
                let component: String
                let requirement: String
                let selected: String?
                let note: String
            }
            let id: String
            let pins: [Pin]
        }
        let components: [Component]
        let undeclared: [String]
        let instances: [Instance]
    }

    private struct Plan: Decodable {
        struct Instance: Decodable {
            let id: String
            let steps: [Step]
        }
        let instances: [Instance]
    }

    let instanceID: String?
    let record: [String: Any]?
    let rows: [Row]
    var version: UInt64? { (record?["version"] as? NSNumber)?.uint64Value }
    var pins: [String: String] { record?["pins"] as? [String: String] ?? [:] }
    var steps: [Step] { rows.compactMap(\.step) }

    /// Matches the plan and pin status to the same record before allowing it to be edited.
    @MainActor static func read(_ instanceID: String?, client: MCPClient) async throws -> Self {
        var record: [String: Any]?
        if let instanceID {
            let result = try await client.call("instances_list")
            guard let records = result as? [[String: Any]] else {
                throw PlatformError.badResponse("instance records")
            }
            record = records.first { $0["id"] as? String == instanceID }
        }
        let scope = record == nil ? [:] : ["instance": instanceID ?? ""]
        let versions = try decode(Versions.self, try await client.call("capabilities_versions", scope))
        var checks: [Versions.Instance.Pin] = []
        var steps: [Step] = []
        if let record {
            let plan = try decode(Plan.self, try await client.call("capabilities_upgrade_plan", scope))
            guard record["version"] is NSNumber,
                let status = versions.instances.first(where: { $0.id == instanceID }),
                let planned = plan.instances.first(where: { $0.id == instanceID })
            else { throw PlatformError.badResponse("instance version or upgrade plan") }
            let pins = record["pins"] as? [String: String] ?? [:]
            guard status.pins.count == pins.count,
                status.pins.allSatisfy({ pins[$0.component] == $0.requirement }),
                planned.steps.allSatisfy({ step in
                    pins[step.component] == step.pin
                        && status.pins.first(where: { $0.component == step.component })?.selected == step.from
                        && versions.components.first(where: { $0.name == step.component })?.version == step.to
                })
            else { throw PlatformError.server("The instance changed while loading. Pull to refresh.") }
            checks = status.pins
            steps = planned.steps
        }
        let components = versions.components.filter { $0.kind == "capability" }
        let declared = components.map { component -> Row in
            let pin = checks.first { $0.component == component.name }
            let issue = component.problems.isEmpty ? nil : component.problems.joined(separator: "\n")
            let step = steps.first { $0.component == component.name && $0.to == component.version }
            return Row(
                id: component.name, version: issue == nil ? (pin == nil ? component.version : pin?.selected) : nil,
                step: issue == nil ? step : nil, issue: issue ?? (pin?.selected == nil ? pin?.note : nil))
        }
        let undeclared = versions.undeclared.filter { name in !components.contains { $0.name == name } }
        let rows = declared + undeclared.map {
            Row(id: $0, version: nil, step: nil, issue: "No version declared.")
        }
        return Self(instanceID: instanceID, record: record, rows: rows.sorted { $0.id < $1.id })
    }

    private static func decode<T: Decodable>(_ type: T.Type, _ json: Any) throws -> T {
        try JSONDecoder().decode(type, from: JSONSerialization.data(withJSONObject: json, options: .fragmentsAllowed))
    }
}

/// Keeps proposal, passkey and record confirmation separate from the read-only plan.
@MainActor
@Observable
final class CapabilityUpgrades {
    private struct Draft {
        let id: String
        let base: UInt64
        let pins: [String: String]
    }

    struct Confirmation {
        let pins: [String: String]
        let at: Date
    }

    private var snapshot: CapabilitySnapshot?
    private var drafts: [String: Draft] = [:]
    private var confirmations: [String: Confirmation] = [:]
    private var generation = 0
    private(set) var loading = false
    private(set) var working: Set<String> = []
    private(set) var error: String?
    private(set) var notice: String?

    func current(_ id: String?) -> CapabilitySnapshot? {
        guard snapshot?.instanceID == id else { return nil }
        return snapshot
    }

    func waiting(_ id: String?) -> Bool { id.map { drafts[$0] != nil } ?? false }

    func confirmation(_ id: String?) -> Confirmation? { id.flatMap { confirmations[$0] } }

    /// Pull to refresh updates the plan and checks any outstanding draft once.
    func load(_ id: String?, client: MCPClient) async {
        guard working.isEmpty else { return }
        generation += 1
        let request = generation
        loading = true
        defer { if request == generation { loading = false } }
        do {
            let status = try await draftStatus(id, client: client)
            let fresh = try await CapabilitySnapshot.read(id, client: client)
            guard request == generation, !Task.isCancelled else { return }
            error = nil
            accept(fresh, status: status)
        } catch {
            guard request == generation, !Task.isCancelled else { return }
            show(error)
        }
    }

    /// Proposes a single whole record, even when it includes multiple patch pins.
    func propose(_ steps: [CapabilitySnapshot.Step], id: String?, client: MCPClient) async {
        guard let id, let shown = current(id), let base = shown.version, !steps.isEmpty,
            !loading, working.isEmpty, drafts[id] == nil else { return }
        working = Set(steps.map(\.component))
        defer { working = [] }
        error = nil
        notice = nil
        do {
            let fresh = try await CapabilitySnapshot.read(id, client: client)
            snapshot = fresh
            guard var record = fresh.record, fresh.version == base, fresh.steps == shown.steps,
                steps.allSatisfy({ fresh.steps.contains($0) }) else {
                throw PlatformError.server("The upgrade plan changed. Review it before trying again.")
            }
            var pins = fresh.pins
            var moved: [String: String] = [:]
            for step in steps {
                pins[step.component] = step.suggested
                moved[step.component] = step.suggested
            }
            record["pins"] = pins
            let answer = try await client.call("instance_propose", ["record": record]) as? [String: Any]
            if let problems = answer?["problems"] as? [String], !problems.isEmpty {
                throw PlatformError.server(problems.joined(separator: "\n"))
            }
            guard let approval = answer?["approval"] as? String else {
                throw PlatformError.badResponse("instance proposal")
            }
            let draft = Draft(id: approval, base: base, pins: moved)
            drafts[id] = draft
            try await approve(draft, instance: id, client: client)
        } catch { show(error) }
    }

    /// Resumes the same draft after a cancelled or interrupted passkey prompt.
    func resume(_ id: String?, client: MCPClient) async {
        guard let id, let draft = drafts[id], working.isEmpty, !loading else { return }
        working = Set(draft.pins.keys)
        defer { working = [] }
        error = nil
        do {
            let status = try await draftStatus(id, client: client)
            if status == "pending" {
                try await approve(draft, instance: id, client: client)
            } else {
                let fresh = try await CapabilitySnapshot.read(id, client: client)
                accept(fresh, status: status)
            }
        } catch { show(error) }
    }

    private func approve(_ draft: Draft, instance: String, client: MCPClient) async throws {
        notice = try await Approver().run(action: "approval-approve", target: draft.id, client: client)
        let status = try await draftStatus(instance, client: client)
        let fresh = try await CapabilitySnapshot.read(instance, client: client)
        accept(fresh, status: status)
    }

    private func draftStatus(_ id: String?, client: MCPClient) async throws -> String? {
        guard let id, let draft = drafts[id] else { return nil }
        let result = try await client.call("approvals_status", ["id": draft.id]) as? [String: Any]
        guard let state = result?["state"] as? String else {
            throw PlatformError.badResponse("upgrade approval status")
        }
        return state == "used" && result?["decided_by_person"] as? Bool != true ? "unconfirmed" : state
    }

    private func accept(_ fresh: CapabilitySnapshot, status: String?) {
        snapshot = fresh
        guard let id = fresh.instanceID, let draft = drafts[id] else { return }
        switch status {
        case "used":
            guard let version = fresh.version, version > draft.base,
                draft.pins.allSatisfy({ fresh.pins[$0.key] == $0.value }) else {
                notice = "Approval was recorded, but the new pins could not be confirmed. Pull to refresh."
                return
            }
            confirmations[id] = Confirmation(pins: draft.pins, at: .now)
            drafts[id] = nil
            notice = nil
        case "denied", "expired", "unconfirmed":
            drafts[id] = nil
            notice = "The upgrade was not confirmed (\(status ?? "unknown")). Review the plan before trying again."
        default:
            break
        }
    }

    private func show(_ failure: Error) {
        if case PlatformError.signedOut = failure { return }
        error = failure.localizedDescription
    }
}
