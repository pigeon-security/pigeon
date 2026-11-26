import SwiftUI

/// Sessions (platform-wide, home only): Live and Compare, as the web console's `sessions.js`.
///
/// - Live: live runs as a tree (planners, then their executors), with budget meters; the landing
///   queue; and what waits on the person. Reads `ai_agents_status` (live, then recent) and
///   `land_list`. A run's sheet sends a note (`run-note`), extends it (`run-extend`) or stops it
///   (`run-stop`), each through the confirm sheet.
/// - Compare: ended runs by model, role and kind (`ai_agents_compare`).
///
/// "Pause all agents" is not here (the person's decision A4).
struct SessionsScreen: View {
    let context: AreaContext

    var body: some View {
        switch context.viewID.wrappedValue {
        case "compare": SessionsCompare(context: context)
        default: SessionsLive(context: context)
        }
    }
}

// MARK: - Model

extension SessionsScreen {
    /// One run of `ai_agents_status`.
    struct Run: Identifiable, Hashable {
        let id: String
        let name: String
        let state: String
        let role: String
        let model: String?
        let step: String?
        let runtime: String?
        let parentRun: String?
        let createdAt: Double
        let elapsedMs: Double?
        let stopAsked: Bool
        let waitingFor: String?
        let ownSpent: Double?
        let executorsSpent: Double?
        let budget: Double?
        var children: [Run] = []

        init?(_ j: JSON) {
            guard let id = j["run"].string, !id.isEmpty else { return nil }
            self.id = id
            let given = j["name"].string ?? j["title"].string ?? ""
            name = given.trimmingCharacters(in: .whitespaces).isEmpty ? "Untitled run" : given
            state = j["state"].string ?? "unknown"
            role = j["role"].string ?? ""
            model = j["model"].string
            step = j["step"].string
            runtime = j["runtime"].string
            parentRun = j["parent_run"].string
            createdAt = j["created_at"].double ?? 0
            elapsedMs = j["elapsed_ms"].double
            stopAsked = j["stop_asked"].bool ?? false
            waitingFor = j["waiting"]["for"].string
            ownSpent = j["spent"]["cost_usd"].double
            executorsSpent = j["spent"]["executors_spent_usd"].double
            budget = j["budget"]["budget_cost_usd"].double
        }

        static let liveStates: Set<String> = ["queued", "preparing", "running", "waiting"]
        static let endedStates: Set<String> = ["done", "failed", "stopped", "over_budget"]

        var isLive: Bool { Run.liveStates.contains(state) }
        var waitsOnYou: Bool { state == "waiting" && (waitingFor == "approval" || waitingFor == "answer") }
        /// A planner's spend includes what its executors spent.
        var spent: Double? {
            guard let own = ownSpent else { return nil }
            return role == "planner" ? own + (executorsSpent ?? 0) : own
        }
        /// Whether the run takes a note, an extension or a stop.
        var canAct: Bool {
            id.range(of: "^[A-Za-z0-9_-]{1,64}$", options: .regularExpression) != nil
                && !stopAsked && isLive
        }
        /// Only a gateway run reads notes and takes extensions.
        var isGateway: Bool { runtime == "gateway" }

        var stateText: String {
            if stopAsked && isLive { return "Stop requested" }
            if waitsOnYou { return "Waiting on you" }
            return PlatformFormat.word(state)
        }

        var stateTone: Tone {
            if stopAsked && isLive { return .neutral }
            if waitsOnYou { return .warn }
            switch state {
            case "preparing", "running", "waiting": return .info
            case "done": return .ok
            case "failed", "over_budget": return .bad
            default: return .neutral
            }
        }

        /// The facts at the top of a run's confirm sheet.
        var facts: [Fact] {
            [Fact("Run", name), Fact("Id", id, mono: true), Fact("Role", PlatformFormat.word(role))]
        }

        /// The live runs as a tree: planners with their executors, and executors whose planner
        /// is not live. Runs that wait on the person first, then the newest.
        static func tree(live: [Run], recent: [Run]?) -> [Run] {
            var all: [String: Run] = [:]
            var order: [String] = []
            for r in recent ?? [] {
                if all[r.id] == nil { order.append(r.id) }
                all[r.id] = r
            }
            for r in live {
                if let known = all[r.id], endedStates.contains(known.state) { continue }
                if all[r.id] == nil { order.append(r.id) }
                all[r.id] = r
            }
            let values = order.compactMap { all[$0] }
            let running = values.filter { $0.isLive }
            let planners = Set(running.filter { $0.role == "planner" }.map { $0.id })
            let roots: [Run] = running
                .filter { $0.role == "planner" || !planners.contains($0.parentRun ?? "") }
                .map { root in
                    var r = root
                    if r.role == "planner" {
                        r.children = values
                            .filter { $0.id != r.id && $0.parentRun == r.id }
                            .sorted { $0.createdAt < $1.createdAt }
                    }
                    return r
                }
            return roots.sorted { a, b in
                let wa = a.waitsOnYou || a.children.contains { $0.waitsOnYou }
                let wb = b.waitsOnYou || b.children.contains { $0.waitsOnYou }
                if wa != wb { return wa }
                return a.createdAt > b.createdAt
            }
        }
    }

    /// One landing of `land_list`.
    struct QueuedLanding: Identifiable, Hashable {
        let id: String
        let branch: String?
        let commit: String?
        let state: String
        let workers: [String]
        let updatedAt: Double?

        init?(_ j: JSON) {
            guard let id = j["id"].string ?? j["id"].text else { return nil }
            self.id = id
            branch = j["branch"].string
            commit = j["commit"].string
            state = j["state"].string ?? "unknown"
            workers = j["workers"].items.compactMap { $0.string }
            updatedAt = j["updated_at"].double
        }

        var awaitsApproval: Bool { state == "awaiting_approval" }

        var stateText: String {
            switch state {
            case "awaiting_approval": return "Awaiting approval"
            default: return PlatformFormat.word(state)
            }
        }

        var stateTone: Tone {
            switch state {
            case "awaiting_approval": return .warn
            case "merged": return .ok
            case "queued": return .neutral
            case "checking", "testing", "ready", "merging", "deploying": return .info
            default: return .neutral
            }
        }
    }
}

// MARK: - Live

struct SessionsLive: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @Environment(\.openArea) private var openArea
    @State private var runs: Loadable<[SessionsScreen.Run]> = .loading
    @State private var landings: Loadable<[SessionsScreen.QueuedLanding]> = .loading
    @State private var paused = false
    @State private var selected: SessionsScreen.Run?

    var body: some View {
        List {
            PlatformViewPicker(context: context)
            waitingSection
            LoadableSection(
                state: runs, emptyTitle: "No live runs",
                emptyMessage: "Runs that are queued, preparing, running or waiting show here.",
                isEmpty: { $0.isEmpty }, retry: { await loadRuns() }
            ) { list in
                Section {
                    ForEach(list) { run in
                        runButton(run, child: false)
                        ForEach(run.children) { child in runButton(child, child: true) }
                    }
                } header: {
                    Text("Live runs")
                } footer: {
                    Text("Planners and their executors. Tap a run to send a note, extend or stop it.")
                }
            }
            LoadableSection(
                state: landings, emptyTitle: "No landings in the queue",
                isEmpty: { $0.isEmpty }, retry: { await loadLandings() }
            ) { list in
                Section("Landing queue") {
                    ForEach(list) { landingRow($0) }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await reload() }
        .task { await reload() }
        .sheet(item: $selected) { run in
            RunActionsSheet(run: run) { Task { await loadRuns() } }
        }
    }

    // MARK: Waiting

    @ViewBuilder
    private var waitingSection: some View {
        if let list = runs.value, let queue = landings.value {
            let waitingRuns = list.flatMap { [$0] + $0.children }.filter { $0.waitsOnYou }.count
            let waitingLandings = queue.filter { $0.awaitsApproval }.count
            let total = waitingRuns + waitingLandings
            Section {
                if total > 0 {
                    HStack(spacing: 10) {
                        VStack(alignment: .leading, spacing: 1) {
                            Text(total == 1 ? "1 request waits on you" : "\(total) requests wait on you")
                                .font(Theme.Fonts.body.weight(.semibold))
                                .foregroundStyle(Theme.accent)
                            Text(waitingDetail(runs: waitingRuns, landings: waitingLandings))
                                .font(Theme.Fonts.meta).foregroundStyle(Theme.ink2)
                        }
                        Spacer(minLength: 8)
                        Button("Open Inbox") { openArea("inbox") }
                            .buttonStyle(RowButtonStyle(.primary))
                    }
                    .listRowBackground(Theme.accentSoft)
                } else {
                    Text(paused ? "New runs are paused. Nothing waits on you here." : "Nothing here waits on you.")
                        .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                }
            }
        }
    }

    // MARK: Rows

    private func runButton(_ run: SessionsScreen.Run, child: Bool) -> some View {
        Button {
            selected = run
        } label: {
            RunRow(run: run, child: child).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private func landingRow(_ l: SessionsScreen.QueuedLanding) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text(l.branch ?? l.id).idStyle(Theme.Fonts.idBody).foregroundStyle(Theme.ink)
                Text(landingDetail(l))
                    .font(Theme.Fonts.meta).foregroundStyle(Theme.muted).lineLimit(1)
            }
            Spacer(minLength: 8)
            if l.awaitsApproval {
                Button {
                    openArea("inbox")
                } label: {
                    StatusPill("Approval", tone: .warn)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Awaiting approval. Review in Inbox")
            } else {
                StatusPill(l.stateText, tone: l.stateTone)
            }
        }
        .frame(minHeight: 32)
    }

    private func waitingDetail(runs: Int, landings: Int) -> String {
        var parts: [String] = []
        if runs > 0 { parts.append(PlatformFormat.count(runs, "run", "runs")) }
        if landings > 0 { parts.append(PlatformFormat.count(landings, "landing", "landings")) }
        return parts.joined(separator: ", ")
    }

    private func landingDetail(_ l: SessionsScreen.QueuedLanding) -> String {
        var parts: [String] = []
        if !l.workers.isEmpty { parts.append(l.workers.joined(separator: ", ")) }
        if let at = l.updatedAt { parts.append(timestampText(at)) }
        return parts.joined(separator: " · ")
    }

    // MARK: Loading

    private func reload() async {
        await loadRuns()
        await loadLandings()
    }

    /// The live runs, then the recent ones (so a planner shows the executors that ended); a
    /// failure of the second read only loses those.
    private func loadRuns() async {
        do {
            let now = try await api.op(
                "ai_agents_status", args: ["limit": 100, "brief": false, "live": true])
            let recent = try? await api.op("ai_agents_status", args: ["limit": 100, "brief": false])
            let live = now["runs"].items.compactMap { SessionsScreen.Run($0) }
            let past = recent.map { r in r["runs"].items.compactMap { SessionsScreen.Run($0) } }
            paused = now["paused"].bool == true
            runs = .loaded(SessionsScreen.Run.tree(live: live, recent: past))
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            if runs.value == nil { runs = .failed(error.localizedDescription) }
        }
    }

    private func loadLandings() async {
        do {
            let list = try await api.op("land_list", args: ["state": "live", "limit": 50])
            landings = .loaded(list.items.compactMap { SessionsScreen.QueuedLanding($0) })
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            if landings.value == nil { landings = .failed(error.localizedDescription) }
        }
    }
}

/// A run in the Live list: name, state, role and model, step; spend against budget.
private struct RunRow: View {
    let run: SessionsScreen.Run
    let child: Bool

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            if child {
                Image(systemName: "arrow.turn.down.right")
                    .font(.caption)
                    .foregroundStyle(Theme.lineStrong)
                    .padding(.top, 3)
                    .accessibilityHidden(true)
            }
            VStack(alignment: .leading, spacing: 3) {
                Text(run.name)
                    .font(Theme.Fonts.body)
                    .foregroundStyle(Theme.ink)
                    .lineLimit(2)
                HStack(spacing: 6) {
                    StatusPill(run.stateText, tone: run.stateTone)
                    Text([PlatformFormat.word(run.role), run.model].compactMap { $0 }.joined(separator: " · "))
                        .idStyle()
                        .foregroundStyle(Theme.muted)
                }
                if let step = run.step, !step.isEmpty {
                    Text(step).font(Theme.Fonts.meta).foregroundStyle(Theme.ink2).lineLimit(1)
                }
            }
            Spacer(minLength: 8)
            VStack(alignment: .trailing, spacing: 4) {
                Text(PlatformFormat.money(run.spent))
                    .font(Theme.Fonts.id).foregroundStyle(Theme.ink)
                if let budget = run.budget, budget > 0 {
                    BudgetMeter(fraction: (run.spent ?? 0) / budget)
                    Text("of \(PlatformFormat.money(budget))")
                        .font(Theme.Fonts.caption).foregroundStyle(Theme.muted)
                }
                Text(PlatformFormat.duration(run.elapsedMs))
                    .font(Theme.Fonts.caption).foregroundStyle(Theme.muted)
            }
        }
        .padding(.leading, child ? 8 : 0)
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }
}

// MARK: - Run sheet

/// A run's actions: a note and an extension (gateway runs only), and stop. Each step hands over
/// to the confirm sheet in the same sheet.
private struct RunActionsSheet: View {
    let run: SessionsScreen.Run
    let onDone: () -> Void

    private enum Step { case menu, note, extend }

    @State private var step: Step = .menu
    @State private var request: WriteRequest?
    @State private var note = ""
    @State private var addBudget = true
    @State private var addTurns = false
    @State private var addWall = false
    @State private var problem: String?
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        if let request {
            ConfirmSheet(request)
        } else {
            switch step {
            case .menu: menu
            case .note: noteForm
            case .extend: extendForm
            }
        }
    }

    private var meta: String {
        var parts = [run.id, PlatformFormat.word(run.role), PlatformFormat.duration(run.elapsedMs)]
        if let budget = run.budget {
            parts.append("\(PlatformFormat.money(run.spent)) of \(PlatformFormat.money(budget))")
        } else if run.spent != nil {
            parts.append(PlatformFormat.money(run.spent))
        }
        return parts.joined(separator: " · ")
    }

    private var menu: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(run.name).font(.title3.weight(.bold)).foregroundStyle(Theme.ink)
                    Text(meta).idStyle().foregroundStyle(Theme.muted)
                    HStack(spacing: 6) {
                        StatusPill(run.stateText, tone: run.stateTone)
                        if let model = run.model { IDText(model) }
                    }
                    if let current = run.step, !current.isEmpty {
                        Text(current).font(Theme.Fonts.meta).foregroundStyle(Theme.ink2)
                    }
                }
                if run.canAct {
                    if run.isGateway {
                        VStack(spacing: 0) {
                            actionRow("Add a note", "The run reads it at its next turn.", "text.bubble") {
                                problem = nil
                                step = .note
                            }
                            Divider().padding(.leading, 14)
                            actionRow("Extend", "Raise its budget, turns or wall time.", "plus.circle") {
                                problem = nil
                                step = .extend
                            }
                        }
                        .background(Theme.raised, in: RoundedRectangle(cornerRadius: 12))
                    }
                    VStack(spacing: 10) {
                        Button("Stop run…") { request = stopRequest }
                            .buttonStyle(SheetButtonStyle(.danger))
                        Button("Close") { dismiss() }
                            .buttonStyle(SheetButtonStyle(.quiet))
                    }
                } else {
                    Text(
                        run.stopAsked
                            ? "A stop has been requested for this run."
                            : "This run is not live, so it takes no note, extension or stop."
                    )
                    .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                    Button("Close") { dismiss() }
                        .buttonStyle(SheetButtonStyle(.quiet))
                }
            }
            .padding(.horizontal, 20)
            .padding(.top, 24)
            .padding(.bottom, 16)
        }
        .scrollBounceBehavior(.basedOnSize)
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }

    private func actionRow(
        _ title: String, _ subtitle: String, _ symbol: String, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: symbol)
                    .foregroundStyle(Theme.accent)
                    .frame(width: 22)
                    .accessibilityHidden(true)
                ListRow(title: title, subtitle: subtitle) {
                    Image(systemName: "chevron.right")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(Theme.lineStrong)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private var noteForm: some View {
        ReviewForm(
            title: "Send a note", subtitle: run.name, problem: problem, reviewLabel: "Review note",
            review: { reviewNote() }
        ) {
            FormField(label: "Note for the next turn", hint: "1 to 4,000 bytes. The run reads it at its next turn.") {
                TextEditor(text: $note)
                    .frame(minHeight: 110)
                    .scrollContentBackground(.hidden)
            }
        }
    }

    private var extendForm: some View {
        ReviewForm(
            title: "Extend run",
            subtitle: "Spent \(PlatformFormat.money(run.spent)) of \(PlatformFormat.money(run.budget))",
            problem: problem, reviewLabel: "Review extension", review: { reviewExtend() }
        ) {
            VStack(spacing: 0) {
                Toggle("+$10 of budget", isOn: $addBudget).padding(.vertical, 6)
                Divider()
                Toggle("+50 turns", isOn: $addTurns).padding(.vertical, 6)
                Divider()
                Toggle("+1 hour of wall time", isOn: $addWall).padding(.vertical, 6)
            }
            .font(Theme.Fonts.body)
            .padding(.horizontal, 14)
            .padding(.vertical, 4)
            .background(Theme.raised, in: RoundedRectangle(cornerRadius: 12))
        }
    }

    // MARK: Requests

    private var stopRequest: WriteRequest {
        WriteRequest(
            title: "Stop run",
            subtitle: run.role == "planner"
                ? "Stops this run and its live executors. Branches are kept."
                : "Stops this run. Its branch is kept.",
            facts: run.facts,
            action: "run-stop",
            target: .string(run.id),
            destructive: true,
            confirmLabel: "Stop with passkey",
            onDone: { _ in onDone() })
    }

    private func reviewNote() {
        let text = note.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, text.utf8.count <= 4000 else {
            problem = "Write a note of 1 to 4,000 bytes."
            return
        }
        request = WriteRequest(
            title: "Send a note",
            subtitle: "The run reads it at its next turn.",
            facts: run.facts,
            lines: [.add(text)],
            action: "run-note",
            target: .string(run.id),
            change: ["text": .string(text)],
            confirmLabel: "Send with passkey",
            onDone: { _ in onDone() })
    }

    private func reviewExtend() {
        var change: [String: JSON] = [:]
        var lines: [DiffLine] = []
        if addBudget {
            change["cost_usd"] = 10
            lines.append(.add("budget +$10.00"))
        }
        if addTurns {
            change["turns"] = 50
            lines.append(.add("turns +50"))
        }
        if addWall {
            change["wall_min"] = 60
            lines.append(.add("wall time +60 min"))
        }
        guard !change.isEmpty else {
            problem = "Choose at least one extension."
            return
        }
        request = WriteRequest(
            title: "Extend run",
            subtitle: "Raises this run's limits now. Its spend so far is unchanged.",
            facts: run.facts,
            lines: lines,
            action: "run-extend",
            target: .string(run.id),
            change: .object(change),
            confirmLabel: "Extend with passkey",
            onDone: { _ in onDone() })
    }
}

// MARK: - Compare

struct SessionsCompare: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @Environment(\.openArea) private var openArea
    @State private var days = "7"
    @State private var role = "all"
    @State private var groups: Loadable<[CompareGroup]> = .loading

    private static let roles = ["planner", "executor", "mechanical"]

    /// One group of `ai_agents_compare`: ended runs of one model, role and kind.
    struct CompareGroup: Identifiable {
        let id: Int
        let model: String
        let route: String?
        let role: String
        let kind: String?
        let runs: Int
        let done: Int
        let failed: Int
        let overBudget: Int
        let stopped: Int
        let median: Double?
        let p90: Double?
        let total: Double?
        let turns: Double?
        let minutes: Double?

        init?(_ j: JSON, index: Int) {
            guard let model = j["model"].string else { return nil }
            id = index
            self.model = model
            route = j["route"].string
            role = j["role"].string ?? ""
            kind = j["kind"].string
            runs = j["runs"].int ?? 0
            done = j["done"].int ?? 0
            failed = j["failed"].int ?? 0
            overBudget = j["over_budget"].int ?? 0
            stopped = j["stopped"].int ?? 0
            median = j["cost_usd"]["median"].double
            p90 = j["cost_usd"]["high"].double
            total = j["cost_usd"]["total"].double
            turns = j["turns"]["median"].double
            minutes = j["minutes"]["median"].double
        }
    }

    var body: some View {
        List {
            PlatformViewPicker(context: context)
            Section {
                Picker("Window", selection: $days) {
                    Text("1 day").tag("1")
                    Text("7 days").tag("7")
                    Text("30 days").tag("30")
                }
                .pickerStyle(.segmented)
                Picker("Role", selection: $role) {
                    Text("All roles").tag("all")
                    ForEach(Self.roles, id: \.self) { Text(PlatformFormat.word($0)).tag($0) }
                }
                .font(Theme.Fonts.body)
            } footer: {
                Text("Ended runs only · outcome: done · failed · over budget · stopped")
            }
            LoadableSection(
                state: groups, emptyTitle: "No ended runs",
                emptyMessage: "No ended runs match this window and role.",
                isEmpty: { $0.isEmpty }, retry: { await load() }
            ) { list in
                ForEach(sectionRoles(list), id: \.self) { r in
                    Section(sectionTitle(r)) {
                        ForEach(list.filter { $0.role == r }) { groupRow($0) }
                    }
                }
            }
            Section {
                Button {
                    openArea("platform")
                } label: {
                    Text("Model defaults, planner by kind among them, are set in Platform › Models.")
                        .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                }
                .buttonStyle(.plain)
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task(id: "\(days)|\(role)") { await load() }
    }

    private func sectionRoles(_ list: [CompareGroup]) -> [String] {
        var seen: [String] = []
        for g in list where !seen.contains(g.role) { seen.append(g.role) }
        return seen
    }

    private func sectionTitle(_ role: String) -> String {
        switch role {
        case "planner": return "Planners, by kind"
        case "executor": return "Executors"
        case "mechanical": return "Mechanical runs"
        default: return PlatformFormat.word(role)
        }
    }

    private func groupRow(_ g: CompareGroup) -> some View {
        let runs = max(g.runs, 0)
        let share: (Int) -> Double = { n in runs > 0 ? Double(n) / Double(runs) : 0 }
        let detail = [
            g.kind.map { PlatformFormat.word($0) },
            PlatformFormat.count(g.runs, "run", "runs"),
            PlatformFormat.pays(g.route) == "—" ? nil : PlatformFormat.pays(g.route),
        ].compactMap { $0 }.joined(separator: " · ")
        let said = "Done \(g.done), failed \(g.failed), over budget \(g.overBudget), stopped \(g.stopped)"
        return HStack(alignment: .top, spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                Text(g.model).idStyle(Theme.Fonts.idBody).foregroundStyle(Theme.ink)
                Text(detail).font(Theme.Fonts.meta).foregroundStyle(Theme.muted).lineLimit(1)
                HStack(spacing: 6) {
                    ShareBar(parts: [
                        .init(fraction: share(g.done), color: Theme.ok),
                        .init(fraction: share(g.failed), color: Theme.bad),
                        .init(fraction: share(g.overBudget), color: Theme.warn),
                        .init(fraction: share(g.stopped), color: Theme.muted),
                    ])
                    Text("\(g.done) · \(g.failed) · \(g.overBudget) · \(g.stopped)")
                        .font(Theme.Fonts.caption).foregroundStyle(Theme.muted)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(said)
            }
            Spacer(minLength: 8)
            VStack(alignment: .trailing, spacing: 2) {
                Text(PlatformFormat.money(g.median)).font(Theme.Fonts.id).foregroundStyle(Theme.ink)
                Text("median").font(Theme.Fonts.caption).foregroundStyle(Theme.muted)
                Text("p90 \(PlatformFormat.money(g.p90))").font(Theme.Fonts.caption).foregroundStyle(Theme.muted)
            }
        }
        .padding(.vertical, 2)
    }

    private func load() async {
        var args: [String: JSON] = ["days": .number(Double(days) ?? 7)]
        if role != "all" { args["role"] = .string(role) }
        do {
            let data = try await api.op("ai_agents_compare", args: .object(args))
            let list = data["groups"].items.enumerated().compactMap { CompareGroup($0.element, index: $0.offset) }
            let order: (String) -> Int = { Self.roles.firstIndex(of: $0) ?? Self.roles.count }
            groups = .loaded(
                list.sorted { a, b in
                    if order(a.role) != order(b.role) { return order(a.role) < order(b.role) }
                    if (a.kind ?? "") != (b.kind ?? "") { return (a.kind ?? "") < (b.kind ?? "") }
                    return a.runs > b.runs
                })
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            groups = .failed(error.localizedDescription)
        }
    }
}
