import SwiftUI

/// One run, read from `ai_agents_status`. Every field is optional on the wire.
struct RunInfo: Identifiable {
    let id: String
    let role: String
    let state: String
    let step: String?
    let parent: String?
    let title: String
    let createdMs: Double
    let endedMs: Double?
    /// How long it has run (ms): the server's `elapsed_ms`, else worked out from its times.
    let elapsedMs: Double
    let spent: Double?
    let budget: Double?
    /// The run waits for an answer or an approval from the person.
    let waitsOnPerson: Bool

    /// Whether the run has not ended.
    var isLive: Bool { ["queued", "preparing", "running", "waiting"].contains(state) }

    /// Reads one run object; nil when it has no id.
    init?(_ json: [String: Any]) {
        guard let id = json["run"] as? String else { return nil }
        self.id = id
        role = json["role"] as? String ?? "executor"
        state = json["state"] as? String ?? "unknown"
        let step = json["step"] as? String
        self.step = (step?.isEmpty == false) ? step : nil
        parent = json["parent_run"] as? String
        title = runTitle(json)
        createdMs = num(json["created_at"]) ?? nowMs()
        endedMs = num(json["ended_at"])
        let start = num(json["started_at"]) ?? createdMs
        elapsedMs = num(json["elapsed_ms"]) ?? max(0, (endedMs ?? nowMs()) - start)
        spent = parseCost((json["spent"] as? [String: Any])?["cost"])
        budget = parseCost((json["budget"] as? [String: Any])?["cost"])
        let kind = (json["waiting"] as? [String: Any])?["for"] as? String
        waitsOnPerson = state == "waiting" && (kind == "answer" || kind == "approval")
    }
}

/// A planner and its executors, drawn as one card.
struct PlannerGroup: Identifiable {
    let planner: RunInfo
    let executors: [RunInfo]
    var id: String { planner.id }

    /// Whether the planner or any executor waits on the person.
    var waitsOnPerson: Bool { planner.waitsOnPerson || executors.contains { $0.waitsOnPerson } }
}

/// Loads `ai_agents_status` and runs the card actions.
@MainActor
@Observable
final class StatusModel {
    private(set) var groups: [PlannerGroup] = []
    private(set) var paused = false
    private(set) var loaded = false
    private(set) var loadError: String?
    private(set) var actionError: String?
    private(set) var busy = false

    /// Fetches the last 100 runs and groups them by planner.
    func load(_ client: MCPClient) async {
        do {
            let data = try await client.call("ai_agents_status", ["limit": 100])
            guard let dict = data as? [String: Any], let rows = dict["runs"] as? [[String: Any]]
            else { throw PlatformError.badResponse("ai_agents_status shape") }
            paused = dict["paused"] as? Bool ?? false
            groups = Self.group(rows.compactMap(RunInfo.init))
            loaded = true
            loadError = nil
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            loadError = "ai_agents_status failed: \(error.localizedDescription)"
        }
    }

    /// Cards for the live runs and the runs that ended in the last 24 hours. A card with a run
    /// that waits on the person sorts first, then the newest.
    static func group(_ runs: [RunInfo]) -> [PlannerGroup] {
        let cutoff = nowMs() - 24 * 3_600_000
        func shown(_ r: RunInfo) -> Bool { r.isLive || (r.endedMs ?? 0) > cutoff }
        let ids = Set(runs.map(\.id))
        let planners = runs.filter {
            $0.role == "planner" || $0.parent == nil || !ids.contains($0.parent ?? "")
        }
        let cards = planners.compactMap { p -> PlannerGroup? in
            let kids = runs.filter { $0.parent == p.id && $0.id != p.id && shown($0) }
            guard shown(p) || !kids.isEmpty else { return nil }
            return PlannerGroup(planner: p, executors: kids.sorted { $0.createdMs < $1.createdMs })
        }
        return cards.sorted {
            if $0.waitsOnPerson != $1.waitsOnPerson { return $0.waitsOnPerson }
            return $0.planner.createdMs > $1.planner.createdMs
        }
    }

    /// Calls a write tool for a run, then reloads. The error stays until the next success.
    func act(_ tool: String, _ args: [String: Any], _ client: MCPClient) async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
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

    /// Pauses or resumes every agent, with `ai_agents_pause` or `ai_agents_resume`.
    func setPaused(_ pause: Bool, _ client: MCPClient) async {
        await act(pause ? "ai_agents_pause" : "ai_agents_resume", [:], client)
    }
}

/// A card action the person has started and not yet confirmed.
struct PendingAction: Identifiable {
    enum Kind { case note, extend, stop }
    let kind: Kind
    let run: String
    var id: String { "\(kind)-\(run)" }
}

/// The Status tab: a card for each planner with its executors.
struct StatusView: View {
    let session: Session
    let model: StatusModel
    let openApprovals: () -> Void
    @State private var pending: PendingAction?
    @State private var noteText = ""
    /// The run the open prompt is about; kept apart because the prompt clears `pending` first.
    @State private var target = ""

    var body: some View {
        Screen(
            errors: [model.loadError, model.actionError].compactMap { $0 },
            refresh: { await model.load(session.client) }
        ) {
            TitleRow(title: "Status")
            if model.groups.isEmpty && model.loaded {
                PlainCard {
                    Text("Nothing is running.").font(AppFont.body).foregroundStyle(Tokens.muted)
                }
            }
            ForEach(model.groups) { group in card(group) }
        }
        .toolbar(.hidden, for: .navigationBar)
        .task { await model.load(session.client) }
        .alert("Add a note", isPresented: showing(.note)) {
            TextField("Note for the run", text: $noteText)
            Button("Send") {
                let text = noteText.trimmingCharacters(in: .whitespacesAndNewlines)
                let run = target
                if !run.isEmpty, !text.isEmpty {
                    Task {
                        await model.act("ai_agents_note", ["run": run, "text": text], session.client)
                    }
                }
                noteText = ""
            }
            Button("Cancel", role: .cancel) { noteText = "" }
        }
        .alert("Add $5 to the budget?", isPresented: showing(.extend)) {
            Button("Add $5") {
                let run = target
                if !run.isEmpty {
                    Task {
                        await model.act(
                            "ai_agents_extend", ["run": run, "cost_usd": 5], session.client)
                    }
                }
            }
            Button("Cancel", role: .cancel) {}
        }
        .alert("Stop this run?", isPresented: showing(.stop)) {
            Button("Stop", role: .destructive) {
                let run = target
                if !run.isEmpty {
                    Task { await model.act("ai_agents_stop", ["run": run], session.client) }
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Its executors stop too.")
        }
    }

    /// Opens the prompt for `kind` about `run`.
    private func start(_ kind: PendingAction.Kind, _ run: String) {
        target = run
        pending = PendingAction(kind: kind, run: run)
    }

    /// A binding that is true while the pending action is of `kind`.
    private func showing(_ kind: PendingAction.Kind) -> Binding<Bool> {
        Binding(
            get: { pending?.kind == kind },
            set: { if !$0 { pending = nil } })
    }

    /// One planner's card.
    private func card(_ group: PlannerGroup) -> some View {
        let p = group.planner
        return PlainCard {
            runLines(p, big: true)
            ForEach(group.executors) { e in
                Divider()
                runLines(e, big: false)
            }
            if p.isLive {
                HStack {
                    Button("Note") { start(.note, p.id) }
                    Button("Extend") { start(.extend, p.id) }
                    Button("Stop") { start(.stop, p.id) }
                        .buttonStyle(OutlinedButtonStyle(colour: Tokens.warn))
                }
                .buttonStyle(OutlinedButtonStyle())
                .disabled(model.busy)
                .padding(.top, 4)
            }
        }
    }

    /// The lines of one run: title and time, state and step, role and budget used.
    private func runLines(_ r: RunInfo, big: Bool) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline) {
                Text(r.title).font(big ? AppFont.cardTitle : AppFont.body)
                    .foregroundStyle(Tokens.ink)
                Spacer(minLength: 8)
                Text(durationText(r.elapsedMs)).font(AppFont.meta).foregroundStyle(Tokens.muted)
            }
            Text(stateLine(r)).font(AppFont.meta).foregroundStyle(Tokens.muted)
            let used = budgetLine(r)
            Text(used.text).font(AppFont.meta)
                .foregroundStyle(used.high ? Tokens.warn : Tokens.muted)
            if r.waitsOnPerson {
                Button("Answer", action: openApprovals)
                    .buttonStyle(PrimaryButtonStyle())
                    .padding(.top, 4)
            }
        }
    }

    /// "Running · reviewing executor branches": the state, then the step when there is one.
    private func stateLine(_ r: RunInfo) -> String {
        let state = r.state.prefix(1).uppercased() + r.state.dropFirst()
        guard let step = r.step else { return state }
        return "\(state) · \(step)"
    }

    /// "Planner · 34% of $25 budget", and whether it is at 80% or more.
    private func budgetLine(_ r: RunInfo) -> (text: String, high: Bool) {
        let role = r.role.prefix(1).uppercased() + r.role.dropFirst()
        guard let budget = r.budget, budget > 0 else {
            if let spent = r.spent { return ("\(role) · \(money(spent)) spent", false) }
            return (String(role), false)
        }
        let share = (r.spent ?? 0) / budget
        return ("\(role) · \(Int((share * 100).rounded()))% of \(money(budget)) budget", share >= 0.8)
    }

    /// Dollars, without cents when whole.
    private func money(_ v: Double) -> String {
        v == v.rounded() ? "$\(Int(v))" : String(format: "$%.2f", v)
    }
}
