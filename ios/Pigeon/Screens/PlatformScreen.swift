import SwiftUI
import UIKit

/// Platform (platform-wide, home only): Architecture, Models and Endpoints, as the web console's
/// `platform.js`.
///
/// - Architecture: `architecture_describe` (counts, the Workers and what each runs; the SVG map
///   stays on the web).
/// - Models: `ai_agents_models`, `ai_agents_config`, `secrets_list`; edits `model-defaults` and
///   `agents-config` through the confirm sheet.
/// - Endpoints: the MCP URLs from `instances_list`, to copy. (The mock-up's TestFlight row needs
///   `ios_releases`, which the console's reads do not allow.)
struct PlatformScreen: View {
    let context: AreaContext

    var body: some View {
        switch context.viewID.wrappedValue {
        case "models": PlatformModels(context: context)
        case "endpoints": PlatformEndpoints(context: context)
        default: PlatformArchitecture(context: context)
        }
    }
}

// MARK: - Architecture

struct PlatformArchitecture: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var state: Loadable<Description> = .loading

    struct Worker: Identifiable, Hashable {
        let name: String
        let lane: String?
        let tools: [String]
        let objects: [String]
        let containers: [String]
        let routes: [String]
        var id: String { name }
    }

    struct Description {
        let workers: [Worker]
        let capabilities: Int
        let tools: Int
        let hostnames: [String]
        let commit: String?
        let digest: String?

        init(_ d: JSON) {
            workers = d["workers"].items.compactMap { (w: JSON) -> Worker? in
                guard let name = w["name"].string else { return nil }
                let tools = Array(Set(w["tools"].items.compactMap { $0.string })).sorted()
                let objects = w["objects"].items.compactMap { (o: JSON) -> String? in
                    guard let cls = o["class"].string else { return nil }
                    if let owner = o["owner"].string, owner != name { return "\(cls) → \(owner)" }
                    return cls
                }
                return Worker(
                    name: name, lane: w["lane"].text, tools: tools, objects: objects,
                    containers: w["containers"].items.compactMap { $0.text },
                    routes: w["routes"].items.compactMap { $0.text })
            }
            capabilities = d["parts"].items.filter { !$0["capability"].isNull && $0["capability"] != .bool(false) }.count
            var all = Set<String>()
            for w in d["workers"].items {
                for t in w["tools"].items { if let t = t.string { all.insert(t) } }
            }
            tools = all.count
            hostnames = d["hostnames"].items.compactMap { $0.text }
            commit = d["commit"].text
            digest = d["digest"].text
        }
    }

    var body: some View {
        List {
            PlatformViewPicker(context: context)
            LoadableSection(
                state: state, emptyTitle: "No Workers described", isEmpty: { $0.workers.isEmpty },
                retry: { await load() }
            ) { d in
                Section {
                    KeyValueRow("Workers", String(d.workers.count))
                    KeyValueRow("Capabilities", String(d.capabilities))
                    KeyValueRow("MCP tools", String(d.tools))
                    KeyValueRow("Hostnames", String(d.hostnames.count))
                } footer: {
                    Text(
                        "\(d.commit.map { "deployed \(String($0.prefix(12)))" } ?? "local build") · digest \(d.digest ?? "—"). The diagram is on the web console."
                    )
                    .font(Theme.Fonts.id)
                }
                Section("Workers") {
                    ForEach(d.workers) { w in
                        NavigationLink {
                            WorkerDetail(worker: w)
                        } label: {
                            ListRow(
                                title: w.name,
                                subtitle: [
                                    PlatformFormat.count(w.tools.count, "MCP tool", "MCP tools"),
                                    w.lane.map { "lane \($0)" },
                                ].compactMap { $0 }.joined(separator: " · "),
                                monoTitle: true)
                        }
                    }
                }
                if !d.hostnames.isEmpty {
                    Section("Hostnames") {
                        ForEach(d.hostnames, id: \.self) { IDText($0) }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
    }

    private func load() async {
        do {
            state = .loaded(Description(try await api.op("architecture_describe")))
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            if state.value == nil { state = .failed(error.localizedDescription) }
        }
    }
}

/// One Worker: its routes, Durable Objects, containers and MCP tools.
private struct WorkerDetail: View {
    let worker: PlatformArchitecture.Worker

    var body: some View {
        List {
            group("Routes", worker.routes)
            group("Durable Objects", worker.objects)
            group("Containers", worker.containers)
            group("MCP tools", worker.tools)
        }
        .listStyle(.insetGrouped)
        .navigationTitle(worker.name)
        .navigationBarTitleDisplayMode(.inline)
    }

    @ViewBuilder
    private func group(_ title: String, _ items: [String]) -> some View {
        Section("\(title) (\(items.count))") {
            if items.isEmpty {
                Text("None").font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            }
            ForEach(items, id: \.self) { IDText($0) }
        }
    }
}

// MARK: - Models

struct PlatformModels: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @Environment(\.openArea) private var openArea
    @State private var state: Loadable<Catalog> = .loading
    @State private var editingModel: ModelSlot?
    @State private var editingCap: CapEdit?

    static let roles = ["planner", "executor", "mechanical", "summary"]
    static let kinds = ["integrate", "docs", "fix", "design"]
    /// A model provider the catalog may list, and the secret that holds its key.
    struct Provider: Identifiable, Hashable {
        let id: String
        let name: String
        let secret: String
    }

    static let providers = [
        Provider(id: "anthropic", name: "Anthropic (own key)", secret: "pigeon-anthropic_anthropic_default"),
        Provider(id: "openai", name: "OpenAI (own key)", secret: "pigeon-openai_openai_default"),
        Provider(id: "cf-anthropic", name: "Anthropic via Cloudflare", secret: "CF_PARENT_TOKEN"),
        Provider(id: "cf-openai", name: "OpenAI via Cloudflare", secret: "CF_PARENT_TOKEN"),
        Provider(id: "workers-ai", name: "Workers AI", secret: "CF_PARENT_TOKEN"),
    ]

    /// A concurrency cap: its key, label, hint and range.
    struct Cap: Identifiable, Hashable {
        let key: String
        let label: String
        let hint: String
        let min: Int
        let max: Int
        var id: String { key }
    }

    static let caps = [
        Cap(key: "max_live_runs", label: "Live runs", hint: "Live runs of every role at once.", min: 1, max: 64),
        Cap(
            key: "max_executors_per_planner", label: "Executors per planner",
            hint: "Executors one planner may run at once.", min: 1, max: 16),
        Cap(
            key: "executor_reserve", label: "Executor reserve",
            hint: "Slots planners leave free for executors; less than live runs.", min: 0, max: 63),
    ]

    /// A slot whose default model is edited: a role, or `planner_<kind>`.
    struct ModelSlot: Identifiable {
        let slot: String
        let label: String
        let current: String?
        let choices: [String]
        var id: String { slot }
    }

    struct CapEdit: Identifiable {
        let cap: Cap
        let current: Int?
        let isDefault: Bool
        var id: String { cap.key }
    }

    struct Catalog {
        let models: JSON
        let config: JSON?
        let secrets: JSON?

        var defaults: JSON { models["defaults"] }
        var notes: [String] { models["notes"].items.compactMap { $0.string } }

        /// Every model the catalog knows, as `provider/model` and bare provider names, `current` first.
        func choices(current: String?) -> [String] {
            var out: [String] = []
            func add(_ s: String) { if !out.contains(s) { out.append(s) } }
            if let current { add(current) }
            let tiers = models["tiers"].object ?? [:]
            for provider in tiers.keys.sorted() {
                let t = tiers[provider] ?? .null
                add(provider)
                for tier in ["top", "middle", "small"] {
                    if let m = t[tier].string { add("\(provider)/\(m)") }
                }
                for extra in t["other"].items {
                    if let m = extra.string { add("\(provider)/\(m)") }
                }
            }
            return out
        }

        /// The key state of a provider's secret, from `secrets_list`.
        func keyPill(_ secret: String) -> (String, Tone) {
            guard let list = secrets?.array else { return ("Unknown", Tone.neutral) }
            guard let s = list.first(where: { $0["name"].string == secret }) else { return ("No slot", Tone.neutral) }
            if s["set"].bool == true {
                return (secret == "CF_PARENT_TOKEN" ? "Parent token set" : "Key set", Tone.ok)
            }
            return ("Key missing", Tone.bad)
        }
    }

    var body: some View {
        List {
            PlatformViewPicker(context: context)
            LoadableSection(state: state, retry: { await load() }) { catalog in
                rolesSection(catalog)
                kindsSection(catalog)
                capsSection(catalog)
                providersSection(catalog)
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
        .sheet(item: $editingModel) { slot in
            ModelDefaultSheet(slot: slot) { Task { await load() } }
        }
        .sheet(item: $editingCap) { edit in
            CapSheet(edit: edit) { Task { await load() } }
        }
    }

    // MARK: Sections

    @ViewBuilder
    private func rolesSection(_ c: Catalog) -> some View {
        let roles = Self.roles.filter { !c.defaults[$0].isNull }
        Section {
            ForEach(roles, id: \.self) { role in
                let d = c.defaults[role]
                editRow(
                    title: PlatformFormat.word(role),
                    subtitle: [d["model"].string ?? "—", d["effort"].string, payText(d["route"].string)]
                        .compactMap { $0 }.joined(separator: " · "),
                    pill: sourcePill(d["source"].string)
                ) {
                    editingModel = ModelSlot(
                        slot: role, label: PlatformFormat.word(role), current: d["model"].string,
                        choices: c.choices(current: d["model"].string))
                }
            }
        } header: {
            Text("Defaults by role")
        } footer: {
            Text(
                (["What a run uses when it names no model."] + c.notes).joined(separator: " ")
            )
        }
    }

    @ViewBuilder
    private func kindsSection(_ c: Catalog) -> some View {
        let planner = c.defaults["planner"]["model"].string
        Section {
            ForEach(Self.kinds, id: \.self) { kind in
                let model = c.defaults["planner_\(kind)"]["model"].string
                editRow(
                    title: PlatformFormat.word(kind),
                    subtitle: model ?? "Planner default · \(planner ?? "—")",
                    pill: model == nil ? nil : ("Override", Tone.accent)
                ) {
                    editingModel = ModelSlot(
                        slot: "planner_\(kind)", label: "\(PlatformFormat.word(kind)) planner",
                        current: model, choices: c.choices(current: model))
                }
            }
        } header: {
            Text("Planner by kind")
        } footer: {
            Text("Kinds without an override use the planner default.")
        }
    }

    @ViewBuilder
    private func capsSection(_ c: Catalog) -> some View {
        Section {
            if let config = c.config {
                ForEach(Self.caps) { cap in
                    let value = config[cap.key].int
                    let isDefault = config["is_default"][cap.key].bool == true
                    editRow(
                        title: cap.label, subtitle: cap.hint,
                        value: value.map { String($0) } ?? "—",
                        pill: isDefault ? ("Default", Tone.neutral) : ("Stored", Tone.accent)
                    ) {
                        editingCap = CapEdit(cap: cap, current: value, isDefault: isDefault)
                    }
                }
            } else {
                Text("The concurrency caps couldn't be read.")
                    .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            }
        } header: {
            Text("Concurrency caps")
        } footer: {
            Text("How many runs may be live at once. A run beyond a cap stays queued and starts as slots free.")
        }
    }

    @ViewBuilder
    private func providersSection(_ c: Catalog) -> some View {
        let tiers = c.models["tiers"]
        let known = Self.providers.filter { !tiers[$0.id].isNull }
        if !known.isEmpty {
            Section {
                ForEach(known) { p in
                    let t = tiers[p.id]
                    let key = c.keyPill(p.secret)
                    ListRow(
                        title: p.name,
                        subtitle: ["top", "middle", "small"].map { t[$0].string ?? "—" }.joined(separator: " · ")
                    ) {
                        StatusPill(key.0, tone: key.1)
                    }
                }
                Button {
                    openArea("access")
                } label: {
                    Text("Keys are set in Access › Secrets.")
                        .font(Theme.Fonts.meta).foregroundStyle(Theme.accent)
                }
                .buttonStyle(.plain)
            } header: {
                Text("Providers")
            } footer: {
                Text("Catalog tiers (top · middle · small), not live availability.")
            }
        }
    }

    // MARK: Rows

    private func editRow(
        title: String, subtitle: String, value: String? = nil, pill: (String, Tone)?,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 1) {
                    Text(title).font(Theme.Fonts.body).foregroundStyle(Theme.ink)
                    Text(subtitle).idStyle().foregroundStyle(Theme.muted)
                }
                Spacer(minLength: 8)
                if let value {
                    Text(value).font(Theme.Fonts.idBody).foregroundStyle(Theme.ink)
                }
                if let pill { StatusPill(pill.0, tone: pill.1) }
                Image(systemName: "chevron.right")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Theme.lineStrong)
                    .accessibilityHidden(true)
            }
            .frame(minHeight: 32)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityHint("Edit")
    }

    private func payText(_ route: String?) -> String? {
        switch route {
        case "byok": return "Own key"
        case "credits": return "Cloudflare credits"
        default: return nil
        }
    }

    private func sourcePill(_ source: String?) -> (String, Tone)? {
        switch source {
        case "stored": return ("Stored", Tone.accent)
        case "built_in": return ("Built-in", Tone.neutral)
        case nil: return nil
        default: return (source ?? "—", Tone.neutral)
        }
    }

    private func load() async {
        do {
            let models = try await api.op("ai_agents_models")
            let config = try? await api.op("ai_agents_config")
            let secrets = try? await api.op("secrets_list")
            state = .loaded(Catalog(models: models, config: config, secrets: secrets))
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            if state.value == nil { state = .failed(error.localizedDescription) }
        }
    }
}

/// Edit a default model: choose, then the confirm sheet for `model-defaults`.
private struct ModelDefaultSheet: View {
    let slot: PlatformModels.ModelSlot
    let onDone: () -> Void
    @State private var choice = ""
    @State private var problem: String?
    @State private var request: WriteRequest?

    var body: some View {
        if let request {
            ConfirmSheet(request)
        } else {
            ReviewForm(
                title: "Edit \(slot.label) default",
                subtitle: "A bare provider name means its model for this slot's tier. A run can still choose its own.",
                problem: problem, review: { review() }
            ) {
                FormField(label: "Now") {
                    Text(slot.current ?? "the planner default").font(Theme.Fonts.idBody)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                FormField(label: "Model") {
                    Picker("Model", selection: $choice) {
                        ForEach(slot.choices, id: \.self) { Text($0).font(Theme.Fonts.idBody).tag($0) }
                    }
                    .pickerStyle(.menu)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .onAppear { if choice.isEmpty { choice = slot.current ?? slot.choices.first ?? "" } }
        }
    }

    private func review() {
        guard !choice.isEmpty, choice != slot.current else {
            problem = "Choose a different model first."
            return
        }
        var lines: [DiffLine] = []
        if let current = slot.current { lines.append(.remove("\(slot.slot): \(current)")) }
        lines.append(.add("\(slot.slot): \(choice)"))
        request = WriteRequest(
            title: "Edit \(slot.label) default",
            facts: [Fact("Slot", slot.slot, mono: true), Fact("Now", slot.current ?? "the planner default", mono: true)],
            lines: lines,
            action: "model-defaults",
            target: "models",
            change: ["set": .object([slot.slot: .string(choice)])],
            confirmLabel: "Save with passkey",
            onDone: { _ in onDone() })
    }
}

/// Set a concurrency cap: a number in its range, then the confirm sheet for `agents-config`.
private struct CapSheet: View {
    let edit: PlatformModels.CapEdit
    let onDone: () -> Void
    @State private var value = 0
    @State private var started = false
    @State private var problem: String?
    @State private var request: WriteRequest?

    var body: some View {
        if let request {
            ConfirmSheet(request)
        } else {
            ReviewForm(
                title: "Set \(edit.cap.label.lowercased())",
                subtitle: "\(edit.cap.hint) \(edit.cap.min) to \(edit.cap.max).",
                problem: problem, review: { review() }
            ) {
                FormField(label: edit.cap.label, hint: nowText) {
                    Stepper(String(value), value: $value, in: edit.cap.min...edit.cap.max)
                        .font(Theme.Fonts.idBody)
                }
            }
            .onAppear {
                if !started {
                    started = true
                    value = min(max(edit.current ?? edit.cap.min, edit.cap.min), edit.cap.max)
                }
            }
        }
    }

    private var nowText: String {
        "Now \(edit.current.map { String($0) } ?? "—")\(edit.isDefault ? " (default)" : "")"
    }

    private func review() {
        guard value != edit.current else {
            problem = "Choose a different value first."
            return
        }
        var lines: [DiffLine] = []
        if let current = edit.current { lines.append(.remove("\(edit.cap.key): \(current)")) }
        lines.append(.add("\(edit.cap.key): \(value)"))
        request = WriteRequest(
            title: "Set \(edit.cap.label.lowercased())",
            subtitle: "A run beyond a cap stays queued and starts as slots free.",
            facts: [Fact("Limit", edit.cap.key, mono: true), Fact("Now", nowText)],
            lines: lines,
            action: "agents-config",
            target: "limits",
            change: .object([edit.cap.key: .number(Double(value))]),
            confirmLabel: "Save with passkey",
            onDone: { _ in onDone() })
    }
}

// MARK: - Endpoints

struct PlatformEndpoints: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @Environment(ToastCenter.self) private var toasts
    @State private var state: Loadable<[Endpoint]> = .loading

    struct Endpoint: Identifiable, Hashable {
        let title: String
        let note: String
        let url: String
        var id: String { url }
    }

    var body: some View {
        List {
            PlatformViewPicker(context: context)
            Section {
                Text("Copy a URL into your MCP client. The client still needs authorization. An instance-scoped endpoint exposes only the tools its enabled capabilities register.")
                    .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            }
            LoadableSection(state: state, isEmpty: { $0.isEmpty }, retry: { await load() }) { rows in
                Section("MCP") {
                    ForEach(rows) { row in
                        HStack(spacing: 10) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(row.title).font(Theme.Fonts.body).foregroundStyle(Theme.ink)
                                Text(row.url).idStyle().foregroundStyle(Theme.ink2).textSelection(.enabled)
                                Text(row.note).font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                            }
                            Spacer(minLength: 8)
                            Button("Copy") { copy(row.url) }
                                .buttonStyle(RowButtonStyle(.quiet))
                                .accessibilityLabel("Copy \(row.title) URL")
                        }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
    }

    private func copy(_ url: String) {
        UIPasteboard.general.string = url
        toasts.show("Copied", tone: .ok)
    }

    /// The direct platform endpoint, the Access portal's (mcp.<zone>), and one per other
    /// instance (`?instance=`).
    private func load() async {
        let host = OAuth.host
        var rows = [Endpoint(title: "Direct platform", note: "Every tool you may call", url: "https://\(host)/mcp")]
        let labels = host.split(separator: ".")
        if labels.count >= 3 {
            let zone = labels.dropFirst().joined(separator: ".")
            rows.append(Endpoint(title: "Cloudflare Access portal", note: "Through the MCP server portal", url: "https://mcp.\(zone)/mcp"))
        }
        do {
            let data = try await api.op("instances_list")
            for r in data.array ?? data["instances"].items {
                guard let id = r["id"].string, r["account"]["kind"].string != "home" else { continue }
                let encoded = id.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? id
                rows.append(
                    Endpoint(
                        title: r["title"].string ?? id, note: "Instance-scoped",
                        url: "https://\(host)/mcp?instance=\(encoded)"))
            }
        } catch is CancellationError {
            return
        } catch {
            // The platform's own endpoints still show.
        }
        state = .loaded(rows)
    }
}
