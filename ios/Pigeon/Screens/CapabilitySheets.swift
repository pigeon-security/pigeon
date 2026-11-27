import SwiftUI

// The sheets a capability row opens beyond Configure, as `views/capabilities.js` opens them:
// DigitalOcean's Connection, Spaces and Runner check, Jobs audit storage, and MCP Federation
// trust. Every record change is one `instance-put` through the confirm sheet.
//
// The DigitalOcean writes (`digitalocean-token-set`, `digitalocean-spaces-key-set`,
// `digitalocean-droplet-token-set`, `digitalocean-droplet-start`, `digitalocean-droplet-cancel`)
// are the console's own: `console::write::ACTIONS` does not open them to the app's bearer, so
// these sheets read and verify, and say where the writes are made.

/// A sheet a capability row opens.
enum CapabilitySheet: Identifiable {
    /// DigitalOcean's token (`spaces` false) or Spaces key connection.
    case connection(JSON, spaces: Bool)
    /// DigitalOcean's temporary Droplet runner check.
    case droplet(JSON)
    /// Jobs audit storage on a linked instance: enabling Jobs, or setting up a bucket.
    case jobs(CapabilitySpec, JSON, editing: Bool)
    /// MCP Federation trust: its grants, and a new one.
    case federation(JSON, editing: Bool)

    var id: String {
        switch self {
        case .connection(_, let spaces): return spaces ? "spaces" : "connection"
        case .droplet: return "droplet"
        case .jobs: return "jobs"
        case .federation: return "federation"
        }
    }
}

/// The one record write: the exact diff, then the passkey, through `instance-put`.
enum CapabilityWrite {
    /// The confirm sheet's request for `record` → `next`, or nil when nothing changes.
    /// `jobsAudit` is the reviewed audit bucket action, `create` or `reuse`.
    static func put(
        _ record: JSON, next: JSON, title: String, facts: [Fact], effect: String,
        destructive: Bool = false, confirmLabel: String, jobsAudit: String? = nil,
        onDone: @escaping () -> Void
    ) -> WriteRequest? {
        let list = RecordDiff.changes(record, next)
        guard !list.isEmpty else { return nil }
        let id = record["id"].text ?? ""
        var change = RecordDiff.putChange(next)
        if let jobsAudit { change = RecordDiff.setting(change, "jobs_audit", .string(jobsAudit)) }
        return WriteRequest(
            title: title,
            subtitle: effect,
            facts: [Fact("Instance", id, mono: true)] + facts,
            lines: RecordDiff.lines(record, list),
            action: "instance-put",
            target: .string(id),
            change: change,
            destructive: destructive,
            confirmLabel: confirmLabel,
            onDone: { _ in onDone() })
    }
}

/// Lays its views out in lines, wrapping onto the next when one does not fit: a row's buttons.
struct FlowLayout: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        arrange(proposal.width ?? .infinity, subviews).size
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let frames = arrange(bounds.width, subviews).frames
        for (view, frame) in zip(subviews, frames) {
            view.place(
                at: CGPoint(x: bounds.minX + frame.minX, y: bounds.minY + frame.minY),
                proposal: ProposedViewSize(frame.size))
        }
    }

    private func arrange(_ width: CGFloat, _ subviews: Subviews) -> (frames: [CGRect], size: CGSize) {
        var frames: [CGRect] = []
        var x: CGFloat = 0
        var y: CGFloat = 0
        var line: CGFloat = 0
        var widest: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > 0 && x + size.width > width {
                x = 0
                y += line + spacing
                line = 0
            }
            frames.append(CGRect(origin: CGPoint(x: x, y: y), size: size))
            widest = max(widest, x + size.width)
            x += size.width + spacing
            line = max(line, size.height)
        }
        return (frames, CGSize(width: widest, height: y + line))
    }
}

/// Facts as the confirm sheet draws them: the label, then the value.
struct CapabilityFacts: View {
    let facts: [Fact]

    var body: some View {
        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 14, verticalSpacing: 6) {
            ForEach(facts) { fact in
                GridRow {
                    Text(fact.label).foregroundStyle(Theme.muted)
                    Text(fact.value)
                        .font(fact.mono ? Theme.Fonts.id : Theme.Fonts.body)
                        .foregroundStyle(Theme.ink)
                        .textSelection(.enabled)
                }
            }
        }
        .font(Theme.Fonts.body)
    }
}

/// A read-only sheet's frame: a title, the content, and Done.
private struct InfoSheet<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Text(title).font(.title3.weight(.bold)).foregroundStyle(Theme.ink)
                content
                Button("Done") { dismiss() }
                    .buttonStyle(SheetButtonStyle(.quiet))
                    .padding(.top, 4)
            }
            .padding(.horizontal, 20)
            .padding(.top, 24)
            .padding(.bottom, 16)
        }
        .scrollBounceBehavior(.basedOnSize)
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }
}

/// A line of a sheet's progress or outcome.
private struct SheetNote: View {
    let text: String
    var bad = false

    var body: some View {
        Text(text)
            .font(Theme.Fonts.meta)
            .foregroundStyle(bad ? Theme.bad : Theme.muted)
            .textSelection(.enabled)
            .fixedSize(horizontal: false, vertical: true)
    }
}

// MARK: - DigitalOcean

/// DigitalOcean's connection (`digitalocean_status`, `digitalocean_verify`) or its Spaces source
/// (`digitalocean_spaces_status`, `digitalocean_spaces_verify`): what is saved for the approved
/// team or bucket, and Verify.
struct DigitalOceanConnectionSheet: View {
    let record: JSON
    let spaces: Bool
    @Environment(ConsoleAPI.self) private var api
    @State private var state: Loadable<String> = .loading
    @State private var verifying = false
    @State private var outcome: String?
    @State private var failure: String?

    private var settings: JSON { record["capabilities"]["digitalocean"] }
    private var credential: String { spaces ? "Spaces key" : "token" }
    private var args: JSON { ["instance": .string(record["id"].text ?? "")] }

    private var facts: [Fact] {
        var out = [
            Fact("Instance", record["id"].text ?? "—", mono: true),
            Fact("Expected team", settings["team_id"].text ?? "—", mono: true),
        ]
        if spaces {
            let s = settings["spaces"]
            out.append(
                Fact(
                    "Source",
                    "\(s["region"].text ?? "") / \(s["bucket"].text ?? "") / \(s["prefix"].text ?? "")",
                    mono: true))
        }
        return out
    }

    var body: some View {
        InfoSheet(title: spaces ? "DigitalOcean Spaces" : "DigitalOcean connection") {
            CapabilityFacts(facts: facts)
            switch state {
            case .loading:
                HStack(spacing: 8) {
                    ProgressView()
                    SheetNote(text: "Checking connection…")
                }
            case .failed(let message):
                SheetNote(text: "Connection status unavailable.")
                SheetNote(text: message, bad: true)
            case .loaded(let s):
                HStack(spacing: 8) {
                    StatusPill(pill(s).0, tone: pill(s).1)
                    SheetNote(text: line(s))
                }
            }
            if verifying {
                HStack(spacing: 8) {
                    ProgressView()
                    SheetNote(text: "Verifying…")
                }
            } else if let failure {
                SheetNote(text: failure, bad: true)
            } else if let outcome {
                SheetNote(text: outcome)
            }
            Button("Verify \(credential)") { Task { await verify() } }
                .buttonStyle(SheetButtonStyle(.primary))
                .disabled(state.value != "connected" || verifying)
            SheetNote(
                text: "Setting or replacing the \(credential) is made in the web console, with your passkey: that write is not open to the app.")
        }
        .task { await load() }
    }

    private func pill(_ s: String) -> (String, Tone) {
        switch s {
        case "connected": return ("Connected", .ok)
        case "unverified": return ("Unverified", .warn)
        default: return ("Missing", .neutral)
        }
    }

    private func line(_ s: String) -> String {
        let what = spaces ? "bucket" : "team"
        switch s {
        case "connected": return "Saved for the approved \(what)."
        case "unverified": return "Replace the saved \(credential) to verify this \(spaces ? "source" : "team")."
        default: return "No \(credential) is saved for the approved \(what)."
        }
    }

    private func load() async {
        do {
            let result = try await api.op(spaces ? "digitalocean_spaces_status" : "digitalocean_status", args: args)
            state = .loaded(result["state"].string ?? "missing")
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            state = .failed(error.localizedDescription)
        }
    }

    private func verify() async {
        verifying = true
        failure = nil
        outcome = nil
        defer { verifying = false }
        do {
            let result = try await api.op(spaces ? "digitalocean_spaces_verify" : "digitalocean_verify", args: args)
            guard result["verified"].bool == true else {
                failure = "DigitalOcean verification did not complete."
                return
            }
            outcome = spaces
                ? "Verified read access to the approved Spaces source."
                : "Verified against the approved DigitalOcean team."
        } catch is CancellationError {
        } catch {
            failure = error.localizedDescription
        }
    }
}

/// DigitalOcean's temporary Droplet runner check (`digitalocean_droplet_status`): the compute
/// token's state, the plan a check would run, and the current run.
struct DigitalOceanDropletSheet: View {
    let record: JSON
    @Environment(ConsoleAPI.self) private var api
    @State private var state: Loadable<JSON> = .loading
    @State private var refreshing = false

    private var facts: [Fact] {
        [
            Fact("Instance", record["id"].text ?? "—", mono: true),
            Fact("Expected team", record["capabilities"]["digitalocean"]["team_id"].text ?? "—", mono: true),
        ]
    }

    var body: some View {
        InfoSheet(title: "DigitalOcean runner check") {
            CapabilityFacts(facts: facts)
            switch state {
            case .loading:
                HStack(spacing: 8) {
                    ProgressView()
                    SheetNote(text: "Checking runner status…")
                }
            case .failed(let message):
                SheetNote(text: message, bad: true)
            case .loaded(let result):
                status(result)
            }
            Button("Refresh") { Task { await load() } }
                .buttonStyle(SheetButtonStyle(.primary))
                .disabled(refreshing)
            SheetNote(
                text: "Setting the compute token, starting a runner check and cancelling one are made in the web console, with your passkey: those writes are not open to the app.")
        }
        .task { await load() }
    }

    @ViewBuilder
    private func status(_ result: JSON) -> some View {
        let configured = result["credential"].string == "configured"
        HStack(spacing: 8) {
            StatusPill(configured ? "Token saved" : "No token", tone: configured ? .ok : .neutral)
            SheetNote(
                text: configured
                    ? "Compute token saved for this team." : "Set a compute token for the approved team.")
        }
        let plan = result["plan"]
        if !plan.isNull {
            CapabilityFacts(facts: [
                Fact("Region", plan["region"].text ?? "—", mono: true),
                Fact("Size", plan["size"].text ?? "—", mono: true),
                Fact("Droplet image", plan["image"].text ?? "—", mono: true),
                Fact("Runner commit", plan["commit"].text ?? "—", mono: true),
                Fact("Runtime limit", "\(plan["max_minutes"].text ?? "—") minutes"),
                Fact("Estimated compute cost", "$\(plan["estimated_usd"].text ?? "—") USD"),
            ])
        }
        let run = result["run"]
        if !run.isNull {
            CapabilityFacts(facts: runFacts(run))
        }
    }

    private func runFacts(_ run: JSON) -> [Fact] {
        var out = [Fact("Run", run["id"].text ?? "—", mono: true), Fact("State", run["state"].text ?? "—")]
        if let health = run["health"].text, !health.isEmpty { out.append(Fact("Health", health)) }
        if let error = run["error"].text, !error.isEmpty { out.append(Fact("Detail", error)) }
        return out
    }

    private func load() async {
        refreshing = true
        defer { refreshing = false }
        do {
            let result = try await api.op(
                "digitalocean_droplet_status", args: ["instance": .string(record["id"].text ?? "")])
            state = .loaded(result)
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            state = .failed(error.localizedDescription)
        }
    }
}

// MARK: - Jobs

/// Jobs audit storage on a linked instance: `app_jobs_audit_plan` names the private R2 bucket
/// and whether it is made or reused, then the confirm sheet saves Jobs with it (`instance-put`
/// with `jobs_audit`, the reviewed action).
struct JobsAuditSheet: View {
    let spec: CapabilitySpec
    let record: JSON
    let instanceTitle: String
    let editing: Bool
    let onDone: () -> Void
    @Environment(ConsoleAPI.self) private var api
    @State private var request: WriteRequest?
    @State private var loading = false
    @State private var problem: String?

    private var title: String {
        editing ? "Set up Jobs audit storage on \(instanceTitle)" : "Enable Jobs on \(instanceTitle)"
    }

    var body: some View {
        if let request {
            ConfirmSheet(request)
        } else {
            ReviewForm(
                title: title,
                subtitle: "All jobs on this instance share a private R2 audit bucket in its linked Cloudflare account.",
                problem: problem, reviewLabel: "Review bucket", review: { Task { await load() } }
            ) {
                if loading {
                    HStack(spacing: 8) {
                        ProgressView()
                        SheetNote(text: "Checking the audit bucket…")
                    }
                }
            }
            .task { await load() }
        }
    }

    private func load() async {
        guard !loading else { return }
        loading = true
        problem = nil
        defer { loading = false }
        do {
            let plan = try await api.op(
                "app_jobs_audit_plan", args: ["instance": .string(record["id"].text ?? "")])
            guard let bucket = plan["bucket"].string, !bucket.isEmpty,
                let action = plan["action"].string, action == "create" || action == "reuse"
            else {
                problem = "Pigeon couldn’t determine which audit bucket action to review."
                return
            }
            let next = CapabilityCatalog.with(record, spec, values: ["audit_bucket": .string(bucket)])
            let account = plan["account_name"].string.flatMap { $0.isEmpty ? nil : $0 }
            let made = CapabilityWrite.put(
                record, next: next, title: title,
                facts: [
                    Fact("R2 audit bucket", bucket, mono: true),
                    Fact("Cloudflare account", account ?? "This instance’s linked account"),
                    Fact(
                        "Bucket action",
                        action == "create" ? "Create a private bucket" : "Reuse the existing private bucket"),
                ],
                effect: "Pigeon verifies write access before saving Jobs. When a run finishes, Pigeon exports its audit record to this bucket.",
                confirmLabel: editing ? "Set up with passkey" : "Enable with passkey",
                jobsAudit: action, onDone: onDone)
            guard let made else {
                problem = "Nothing changes."
                return
            }
            request = made
        } catch is CancellationError {
        } catch {
            problem = error.localizedDescription
        }
    }
}

// MARK: - MCP Federation

/// MCP Federation trust: each grant with Revoke, and a new grant (a peer instance, a person, an
/// OAuth app or all of them, and the read tools), each a record change through `instance-put`.
/// The peer instance's owner approves the same access on their side.
struct FederationTrustSheet: View {
    let record: JSON
    let editing: Bool
    let onDone: () -> Void
    @Environment(ConsoleAPI.self) private var api
    @State private var request: WriteRequest?
    @State private var problem: String?
    @State private var peers: Loadable<[JSON]> = .loading
    @State private var people: Loadable<[JSON]> = .loading
    @State private var peer = ""
    @State private var person = ""
    @State private var allApps = false
    @State private var app = ""
    @State private var chosen = Set(FederationTrustSheet.tools.map { $0.0 })

    static let name = "mcp-federation"
    /// The reads a grant may share, as the web lists them.
    static let tools: [(String, String)] = [
        ("digitalocean_droplet_jobs", "List runner checks"),
        ("digitalocean_droplet_job", "Read a runner check"),
        ("digitalocean_droplet_status", "Read runner status"),
    ]

    private var grants: [JSON] { record["capabilities"][Self.name]["federation"].items }
    private var title: String { editing ? "Manage MCP Federation trust" : "Enable MCP Federation" }

    var body: some View {
        if let request {
            ConfirmSheet(request)
        } else {
            ReviewForm(title: title, problem: problem, reviewLabel: "Review grant", review: { add() }) {
                ForEach(Array(grants.enumerated()), id: \.offset) { index, g in
                    VStack(alignment: .leading, spacing: 10) {
                        CapabilityFacts(facts: facts(g))
                        Button("Revoke grant") { revoke(index) }
                            .buttonStyle(RowButtonStyle(.danger))
                    }
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Theme.raised, in: RoundedRectangle(cornerRadius: 10))
                }
                FormField(label: "Peer instance") {
                    Picker("Peer instance", selection: $peer) {
                        Text(prompt(peers, loading: "Loading instances…", failed: "Could not load instances",
                                    none: "No eligible peer instances", choose: "Select a peer instance")).tag("")
                        ForEach(peers.value ?? [], id: \.self) { p in
                            Text(peerLabel(p)).tag(p["id"].string ?? "")
                        }
                    }
                    .pickerStyle(.menu)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                FormField(label: "Person") {
                    Picker("Person", selection: $person) {
                        Text(prompt(people, loading: "Loading people…", failed: "Could not load people",
                                    none: "No instance members", choose: "Select a person")).tag("")
                        ForEach(people.value ?? [], id: \.self) { m in
                            Text(personLabel(m)).tag(m["id"].string ?? "")
                        }
                    }
                    .pickerStyle(.menu)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                FormField(label: "OAuth app access") {
                    Picker("OAuth app access", selection: $allApps) {
                        Text("Specific app").tag(false)
                        Text("All apps").tag(true)
                    }
                    .pickerStyle(.segmented)
                }
                if !allApps {
                    FormField(
                        label: "OAuth app service user ID",
                        hint: "Expand the person’s connected app under Access › People to see its service user ID."
                    ) {
                        TextField("Service user ID", text: $app)
                            .font(Theme.Fonts.idBody)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                    }
                }
                FormField(label: "Read tools") {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(Self.tools.map { $0.0 }, id: \.self) { tool in
                            Toggle(Self.toolLabel(tool), isOn: toolBinding(tool))
                        }
                    }
                }
            }
            .task { await load() }
        }
    }

    private func toolBinding(_ tool: String) -> Binding<Bool> {
        Binding(
            get: { chosen.contains(tool) },
            set: { on in
                if on { chosen.insert(tool) } else { chosen.remove(tool) }
            })
    }

    private func prompt(
        _ list: Loadable<[JSON]>, loading: String, failed: String, none: String, choose: String
    ) -> String {
        switch list {
        case .loading: return loading
        case .failed: return failed
        case .loaded(let items): return items.isEmpty ? none : choose
        }
    }

    private func peerLabel(_ p: JSON) -> String {
        let id = p["id"].text ?? "—"
        guard let t = p["title"].string, !t.isEmpty, t != id else { return id }
        return "\(t) (\(id))"
    }

    private func personLabel(_ m: JSON) -> String {
        if let n = m["name"].string, !n.isEmpty { return n }
        return m["id"].text ?? "—"
    }

    /// A read tool's label, or its name when it is not one the web lists.
    static func toolLabel(_ tool: String) -> String {
        tools.first { $0.0 == tool }?.1 ?? tool
    }

    private func facts(_ g: JSON) -> [Fact] {
        let names = g["tools"].items.compactMap { $0.string }.map { Self.toolLabel($0) }
        return [
            Fact("Peer instance", g["peer"].text ?? "—", mono: true),
            Fact("Person ID", g["person"].text ?? "—", mono: true),
            g["all_apps"].bool == true
                ? Fact("OAuth app scope", "All current and future OAuth apps")
                : Fact("OAuth app scope", g["app"].text ?? "—", mono: true),
            Fact("Read tools", names.joined(separator: ", ")),
        ]
    }

    private func add() {
        let appID = app.trimmingCharacters(in: .whitespacesAndNewlines)
        let tools = Self.tools.map { $0.0 }.filter { chosen.contains($0) }
        guard !peer.isEmpty, !person.isEmpty, allApps || !appID.isEmpty, !tools.isEmpty else {
            problem = "Select a peer, a person, an app scope, and at least one tool. Enter the app ID for a specific app."
            return
        }
        let scope: String? = allApps ? nil : appID
        if grants.contains(where: { $0["peer"].string == peer && $0["person"].string == person && $0["app"].string == scope }) {
            problem = "Revoke the existing grant for this peer, person and app before replacing it."
            return
        }
        if grants.count >= 16 {
            problem = "This instance already has 16 grants."
            return
        }
        var grant: [String: JSON] = [
            "peer": .string(peer), "person": .string(person), "tools": .array(tools.map { .string($0) }),
        ]
        if allApps { grant["all_apps"] = .bool(true) } else { grant["app"] = .string(appID) }
        review(grants + [.object(grant)], removing: false)
    }

    private func revoke(_ index: Int) {
        var next = grants
        guard next.indices.contains(index) else { return }
        next.remove(at: index)
        review(next, removing: true)
    }

    private func review(_ nextGrants: [JSON], removing: Bool) {
        var settings = record["capabilities"][Self.name].object ?? [:]
        if nextGrants.isEmpty {
            settings.removeValue(forKey: "federation")
        } else {
            settings["federation"] = .array(nextGrants)
        }
        let caps = RecordDiff.setting(record["capabilities"], Self.name, .object(settings))
        let next = RecordDiff.setting(record, "capabilities", caps)
        let effect: String
        if removing {
            effect = "Blocks this federation grant on the next call."
        } else if nextGrants.last?["all_apps"].bool == true {
            effect = "This grant covers all current and future OAuth apps connected to this person. The peer instance must also approve access."
        } else {
            effect = "The peer instance owner must also approve this person, app and tool access."
        }
        let made = CapabilityWrite.put(
            record, next: next, title: removing ? "Revoke MCP Federation trust" : title,
            facts: [Fact("Capability", Self.name, mono: true)], effect: effect,
            destructive: removing, confirmLabel: removing ? "Revoke with passkey" : "Approve with passkey",
            onDone: onDone)
        guard let made else {
            problem = "Nothing changes."
            return
        }
        request = made
    }

    private func load() async {
        let home = record["account"]["kind"].string == "home"
        async let instances = api.op("instances_list")
        async let roster = api.op("app_federation_people", args: ["instance": .string(record["id"].text ?? "")])
        do {
            let data = try await instances
            let rows = data.array ?? data["instances"].items
            peers = .loaded(rows.filter { i in
                i["id"] != record["id"] && i["account"]["kind"].string == (home ? "linked" : "home")
            })
        } catch is CancellationError {
        } catch {
            peers = .failed(error.localizedDescription)
            problem = error.localizedDescription
        }
        do {
            let data = try await roster
            people = .loaded(data.items)
        } catch is CancellationError {
        } catch {
            people = .failed(error.localizedDescription)
            problem = error.localizedDescription
        }
    }
}
