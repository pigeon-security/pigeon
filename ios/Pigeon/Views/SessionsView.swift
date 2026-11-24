import SwiftUI

/// One run, read from `ai_agents_status`. Every field is optional on the wire.
struct RunInfo: Identifiable {
    let id: String
    /// The short human name: `name`, else `title`, else "Untitled run". Never the run id.
    let name: String
    let role: String
    let state: String
    let step: String?
    let parent: String?
    let createdMs: Double
    let endedMs: Double?
    /// How long it has run (ms): the server's `elapsed_ms`, else worked out from its times.
    let elapsedMs: Double
    /// What the run and its executors have spent, in dollars.
    let spent: Double?
    /// The run's cost budget, in dollars.
    let budget: Double?
    /// The run waits for an answer or an approval from the person.
    let waitsOnPerson: Bool

    /// The states of a run that has ended.
    static let endedStates = ["done", "failed", "stopped", "over_budget"]

    /// Whether the run has not ended.
    var isLive: Bool { !Self.endedStates.contains(state) }

    /// Reads one run object; nil when it has no id.
    init?(_ json: [String: Any]) {
        guard let id = json["run"] as? String else { return nil }
        self.id = id
        name = runName(json)
        role = json["role"] as? String ?? "executor"
        state = json["state"] as? String ?? "unknown"
        let step = json["step"] as? String
        self.step = (step?.isEmpty == false) ? step : nil
        parent = json["parent_run"] as? String
        createdMs = num(json["created_at"]) ?? nowMs()
        endedMs = num(json["ended_at"])
        let start = num(json["started_at"]) ?? createdMs
        elapsedMs = num(json["elapsed_ms"]) ?? max(0, (endedMs ?? nowMs()) - start)
        let spent = json["spent"] as? [String: Any]
        let own = parseCost(spent?["cost_usd"])
        let kids = parseCost(spent?["executors_cost_usd"])
        if own != nil || kids != nil {
            self.spent = (own ?? 0) + (kids ?? 0)
        } else {
            self.spent = parseCost(spent?["cost"])
        }
        let budget = json["budget"] as? [String: Any]
        self.budget = parseCost(budget?["budget_cost_usd"]) ?? parseCost(budget?["cost"])
        let kind = (json["waiting"] as? [String: Any])?["for"] as? String
        waitsOnPerson = state == "waiting" && (kind == "answer" || kind == "approval")
    }
}

/// A planner and its executors, drawn as one card. A lone executor has no executors.
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
final class SessionsModel {
    private(set) var runs: [RunInfo] = []
    private(set) var paused = false
    private(set) var loaded = false
    private(set) var loadError: String?
    private(set) var actionError: String?
    private(set) var busy = false

    /// Fetches the last 100 runs.
    func load(_ client: MCPClient) async {
        do {
            let data = try await client.call("ai_agents_status", ["limit": 100])
            guard let dict = data as? [String: Any], let rows = dict["runs"] as? [[String: Any]]
            else { throw PlatformError.badResponse("ai_agents_status shape") }
            paused = dict["paused"] as? Bool ?? false
            runs = rows.compactMap(RunInfo.init)
            loaded = true
            loadError = nil
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            loadError = "ai_agents_status failed: \(error.localizedDescription)"
        }
    }

    /// The live work: a live planner with all its executors, and each live lone executor. A card
    /// with a run that waits on the person sorts first, then the newest.
    var cards: [PlannerGroup] {
        let livePlanners = Set(runs.filter { $0.role == "planner" && $0.isLive }.map(\.id))
        var out: [PlannerGroup] = []
        for r in runs where r.isLive {
            if r.role == "planner" {
                let kids = runs.filter { $0.parent == r.id && $0.id != r.id }
                out.append(PlannerGroup(planner: r, executors: kids.sorted { $0.createdMs < $1.createdMs }))
            } else if !livePlanners.contains(r.parent ?? "") {
                out.append(PlannerGroup(planner: r, executors: []))
            }
        }
        return out.sorted {
            if $0.waitsOnPerson != $1.waitsOnPerson { return $0.waitsOnPerson }
            return $0.planner.createdMs > $1.planner.createdMs
        }
    }

    /// The runs that ended today, newest first.
    var finishedToday: [RunInfo] {
        let start = Calendar.current.startOfDay(for: Date()).timeIntervalSince1970 * 1000
        return runs.filter { !$0.isLive && ($0.endedMs ?? 0) >= start }
            .sorted { ($0.endedMs ?? 0) > ($1.endedMs ?? 0) }
    }

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
private enum SessionPanel {
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
    return (r.state.prefix(1).uppercased() + r.state.dropFirst()).replacingOccurrences(of: "_", with: " ")
}

/// A chip with a run's state: green when running, orange when it needs the person or failed.
private func stateChip(_ r: RunInfo) -> Chip {
    if r.waitsOnPerson || r.state == "failed" || r.state == "over_budget" {
        return Chip(stateWord(r), colour: Tokens.warn, fill: Tokens.chip)
    }
    if r.state == "running" { return Chip(stateWord(r), colour: Tokens.good, fill: Tokens.chip) }
    return Chip(stateWord(r))
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

/// The Sessions tab: the Needs you card, the live work, and today's finished sessions.
struct SessionsView: View {
    let session: Session
    let sessions: SessionsModel
    let needs: NeedsYouModel

    var body: some View {
        Screen(
            title: "Sessions", trailing: AnyView(InstanceSwitcher()), root: true,
            errors: [sessions.loadError, sessions.actionError].compactMap { $0 },
            refresh: {
                await sessions.load(session.client)
                await needs.load(session.client)
            }
        ) {
            NavigationLink {
                NeedsYouView(session: session, model: needs)
            } label: {
                needsCard
            }
            .buttonStyle(.plain)
            CardSection("Running now") {
                VStack(alignment: .leading, spacing: 14) {
                    if sessions.cards.isEmpty && sessions.loaded {
                        PlainCard {
                            Text("Nothing is running.").font(AppFont.note)
                                .foregroundStyle(Tokens.muted)
                        }
                    }
                    ForEach(sessions.cards) { group in
                        PlannerCard(group: group, model: sessions, session: session)
                    }
                }
            }
            let finished = sessions.finishedToday
            if !finished.isEmpty {
                NavigationLink {
                    FinishedTodayView(model: sessions, session: session)
                } label: {
                    HStack {
                        Text("\(finished.count) finished session\(finished.count == 1 ? "" : "s") today")
                            .font(AppFont.body).foregroundStyle(Tokens.accent)
                        Image(systemName: "chevron.right")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(Tokens.accent)
                        Spacer(minLength: 0)
                    }
                    .padding(.leading, Metrics.sectionIndent)
                    .frame(minHeight: 36)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
        .task {
            await sessions.load(session.client)
            await needs.load(session.client)
        }
    }

    /// The top card: how many things need the person, or that all is caught up.
    private var needsCard: some View {
        let n = needs.count
        return PlainCard(radius: Metrics.cardRadiusLarge) {
            HStack(spacing: 12) {
                if n > 0 {
                    CountBadge(count: n)
                } else {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 30)).foregroundStyle(Tokens.good)
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text(n == 0 ? "You're all caught up" : n == 1 ? "One thing needs your call" : "\(n) things need your call")
                        .font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
                    Text(n == 0 ? "Your agents have what they need" : needsSubline)
                        .font(AppFont.meta).foregroundStyle(Tokens.muted)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 8)
                Chevron()
            }
        }
    }

    /// "1 question · 1 decision to check · 2 approvals"; parts at zero are left out.
    private var needsSubline: String {
        var parts: [String] = []
        let q = needs.questions.count
        let d = needs.decisions.count
        let a = needs.approvals.count
        if q > 0 { parts.append("\(q) question\(q == 1 ? "" : "s")") }
        if d > 0 { parts.append("\(d) decision\(d == 1 ? "" : "s") to check") }
        if a > 0 { parts.append("\(a) approval\(a == 1 ? "" : "s")") }
        return parts.joined(separator: " · ")
    }
}

/// A simple list of the runs that ended today: name, state and cost.
struct FinishedTodayView: View {
    let model: SessionsModel
    let session: Session

    var body: some View {
        Screen(title: "Finished today", errors: [model.loadError].compactMap { $0 },
               refresh: { await model.load(session.client) }) {
            let runs = model.finishedToday
            if runs.isEmpty { Footnote("No sessions finished today.") }
            GroupedCard {
                ForEach(Array(runs.enumerated()), id: \.element.id) { index, r in
                    if index > 0 { RowSeparator(inset: 16) }
                    HStack(spacing: 10) {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(r.name).font(AppFont.row).foregroundStyle(Tokens.ink)
                            stateChip(r)
                        }
                        Spacer(minLength: 8)
                        Text(r.spent.map { money($0) } ?? "")
                            .font(AppFont.body).monospacedDigit().foregroundStyle(Tokens.muted)
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                    .frame(minHeight: 56)
                }
            }
        }
    }
}

/// One planner's card (or a lone executor's): lines, budget bar, executors and actions.
private struct PlannerCard: View {
    let group: PlannerGroup
    let model: SessionsModel
    let session: Session
    @State private var panel: SessionPanel?
    @State private var noteText = ""
    @State private var addCost = true
    @State private var addTurns = false
    @State private var addTime = false
    @State private var toast: String?

    private var planner: RunInfo { group.planner }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(planner.name).font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Text(elapsedText(planner.elapsedMs))
                    .font(.system(size: 13)).monospacedDigit().foregroundStyle(Tokens.muted)
            }
            HStack(spacing: 8) {
                stateChip(planner)
                if let step = planner.step {
                    Text(step).font(AppFont.meta).foregroundStyle(Tokens.muted).lineLimit(1)
                }
            }
            budgetBar
            if !group.executors.isEmpty { executors }
            actions.padding(.top, 2)
            switch panel {
            case .note: notePanel
            case .extend: extendPanel
            case .stop: stopPanel
            case nil: EmptyView()
            }
            if let toast { Toast(toast) }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .block(radius: 16)
    }

    // MARK: Budget

    /// The bar and "$X of $Y budget · n of m executors finished".
    @ViewBuilder
    private var budgetBar: some View {
        let spent = planner.spent ?? 0
        let m = group.executors.count
        let n = group.executors.filter { !$0.isLive }.count
        let tail = m > 0 ? " · \(n) of \(m) executors finished" : ""
        if let budget = planner.budget, budget > 0 {
            let share = min(1, max(0, spent / budget))
            VStack(alignment: .leading, spacing: 6) {
                GeometryReader { geo in
                    ZStack(alignment: .leading) {
                        Capsule().fill(Tokens.chip)
                        Capsule().fill(share >= 0.8 ? Tokens.warn : Tokens.accent)
                            .frame(width: geo.size.width * share)
                    }
                }
                .frame(height: 6)
                Text("\(money(spent)) of \(money(budget)) budget\(tail)")
                    .font(.system(size: 13)).monospacedDigit().foregroundStyle(Tokens.muted)
            }
        } else {
            Text("\(money(spent)) spent\(tail)")
                .font(.system(size: 13)).monospacedDigit().foregroundStyle(Tokens.muted)
        }
    }

    // MARK: Executors

    /// The rule, the label and one row for each executor; ended ones are dimmed.
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

    /// One executor: name and time, then its state chip and cost.
    private func executorRow(_ e: RunInfo) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline) {
                Text(e.name).font(.system(size: 15, weight: .semibold)).foregroundStyle(Tokens.ink)
                Spacer(minLength: 8)
                Text(elapsedText(e.elapsedMs))
                    .font(.system(size: 13)).monospacedDigit().foregroundStyle(Tokens.muted)
            }
            HStack(spacing: 8) {
                stateChip(e)
                if let spent = e.spent {
                    Text(money(spent)).font(.system(size: 13)).monospacedDigit()
                        .foregroundStyle(Tokens.muted)
                }
            }
        }
        .padding(.vertical, 10)
        .opacity(e.isLive ? 1 : 0.55)
    }

    // MARK: Actions

    /// Opens `next`, or closes it when it is already open.
    private func toggle(_ next: SessionPanel) {
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
