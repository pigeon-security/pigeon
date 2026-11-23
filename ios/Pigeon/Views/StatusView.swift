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

    /// Run ids stopped from this app in this session. The server does not say who stopped a run.
    private(set) var stoppedHere: Set<String> = []

    /// Calls a write tool for a run, then reloads. The error stays until the next success.
    /// Answers whether the call worked.
    @discardableResult
    func act(_ tool: String, _ args: [String: Any], _ client: MCPClient) async -> Bool {
        guard !busy else { return false }
        busy = true
        defer { busy = false }
        do {
            _ = try await client.call(tool, args)
            actionError = nil
            if tool == "ai_agents_stop", let run = args["run"] as? String {
                stoppedHere.insert(run)
            }
            await load(client)
            return true
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            actionError = "\(tool) failed: \(error.localizedDescription)"
        }
        return false
    }

    /// Pauses or resumes every agent, with `ai_agents_pause` or `ai_agents_resume`.
    func setPaused(_ pause: Bool, _ client: MCPClient) async {
        await act(pause ? "ai_agents_pause" : "ai_agents_resume", [:], client)
    }
}

/// The panel open inside a planner card.
private enum StatusPanel {
    case note, extend, stop
}

/// A time in ms as "1 h 12 m", "34 m", "2 d 3 h" or "under 1 m".
private func elapsedText(_ ms: Double) -> String {
    let minutes = Int(max(0, ms) / 60_000)
    if minutes < 1 { return "under 1 m" }
    if minutes < 60 { return "\(minutes) m" }
    if minutes < 1440 { return "\(minutes / 60) h \(minutes % 60) m" }
    return "\(minutes / 1440) d \((minutes % 1440) / 60) h"
}

/// Dollars, without cents when whole.
private func money(_ v: Double) -> String {
    v == v.rounded() ? "$\(Int(v))" : String(format: "$%.2f", v)
}

/// The state word of a run, capitalised; a run that waits on the person reads "Waiting on you".
private func stateWord(_ r: RunInfo) -> String {
    if r.waitsOnPerson { return "Waiting on you" }
    return r.state.prefix(1).uppercased() + r.state.dropFirst()
}

/// The colour of a run's state word.
private func stateColour(_ r: RunInfo) -> Color {
    if r.waitsOnPerson { return Tokens.warn }
    return r.state == "running" ? Tokens.good : Tokens.muted
}

/// The state line of a run as one wrapping text: dot, state, step.
private func stateText(_ r: RunInfo, dot: CGFloat, byYou: Bool, extra: String?) -> Text {
    let colour = stateColour(r)
    var text =
        Text(Image(systemName: "circle.fill")).font(.system(size: dot)).foregroundColor(colour)
        + Text(" ")
        + Text(stateWord(r)).fontWeight(.semibold).foregroundColor(colour)
    if byYou { text = text + Text(" · by you") }
    if let step = r.step, r.state != "stopped" { text = text + Text(" · \(step)") }
    if let extra { text = text + Text(" · \(extra)") }
    return text
}

/// How much of its budget a run has spent (1 is all of it), when it has a budget.
private func budgetShare(_ r: RunInfo) -> Double? {
    guard let budget = r.budget, budget > 0 else { return nil }
    return (r.spent ?? 0) / budget
}

/// A pill that toggles an extension: soft accent when on, a line border when off.
private struct PillStyle: ButtonStyle {
    let on: Bool

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(AppFont.label)
            .foregroundStyle(on ? Tokens.accent : Tokens.ink)
            .frame(maxWidth: .infinity)
            .frame(height: 36)
            .background(Capsule().fill(on ? Tokens.accentSoft : Color.clear))
            .overlay(Capsule().strokeBorder(on ? Tokens.accent : Tokens.line, lineWidth: 1))
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

/// The confirm button of the stop panel: warn fill, light text.
private struct StopRunStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(AppFont.button)
            .foregroundStyle(Tokens.onAccent)
            .frame(maxWidth: .infinity)
            .frame(height: Metrics.buttonHeight)
            .background(RoundedRectangle(cornerRadius: Metrics.buttonRadius).fill(Tokens.warn))
            .opacity(isEnabled ? (configuration.isPressed ? 0.7 : 1) : 0.5)
    }
}

/// The small accent button on a run that waits on the person.
private struct AnswerButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(AppFont.label)
            .foregroundStyle(Tokens.onAccent)
            .padding(.horizontal, 14)
            .frame(height: 32)
            .background(RoundedRectangle(cornerRadius: 8).fill(Tokens.accent))
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

/// The Status tab: a card for each planner with its executors.
struct StatusView: View {
    let session: Session
    let model: StatusModel
    let openApprovals: () -> Void

    var body: some View {
        Screen(
            errors: [model.loadError, model.actionError].compactMap { $0 },
            refresh: { await model.load(session.client) }
        ) {
            VStack(alignment: .leading, spacing: 14) {
                if model.groups.isEmpty && model.loaded {
                    PlainCard {
                        Text("Nothing is running.").font(AppFont.note)
                            .foregroundStyle(Tokens.muted)
                    }
                }
                ForEach(model.groups) { group in
                    PlannerCard(
                        group: group, model: model, session: session,
                        openApprovals: openApprovals)
                }
            }
        }
        .navigationTitle("Status")
        .navigationBarTitleDisplayMode(.large)
        .task { await model.load(session.client) }
    }
}

/// One planner's card: its lines, its executors and its actions.
private struct PlannerCard: View {
    let group: PlannerGroup
    let model: StatusModel
    let session: Session
    let openApprovals: () -> Void
    @State private var panel: StatusPanel?
    @State private var noteText = ""
    @State private var addCost = true
    @State private var addTurns = false
    @State private var addTime = false
    @State private var toast: String?

    private var planner: RunInfo { group.planner }
    private var stopped: Bool { planner.state == "stopped" }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            plannerLines
            if !group.executors.isEmpty && !stopped { executors }
            if planner.isLive && !stopped {
                actions.padding(.top, 2)
                switch panel {
                case .note: notePanel
                case .extend: extendPanel
                case .stop: stopPanel
                case nil: EmptyView()
                }
            }
            if let toast { Toast(toast) }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .block(radius: 16)
    }

    // MARK: Planner

    /// Title and time, state line, budget line.
    private var plannerLines: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(planner.title).font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
                    .frame(maxWidth: .infinity, alignment: .leading)
                VStack(alignment: .trailing, spacing: 6) {
                    Text(elapsedText(planner.elapsedMs))
                        .font(.system(size: 13)).monospacedDigit().foregroundStyle(Tokens.muted)
                    if planner.waitsOnPerson {
                        Button("Answer", action: openApprovals).buttonStyle(AnswerButtonStyle())
                    }
                }
            }
            stateText(
                planner, dot: 8, byYou: stopped && model.stoppedHere.contains(planner.id),
                extra: nil
            )
            .font(AppFont.body).foregroundStyle(Tokens.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
            budgetLine
        }
    }

    /// "Planner · 34% of $25 budget", with the percent in warn at 80% or more.
    private var budgetLine: some View {
        let line: Text
        if let share = budgetShare(planner), let budget = planner.budget {
            let high = share >= 0.8
            line =
                Text("Planner · ")
                + Text("\(Int((share * 100).rounded()))%").fontWeight(.semibold)
                .foregroundColor(high ? Tokens.warn : Tokens.ink)
                + Text(" of \(money(budget)) budget")
        } else if let spent = planner.spent {
            line = Text("Planner · \(money(spent)) spent")
        } else {
            line = Text("Planner")
        }
        return line.font(.system(size: 13)).monospacedDigit().foregroundStyle(Tokens.muted)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: Executors

    /// The rule, the label and one row for each executor.
    private var executors: some View {
        VStack(alignment: .leading, spacing: 0) {
            Rectangle().fill(Tokens.line).frame(height: 1)
            Text("Executors · \(group.executors.count)")
                .font(AppFont.label).foregroundStyle(Tokens.muted)
                .padding(.top, 10)
            ForEach(Array(group.executors.enumerated()), id: \.element.id) { i, e in
                if i > 0 { Rectangle().fill(Tokens.line).frame(height: 1) }
                executorRow(e)
            }
        }
        .padding(.top, 2)
    }

    /// One executor: title and time, then state, step and budget share.
    private func executorRow(_ e: RunInfo) -> some View {
        var extra: String?
        if let share = budgetShare(e), let budget = e.budget {
            extra = "\(Int((share * 100).rounded()))% of \(money(budget))"
        }
        return HStack(alignment: .center, spacing: 8) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline) {
                    Text(e.title).font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(Tokens.ink)
                    Spacer(minLength: 8)
                    Text(elapsedText(e.elapsedMs))
                        .font(.system(size: 13)).monospacedDigit().foregroundStyle(Tokens.muted)
                }
                stateText(e, dot: 7, byYou: false, extra: extra)
                    .font(.system(size: 13)).monospacedDigit().foregroundStyle(Tokens.muted)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if e.waitsOnPerson {
                Button("Answer", action: openApprovals).buttonStyle(AnswerButtonStyle())
            }
        }
        .padding(.vertical, 10)
    }

    // MARK: Actions

    /// Opens `next`, or closes it when it is already open.
    private func toggle(_ next: StatusPanel) {
        toast = nil
        panel = panel == next ? nil : next
    }

    /// Note, Extend and Stop in three equal columns.
    private var actions: some View {
        HStack(spacing: 8) {
            Button("Note") { toggle(.note) }.buttonStyle(ActionButtonStyle(.secondary))
            Button("Extend") { toggle(.extend) }.buttonStyle(ActionButtonStyle(.secondary))
            Button("Stop") { toggle(.stop) }.buttonStyle(ActionButtonStyle(.destructive))
        }
        .disabled(model.busy)
    }

    /// Runs a write tool; on success shows `done` and closes the panel.
    private func run(_ tool: String, _ args: [String: Any], done: String) {
        Task {
            if await model.act(tool, args, session.client) {
                toast = done
                panel = nil
            }
        }
    }

    /// The extend panel: three toggles and the button that sends them.
    private var extendPanel: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                pill("+$10", $addCost)
                pill("+50 turns", $addTurns)
                pill("+1 h", $addTime)
            }
            Button("Extend budget") {
                var args: [String: Any] = ["run": planner.id]
                if addCost { args["cost_usd"] = 10 }
                if addTurns { args["turns"] = 50 }
                if addTime { args["wall_min"] = 60 }
                run("ai_agents_extend", args, done: "Budget extended.")
            }
            .buttonStyle(ActionButtonStyle(.primary))
            .disabled(!(addCost || addTurns || addTime) || model.busy)
        }
    }

    /// One toggle pill.
    private func pill(_ title: String, _ on: Binding<Bool>) -> some View {
        Button(title) { on.wrappedValue.toggle() }
            .buttonStyle(PillStyle(on: on.wrappedValue))
    }

    /// The stop panel: a question and two buttons.
    private var stopPanel: some View {
        let n = group.executors.count
        let text =
            n > 0
            ? "Stop this run and its \(n) executor\(n == 1 ? "" : "s")? Branches are kept."
            : "Stop this run? Branches are kept."
        return VStack(alignment: .leading, spacing: 8) {
            Text(text).font(AppFont.body).foregroundStyle(Tokens.ink)
            HStack(spacing: 8) {
                Button("Stop run") {
                    run("ai_agents_stop", ["run": planner.id], done: "Stopped.")
                }
                .buttonStyle(StopRunStyle())
                .disabled(model.busy)
                Button("Cancel") { panel = nil }.buttonStyle(ActionButtonStyle(.secondary))
            }
        }
    }

    /// The note panel: a text field and a Send button.
    private var notePanel: some View {
        HStack(spacing: 8) {
            TextField("Read on its next turn", text: $noteText)
                .font(.system(size: 15))
                .padding(.horizontal, 10)
                .frame(minHeight: 40)
                .background(RoundedRectangle(cornerRadius: 10).fill(Tokens.background))
                .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Tokens.line, lineWidth: 1))
            Button("Send") {
                let text = noteText.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !text.isEmpty else { return }
                noteText = ""
                run("ai_agents_note", ["run": planner.id, "text": text], done: "Note sent.")
            }
            .buttonStyle(ActionButtonStyle(.primary, fill: false))
            .disabled(model.busy)
        }
    }
}
