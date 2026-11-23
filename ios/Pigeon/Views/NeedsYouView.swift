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

/// One approval waiting on the person's passkey.
struct Entry: Identifiable {
    let id: String
    let kind: ApprovalKind
    let title: String
    /// One line: who asked, or which run.
    let sub: String
    /// When it expires (ms), if it does.
    let expiresMs: Double?
    /// When it was made (ms), if known.
    let createdMs: Double?
}

/// One labelled row of a question or a decision.
struct RunFact: Identifiable {
    let id: Int
    let label: String
    let value: String
    let mono: Bool
}

/// Reads a `facts` array; anything odd is skipped.
func parseFacts(_ value: Any?) -> [RunFact] {
    let rows = value as? [[String: Any]] ?? []
    var out: [RunFact] = []
    for r in rows {
        guard let label = r["label"] as? String, let v = r["value"] as? String else { continue }
        out.append(RunFact(id: out.count, label: label, value: v, mono: r["mono"] as? Bool ?? false))
    }
    return out
}

/// A run waiting on an answer: what it asks and how the person can reply.
struct Question: Identifiable {
    let run: String
    let name: String
    let headline: String
    let question: String
    let facts: [RunFact]
    let options: [String]
    /// The index of the option the run takes when no answer comes.
    let conventional: Int?
    let sinceMs: Double?
    let answerByMs: Double?
    var id: String { run }

    /// When the run goes on alone: `answer_by`, else five minutes after the question.
    var deadlineMs: Double? { answerByMs ?? sinceMs.map { $0 + 300_000 } }
}

/// A question that timed out: what the run chose for the person.
struct Decision: Identifiable {
    let run: String
    let at: Double
    let name: String
    let branch: String?
    let question: String
    let headline: String?
    let facts: [RunFact]
    let chose: String?
    let instead: [String]
    var id: String { "\(run)|\(Int(at))" }
}

/// Loads `approvals_list` and `ai_agents_inbox`, and decides what waits on the person.
@MainActor
@Observable
final class NeedsYouModel {
    private(set) var questions: [Question] = []
    private(set) var decisions: [Decision] = []
    private(set) var approvals: [Entry] = []
    private(set) var loaded = false
    private(set) var loadError: String?
    private(set) var inboxError: String?
    private(set) var actionError: String?
    private(set) var notice: String?
    private(set) var busy: String?
    @ObservationIgnored private let approver = Approver()
    @ObservationIgnored private static let dismissedKey = "dismissedNotices"

    /// How many things need the person, for the Sessions badge.
    var count: Int { questions.count + decisions.count + approvals.count }

    /// Fetches both lists. A failure in one keeps the other's items.
    func load(_ client: MCPClient) async {
        await loadApprovals(client)
        await loadInbox(client)
        loaded = true
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
            approvals = found
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
            id: id, kind: kind, title: shown, sub: sub,
            expiresMs: num(json["expires_ms"]), createdMs: num(json["created_ms"]))
    }

    /// Fetches the agents' questions and decisions with `ai_agents_inbox`.
    private func loadInbox(_ client: MCPClient) async {
        do {
            let data = try await client.call("ai_agents_inbox", ["since": 0])
            guard let dict = data as? [String: Any] else {
                throw PlatformError.badResponse("ai_agents_inbox shape")
            }
            let dismissed = Set(UserDefaults.standard.stringArray(forKey: Self.dismissedKey) ?? [])
            var asked: [Question] = []
            for run in dict["waiting"] as? [[String: Any]] ?? [] {
                guard let id = run["run"] as? String, let wait = run["waiting"] as? [String: Any],
                    wait["for"] as? String == "answer",
                    let question = wait["question"] as? String
                else { continue }
                let headline = wait["headline"] as? String ?? ""
                var conventional = (wait["conventional"] as? NSNumber)?.intValue
                let options = wait["options"] as? [String] ?? []
                if let c = conventional, c < 0 || c >= options.count { conventional = nil }
                asked.append(
                    Question(
                        run: id, name: runName(run),
                        headline: headline.isEmpty ? question : headline, question: question,
                        facts: parseFacts(wait["facts"]), options: options,
                        conventional: conventional, sinceMs: num(wait["since"]),
                        answerByMs: num(wait["answer_by"])))
            }
            var decided: [Decision] = []
            for note in dict["assumed"] as? [[String: Any]] ?? [] {
                guard let run = note["run"] as? String, let at = num(note["at"]) else { continue }
                if dismissed.contains(Self.mark(run, at)) { continue }
                let headline = note["headline"] as? String
                decided.append(
                    Decision(
                        run: run, at: at, name: runName(note), branch: note["branch"] as? String,
                        question: note["question"] as? String ?? "",
                        headline: (headline?.isEmpty ?? true) ? nil : headline,
                        facts: parseFacts(note["facts"]),
                        chose: note["chose"] as? String,
                        instead: note["instead"] as? [String] ?? []))
            }
            questions = asked
            decisions = decided
            inboxError = nil
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            inboxError = "ai_agents_inbox failed: \(error.localizedDescription)"
        }
    }

    /// The key kept for a dismissed decision.
    private static func mark(_ run: String, _ at: Double) -> String { "\(run)|\(Int(at))" }

    /// Approves (or denies) `item` with a passkey, then reloads.
    func decide(_ item: Entry, approve: Bool, client: MCPClient) async {
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

    /// Sends an answer to an agent's question with `ai_agents_answer`, then reloads.
    func answer(run: String, text: String, client: MCPClient) async {
        await act("ai_agents_answer", ["run": run, "answer": text], client)
    }

    /// Stops a run with `ai_agents_stop`, then reloads.
    func stop(run: String, client: MCPClient) async {
        await act("ai_agents_stop", ["run": run], client)
    }

    /// Sends a note to a run with `ai_agents_note`, then reloads.
    func note(run: String, text: String, client: MCPClient) async {
        await act("ai_agents_note", ["run": run, "text": text], client)
    }

    /// Hides a decision on this device and remembers it.
    func dismiss(_ d: Decision) {
        var kept = UserDefaults.standard.stringArray(forKey: Self.dismissedKey) ?? []
        kept.append(Self.mark(d.run, d.at))
        UserDefaults.standard.set(Array(kept.suffix(200)), forKey: Self.dismissedKey)
        decisions.removeAll { $0.id == d.id }
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

/// The Needs you screen: questions, decisions made for the person, and approvals.
struct NeedsYouView: View {
    let session: Session
    let model: NeedsYouModel

    var body: some View {
        Screen(
            title: "Needs you",
            errors: [model.loadError, model.inboxError, model.actionError].compactMap { $0 },
            refresh: { await model.load(session.client) }
        ) {
            if model.count == 0 && model.loaded {
                PlainCard {
                    Text("You're all caught up").font(AppFont.cardTitle)
                        .foregroundStyle(Tokens.ink)
                    Text("Your agents have what they need")
                        .font(AppFont.note).foregroundStyle(Tokens.muted)
                }
            }
            if !model.questions.isEmpty {
                CardSection("Your call") {
                    VStack(spacing: Metrics.gap) {
                        ForEach(model.questions) { q in
                            QuestionCard(question: q, model: model, session: session)
                        }
                    }
                }
            }
            if !model.decisions.isEmpty {
                CardSection("Decided for you") {
                    VStack(spacing: Metrics.gap) {
                        ForEach(model.decisions) { d in
                            DecisionCard(decision: d, model: model, session: session)
                        }
                    }
                }
            }
            if !model.approvals.isEmpty {
                CardSection("Approvals") {
                    VStack(spacing: Metrics.gap) {
                        ForEach(model.approvals) { item in
                            EntryCard(item: item, model: model, session: session)
                        }
                    }
                }
            }
            if let notice = model.notice {
                Toast(notice)
            }
        }
        .task { await model.load(session.client) }
    }
}

/// Labelled rows for a question's or a decision's facts; mono values use a monospaced font.
struct FactRows: View {
    let facts: [RunFact]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(facts) { f in
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Text(f.label).font(AppFont.meta).foregroundStyle(Tokens.muted)
                        .frame(width: 84, alignment: .leading)
                    Text(f.value)
                        .font(
                            f.mono
                                ? Font.system(size: 13, design: .monospaced) : AppFont.body
                        )
                        .foregroundStyle(Tokens.ink)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }
}

/// A text field in the app's input look.
private struct InputField: View {
    let placeholder: String
    @Binding var text: String

    var body: some View {
        TextField(placeholder, text: $text, axis: .vertical)
            .font(.system(size: 15))
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .frame(minHeight: 40)
            .background(RoundedRectangle(cornerRadius: 10).fill(Tokens.background))
            .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Tokens.line, lineWidth: 1))
    }
}

/// One run waiting on an answer: options keyed A, B, C…, Other, Send answer and Stop run.
struct QuestionCard: View {
    let question: Question
    let model: NeedsYouModel
    let session: Session
    @State private var pick: Int?
    @State private var other = false
    @State private var text = ""

    /// The text that would be sent, or nil when nothing is chosen yet.
    private var answer: String? {
        if other {
            let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
            return t.isEmpty ? nil : t
        }
        if let pick, pick < question.options.count { return question.options[pick] }
        return nil
    }

    var body: some View {
        PlainCard(radius: Metrics.cardRadiusLarge) {
            HStack {
                Chip("Question", colour: Tokens.accent, fill: Tokens.accentSoft)
                Spacer()
            }
            Text(question.name).font(AppFont.meta).foregroundStyle(Tokens.muted)
            Text(question.headline).font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
                .fixedSize(horizontal: false, vertical: true)
            if !question.facts.isEmpty {
                FactRows(facts: question.facts).padding(.vertical, 4)
            }
            VStack(spacing: 8) {
                ForEach(Array(question.options.enumerated()), id: \.offset) { i, option in
                    optionButton(key: optionKey(i), label: option, on: !other && pick == i) {
                        other = false
                        pick = i
                    }
                }
                optionButton(key: "…", label: "Other", on: other) {
                    other = true
                    pick = nil
                }
                if other { InputField(placeholder: "Your answer", text: $text) }
            }
            .padding(.top, 4)
            HStack(spacing: Metrics.buttonGap) {
                Button("Send answer") {
                    guard let answer else { return }
                    Task { await model.answer(run: question.run, text: answer, client: session.client) }
                }
                .buttonStyle(ActionButtonStyle(.primary))
                .disabled(answer == nil)
                Button("Stop run") {
                    Task { await model.stop(run: question.run, client: session.client) }
                }
                .buttonStyle(ActionButtonStyle(.destructive))
            }
            .disabled(model.busy != nil)
            .padding(.top, 4)
            countdown
        }
    }

    /// One answer button: a key square and the option text; filled softly when chosen.
    private func optionButton(
        key: String, label: String, on: Bool, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Text(key)
                    .font(.system(size: 13, weight: .bold))
                    .foregroundStyle(on ? Tokens.onAccent : Tokens.accent)
                    .frame(width: 26, height: 26)
                    .background(RoundedRectangle(cornerRadius: 7).fill(on ? Tokens.accent : Tokens.accentSoft))
                Text(label).multilineTextAlignment(.leading)
                Spacer(minLength: 0)
            }
            .font(AppFont.button)
            .foregroundStyle(Tokens.ink)
            .padding(10)
            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: Metrics.buttonRadius).fill(on ? Tokens.accentSoft : Color.clear))
            .overlay(
                RoundedRectangle(cornerRadius: Metrics.buttonRadius)
                    .strokeBorder(on ? Tokens.accent : Tokens.line, lineWidth: 1))
        }
        .buttonStyle(.plain)
    }

    /// "No answer in M min S s and the run takes A", updated each second.
    @ViewBuilder
    private var countdown: some View {
        if let deadline = question.deadlineMs {
            TimelineView(.periodic(from: .now, by: 1)) { context in
                Text(countdownText(deadline: deadline, now: context.date))
                    .font(AppFont.meta).monospacedDigit().foregroundStyle(Tokens.muted)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    /// The countdown line for `now`.
    private func countdownText(deadline: Double, now: Date) -> String {
        let left = deadline - now.timeIntervalSince1970 * 1000
        if left <= 0 { return "The run is going on with the conventional option" }
        let seconds = Int(left / 1000)
        let tail =
            question.conventional.map { "the run takes \(optionKey($0))" }
            ?? "the run picks the conventional option"
        return "No answer in \(seconds / 60) min \(seconds % 60) s and \(tail)"
    }
}

/// One question that timed out: what the run chose, with Looks right, Correct it and Stop run.
struct DecisionCard: View {
    let decision: Decision
    let model: NeedsYouModel
    let session: Session
    @State private var correcting = false
    @State private var text = ""

    var body: some View {
        PlainCard(radius: Metrics.cardRadiusLarge) {
            HStack {
                Chip("Decided for you")
                Spacer()
                Text("\(timeAgoText(decision.at)) · \(decision.name)")
                    .font(AppFont.meta).foregroundStyle(Tokens.muted).lineLimit(1)
            }
            if let chose = decision.chose, !chose.isEmpty {
                Text("It chose").font(AppFont.meta).foregroundStyle(Tokens.muted)
                Text(chose).font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                Text(decision.headline ?? (decision.question.isEmpty ? "A question" : decision.question))
                    .font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
                    .fixedSize(horizontal: false, vertical: true)
                Text("See its report for what it chose")
                    .font(AppFont.meta).foregroundStyle(Tokens.muted)
            }
            if !decision.instead.isEmpty {
                Text("Instead of: \(decision.instead.joined(separator: " or "))")
                    .font(AppFont.body).foregroundStyle(Tokens.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            factsBox
            VStack(spacing: Metrics.buttonGap) {
                HStack(spacing: Metrics.buttonGap) {
                    Button("Looks right") { model.dismiss(decision) }
                        .buttonStyle(ActionButtonStyle(.primary))
                    Button("Correct it") { correcting.toggle() }
                        .buttonStyle(ActionButtonStyle(.secondary))
                }
                if correcting {
                    InputField(placeholder: "What it should do instead", text: $text)
                    Button("Send note") {
                        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
                        guard !t.isEmpty else { return }
                        Task {
                            await model.note(run: decision.run, text: t, client: session.client)
                            text = ""
                            correcting = false
                        }
                    }
                    .buttonStyle(ActionButtonStyle(.primary))
                }
                Button("Stop run") {
                    Task { await model.stop(run: decision.run, client: session.client) }
                }
                .buttonStyle(ActionButtonStyle(.destructive))
            }
            .disabled(model.busy != nil)
            .padding(.top, 4)
        }
    }

    /// The facts box: why, the branch, then the entry's own facts.
    private var factsBox: some View {
        var rows = [RunFact(id: 0, label: "Why", value: "No answer within 5 min", mono: false)]
        if let branch = decision.branch, !branch.isEmpty {
            rows.append(RunFact(id: 1, label: "Branch", value: branch, mono: true))
        }
        for f in decision.facts {
            rows.append(RunFact(id: rows.count, label: f.label, value: f.value, mono: f.mono))
        }
        return FactRows(facts: rows)
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 10).fill(Tokens.background))
    }
}

/// One approval card, with Approve and Deny behind the passkey.
struct EntryCard: View {
    let item: Entry
    let model: NeedsYouModel
    let session: Session

    var body: some View {
        PlainCard {
            HStack {
                Text(item.kind.rawValue).font(AppFont.label).foregroundStyle(Tokens.muted)
                Spacer()
                timeView
            }
            Text(item.title).font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
            Text(item.sub).font(AppFont.body).foregroundStyle(Tokens.muted)
            buttons.padding(.top, 4)
        }
    }

    /// The time left (orange under 15 minutes) or the time since it was made, after a clock.
    @ViewBuilder
    private var timeView: some View {
        if let expires = item.expiresMs {
            clock(timeLeftText(expires), urgent: expires - nowMs() < 15 * 60_000)
        } else if let created = item.createdMs {
            clock(timeAgoText(created), urgent: false)
        }
    }

    /// A clock symbol and a time, in one colour.
    private func clock(_ text: String, urgent: Bool) -> some View {
        HStack(spacing: 4) {
            Image(systemName: "clock").font(.system(size: 12))
            Text(text).font(.system(size: 13, weight: .semibold)).monospacedDigit()
        }
        .foregroundStyle(urgent ? Tokens.warn : Tokens.muted)
    }

    /// Approve, and Deny when the kind can be denied.
    private var buttons: some View {
        HStack(spacing: Metrics.buttonGap) {
            Button {
                Task { await model.decide(item, approve: true, client: session.client) }
            } label: {
                Label("Approve", systemImage: "key")
            }
            .buttonStyle(ActionButtonStyle(.primary))
            if item.kind.deny != nil {
                Button("Deny") {
                    Task { await model.decide(item, approve: false, client: session.client) }
                }
                .buttonStyle(ActionButtonStyle(.destructive))
            }
        }
        .disabled(model.busy != nil)
    }
}
