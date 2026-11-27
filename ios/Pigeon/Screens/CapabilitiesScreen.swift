import SwiftUI

/// What Capabilities reads: the record (`instances_list`), the versions
/// (`capabilities_versions`) and the upgrade plan (`capabilities_upgrade_plan`).
struct CapabilitiesData {
    let record: JSON?
    let versions: JSON
    let plan: JSON
}

/// A capability to enable or configure in the form sheet.
struct CapabilityEdit: Identifiable {
    let spec: CapabilitySpec
    let record: JSON
    let editing: Bool
    var id: String { spec.cap }
}

/// Capabilities (M2): the one place capabilities change. Enable, Configure, Upgrade and Disable
/// each make one diff of the instance record, approved with the passkey through `instance-put`;
/// Upgrade moves a pin as `capabilities_upgrade_plan` suggests. Beside the record's own
/// capabilities are the catalogue's, enabled by name in `capabilities`; DigitalOcean, Jobs and
/// MCP Federation open their own sheets (`CapabilitySheets.swift`).
struct CapabilitiesScreen: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @Environment(ManifestStore.self) private var store
    @Environment(ToastCenter.self) private var toasts
    @State private var state: Loadable<CapabilitiesData> = .loading
    @State private var request: WriteRequest?
    @State private var editing: CapabilityEdit?
    @State private var sheet: CapabilitySheet?
    /// Whether the internal plumbing (`plumbing` in `capabilities_versions`) is shown.
    @State private var showInternal = false

    private var instance: ConsoleManifest.Instance { context.instance }

    /// The first view each capability's version is too old for, from the manifest.
    private var gated: [String: AreaView] {
        var out: [String: AreaView] = [:]
        for area in store.manifest?.areas ?? [] {
            for view in area.views where view.upgrade != nil {
                if let cap = view.capability, out[cap] == nil { out[cap] = view }
            }
        }
        return out
    }

    var body: some View {
        List {
            ViewPickerSection(context: context)
            UpgradeSection(context: context)
            Section {
                MutedNoteRow("Enable, Configure, Upgrade and Disable each make one diff of the instance record, which you approve with your passkey. Agents propose the same diff with instance_propose; it waits in Inbox.")
            }
            switch state {
            case .loading:
                Section { LoadingRows(count: 5) }
            case .failed(let message):
                Section { ErrorState(message: message) { await load() } }
            case .loaded(let data):
                if let record = data.record {
                    sections(record, data)
                } else {
                    Section {
                        EmptyState(
                            title: "No record", systemImage: "questionmark.folder",
                            message: "This instance has no record to change.")
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
        .confirmSheet($request)
        .sheet(item: $editing) { edit in
            CapabilityEditSheet(edit: edit, instanceTitle: instance.title) {
                Task { await reloadAll() }
            }
        }
        .sheet(item: $sheet) { s in
            switch s {
            case .connection(let record, let spaces):
                DigitalOceanConnectionSheet(record: record, spaces: spaces)
            case .droplet(let record):
                DigitalOceanDropletSheet(record: record)
            case .jobs(let spec, let record, let editing):
                JobsAuditSheet(spec: spec, record: record, instanceTitle: instance.title, editing: editing) {
                    Task { await reloadAll() }
                }
            case .federation(let record, let editing):
                FederationTrustSheet(record: record, editing: editing) {
                    Task { await reloadAll() }
                }
            }
        }
    }

    /// The capabilities this instance is offered: the record's own, then the catalogue's.
    private func offered(_ data: CapabilitiesData) -> [CapabilitySpec] {
        CapabilityCatalog.all + CapabilityCatalog.catalogue(data.versions, home: instance.home)
    }

    // MARK: Loading

    private func load() async {
        do {
            async let record = InstanceRecordLoader.load(api, id: instance.id)
            async let versions = softRead("capabilities_versions")
            async let plan = softRead("capabilities_upgrade_plan")
            let r = try await record
            let v = await versions
            let p = await plan
            state = .loaded(CapabilitiesData(record: r, versions: v, plan: p))
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            if state.value == nil { state = .failed(error.localizedDescription) }
        }
    }

    /// A read whose failure only leaves its column blank, as on the web.
    private func softRead(_ name: String) async -> JSON {
        (try? await api.op(name, args: ["instance": .string(instance.id)])) ?? .null
    }

    private func reloadAll() async {
        await load()
        await store.refresh(api)
    }

    // MARK: Rows

    @ViewBuilder
    private func sections(_ record: JSON, _ data: CapabilitiesData) -> some View {
        let all = offered(data)
        let enabled = all.filter { CapabilityCatalog.isOn(record, $0) }
        let available = all.filter { !CapabilityCatalog.isOn(record, $0) }
        Section("Enabled on \(instance.title) (\(enabled.count))") {
            if enabled.isEmpty {
                MutedNoteRow("None yet. Enable one below.")
            }
            ForEach(enabled) { spec in
                enabledRow(spec, record, data)
            }
        }
        Section("Available (\(available.count))") {
            ForEach(available) { spec in
                availableRow(spec, record)
            }
        }
        if instance.home {
            let ours = Set(all.map(\.component))
            let deployed = data.versions["components"].items.filter {
                $0["kind"].string == "capability" && !ours.contains($0["name"].string ?? "")
            }
            let platform = deployed.filter { $0["plumbing"].bool != true }
            let plumbing = deployed.filter { $0["plumbing"].bool == true }
            if !platform.isEmpty {
                Section {
                    ForEach(platform, id: \.self) { c in
                        platformRow(c)
                    }
                } header: {
                    Text("Platform capabilities")
                } footer: {
                    Text("Deployed with the platform.")
                }
            }
            if !plumbing.isEmpty {
                Section {
                    Button(showInternal ? "Hide internal" : "Show internal (\(plumbing.count))") {
                        showInternal.toggle()
                    }
                    if showInternal {
                        ForEach(plumbing, id: \.self) { c in
                            platformRow(c)
                        }
                    }
                } header: {
                    Text("Internal")
                } footer: {
                    Text("Platform plumbing: deployed with the platform, nothing to use directly.")
                }
            }
        }
    }

    private func platformRow(_ c: JSON) -> some View {
        ListRow(
            title: c["name"].string ?? "—",
            subtitle: c["worker"].string.map { "Served by \($0) · changes land through code" }
                ?? "Changes land through code",
            monoTitle: true
        ) {
            Text(c["version"].text ?? "—").idStyle().foregroundStyle(Theme.muted)
        }
    }

    private func component(_ data: CapabilitiesData, _ name: String) -> JSON {
        data.versions["components"].items.first { $0["name"].string == name } ?? .null
    }

    private func pin(_ data: CapabilitiesData, _ record: JSON, _ name: String) -> JSON {
        let mine = data.versions["instances"].items.first { $0["id"] == record["id"] } ?? .null
        return mine["pins"].items.first { $0["component"].string == name } ?? .null
    }

    private func step(_ data: CapabilitiesData, _ record: JSON, _ name: String) -> JSON {
        let mine = data.plan["instances"].items.first { $0["id"] == record["id"] } ?? .null
        return mine["steps"].items.first { $0["component"].string == name } ?? .null
    }

    private static func major(_ v: JSON) -> Int {
        let text = v.text ?? "0"
        return Int(text.split(separator: ".").first ?? "0") ?? 0
    }

    private func enabledRow(_ spec: CapabilitySpec, _ record: JSON, _ data: CapabilitiesData) -> some View {
        let latest = component(data, spec.component)["version"].text
        let pinRow = pin(data, record, spec.component)
        let stepRow = step(data, record, spec.component)
        let view = gated[spec.cap]
        var status: (String, Tone)
        if let view {
            status = ("Upgrade to see \(view.title)", Tone.warn)
        } else if pinRow["state"].string == "held" {
            status = ("Held by its pin", Tone.bad)
        } else if !stepRow.isNull {
            let major = Self.major(stepRow["to"]) > Self.major(stepRow["from"])
            status = (major ? "Major upgrade" : "Update available", Tone.info)
        } else {
            status = ("Current", Tone.ok)
        }
        let settings = RecordDiff.at(record, spec.field)
        let auditMissing = spec.linkedJobs && (settings["audit_bucket"].string ?? "").isEmpty
        if auditMissing { status = ("Audit setup required", Tone.warn) }
        let pill = status
        let pinText = pinRow.isNull
            ? "unpinned → \(latest ?? "newest")"
            : "\(pinRow["requirement"].text ?? "—") → \(pinRow["selected"].text ?? "none")"
        let latestText = latest.map { " · latest " + $0 } ?? ""
        return VStack(alignment: .leading, spacing: 6) {
            ListRow(title: spec.cap, subtitle: spec.what, monoTitle: true) {
                StatusPill(pill.0, tone: pill.1)
            }
            Text("pin " + pinText + latestText)
                .idStyle()
                .foregroundStyle(Theme.muted)
            // Up to seven capsules, so they wrap onto a second line on an iPhone.
            FlowLayout(spacing: 8) {
                if auditMissing {
                    Button("Set up audit storage") {
                        sheet = .jobs(spec, record, editing: true)
                    }
                    .buttonStyle(RowButtonStyle(.primary))
                }
                if !stepRow.isNull {
                    Button("Upgrade") { upgrade(spec, record, stepRow) }
                        .buttonStyle(RowButtonStyle(view != nil ? .primary : .quiet))
                }
                if !spec.fields(record).isEmpty || spec.cap == "mcp-federation" {
                    Button(spec.cap == "mcp-federation" ? "Manage trust" : "Configure") {
                        configure(spec, record)
                    }
                    .buttonStyle(RowButtonStyle(.quiet))
                }
                if spec.cap == "digitalocean" {
                    Button("Connection") { sheet = .connection(record, spaces: false) }
                        .buttonStyle(RowButtonStyle(.quiet))
                    if settings["droplet_health"].bool == true {
                        Button("Runner check") { sheet = .droplet(record) }
                            .buttonStyle(RowButtonStyle(.quiet))
                    }
                    if !settings["spaces"].isNull {
                        Button("Spaces") { sheet = .connection(record, spaces: true) }
                            .buttonStyle(RowButtonStyle(.quiet))
                    }
                }
                Button("Disable") { disable(spec, record) }
                    .buttonStyle(RowButtonStyle(.danger))
            }
        }
        .padding(.vertical, 4)
    }

    private func availableRow(_ spec: CapabilitySpec, _ record: JSON) -> some View {
        let need = spec.needs(record)
        let disabled = record[spec.field]["state"].string == "disabled"
        return VStack(alignment: .leading, spacing: 6) {
            ListRow(title: spec.cap, subtitle: spec.what, monoTitle: true) {
                StatusPill(disabled ? "Disabled" : "Off", tone: .neutral)
            }
            if let need {
                Text("Needs \(need)").font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            } else {
                Button("Enable") { enable(spec, record) }
                    .buttonStyle(RowButtonStyle(.quiet))
            }
        }
        .padding(.vertical, 4)
    }

    // MARK: Writes

    /// One record write: the exact diff, then the passkey, through `instance-put`.
    private func put(
        _ record: JSON, next: JSON, title: String, facts: [Fact], effect: String,
        destructive: Bool = false, confirmLabel: String
    ) {
        guard
            let made = CapabilityWrite.put(
                record, next: next, title: title, facts: facts, effect: effect,
                destructive: destructive, confirmLabel: confirmLabel,
                onDone: { Task { await reloadAll() } })
        else {
            toasts.show("Nothing changes.", tone: .info)
            return
        }
        request = made
    }

    /// Configure (or Manage trust): Jobs and MCP Federation open their own sheets.
    private func configure(_ spec: CapabilitySpec, _ record: JSON) {
        if spec.linkedJobs {
            sheet = .jobs(spec, record, editing: true)
        } else if spec.cap == "mcp-federation" {
            sheet = .federation(record, editing: true)
        } else {
            editing = CapabilityEdit(spec: spec, record: record, editing: true)
        }
    }

    private func enable(_ spec: CapabilitySpec, _ record: JSON) {
        if spec.linkedJobs {
            sheet = .jobs(spec, record, editing: false)
        } else if spec.cap == "mcp-federation" {
            sheet = .federation(record, editing: false)
        } else if spec.fields(record).isEmpty {
            put(
                record, next: CapabilityCatalog.with(record, spec, values: [:]),
                title: "Enable \(spec.cap) on \(instance.title)",
                facts: [Fact("Capability", spec.cap, mono: true)],
                effect: spec.effect
                    ?? "Adds \(spec.adds.joined(separator: ", ")) to this instance’s console, and \(spec.cap)’s tools to its MCP endpoint.",
                confirmLabel: "Enable with passkey")
        } else {
            editing = CapabilityEdit(spec: spec, record: record, editing: false)
        }
    }

    private func upgrade(_ spec: CapabilitySpec, _ record: JSON, _ step: JSON) {
        let suggested = step["suggested"]
        let pins = RecordDiff.setting(record["pins"], spec.component, suggested)
        let next = RecordDiff.setting(record, "pins", pins)
        var facts = [
            Fact("Capability", spec.cap, mono: true),
            Fact("Version", "\(step["from"].text ?? "held") → \(step["to"].text ?? "—")", mono: true),
        ]
        let work = step["work"].items.map { w -> String in
            var line = "\(w["version"].text ?? ""): \(w["summary"].text ?? "")"
            if let m = w["migration"].string, !m.isEmpty { line += " Migration: \(m)" }
            return line
        }
        if !work.isEmpty { facts.append(Fact("Breaking on the way", work.joined(separator: "\n"))) }
        let unlocks = gated[spec.cap].map { " It unlocks \($0.title)." } ?? ""
        put(
            record, next: next, title: "Upgrade \(spec.cap) on \(instance.title)", facts: facts,
            effect: "Moves the pin on \(spec.component) to \(suggested.text ?? "—"), which selects \(step["to"].text ?? "—").\(unlocks)",
            confirmLabel: "Upgrade with passkey")
    }

    private func disable(_ spec: CapabilitySpec, _ record: JSON) {
        let also = CapabilityCatalog.dependents(record, spec)
        let every = [spec] + also
        var facts = [Fact("Capability", spec.cap, mono: true)]
        if !also.isEmpty { facts.append(Fact("Also disables", also.map(\.cap).joined(separator: ", "))) }
        put(
            record, next: CapabilityCatalog.without(record, spec),
            title: "Disable \(spec.cap) on \(instance.title)", facts: facts,
            effect: "Removes \(every.flatMap(\.adds).joined(separator: ", ")) from this instance’s console and \(every.map(\.cap).joined(separator: " and ")) from its MCP tools. Nothing in the account is deleted.",
            destructive: true, confirmLabel: "Disable with passkey")
    }
}

/// Enable or Configure a capability that takes settings: the fields, the record diff as they
/// are typed, then the confirm sheet's passkey step (`instance-put`).
struct CapabilityEditSheet: View {
    let edit: CapabilityEdit
    let instanceTitle: String
    let onDone: () -> Void
    @State private var text: [String: String]

    init(edit: CapabilityEdit, instanceTitle: String, onDone: @escaping () -> Void) {
        self.edit = edit
        self.instanceTitle = instanceTitle
        self.onDone = onDone
        _text = State(initialValue: CapabilityCatalog.currentValues(edit.record, edit.spec))
    }

    private var fields: [CapabilityField] { edit.spec.fields(edit.record) }
    private var verb: String { edit.editing ? "Configure" : "Enable" }
    private var record: JSON { edit.record }

    private var next: JSON {
        CapabilityCatalog.with(record, edit.spec, values: CapabilityCatalog.values(text, fields: fields))
    }

    private var request: WriteRequest {
        let done = onDone
        return WriteRequest(
            title: "\(verb) \(edit.spec.cap) on \(instanceTitle)",
            subtitle: edit.spec.effect ?? (edit.editing
                ? "Changes how \(edit.spec.cap) is scoped. The console sections stay the same."
                : "Adds \(edit.spec.adds.joined(separator: ", ")) to this instance’s console, and \(edit.spec.cap)’s tools to its MCP endpoint."),
            facts: [
                Fact("Instance", record["id"].text ?? "—", mono: true),
                Fact("Capability", edit.spec.cap, mono: true),
            ],
            action: "instance-put",
            target: .string(record["id"].text ?? ""),
            confirmLabel: "\(verb) with passkey",
            onDone: { _ in done() })
    }

    var body: some View {
        ConfirmSheet(request, prepare: { try change() }) {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(fields, id: \.key) { f in
                    fieldView(f)
                }
                VStack(alignment: .leading, spacing: 6) {
                    Text("Record diff")
                        .font(Theme.Fonts.caption).textCase(.uppercase)
                        .foregroundStyle(Theme.muted)
                    let list = RecordDiff.changes(record, next)
                    if list.isEmpty {
                        Text("No changes yet.").font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                    } else {
                        DiffBlock(lines: RecordDiff.lines(record, list))
                    }
                }
            }
        }
    }

    private func binding(_ key: String) -> Binding<String> {
        Binding(get: { text[key] ?? "" }, set: { text[key] = $0 })
    }

    @ViewBuilder
    private func fieldView(_ f: CapabilityField) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(f.label).font(Theme.Fonts.meta.weight(.semibold)).foregroundStyle(Theme.ink)
            if let options = f.options {
                Picker(f.label, selection: binding(f.key)) {
                    if !options.contains(text[f.key] ?? "") {
                        Text(f.key == "site" ? "Choose a site" : "Choose").tag(text[f.key] ?? "")
                    }
                    ForEach(options, id: \.self) { o in
                        Text(f.labels?[o] ?? o).tag(o)
                    }
                }
                .pickerStyle(.menu)
            } else {
                TextField(f.label, text: binding(f.key))
                    .font(f.mono ? Theme.Fonts.idBody : Theme.Fonts.body)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .textFieldStyle(.roundedBorder)
            }
            if let hint = f.hint {
                Text(hint).font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            }
        }
    }

    private func change() throws -> JSON {
        let values = CapabilityCatalog.values(text, fields: fields)
        if let missing = CapabilityCatalog.missing(values, fields: fields) {
            throw InputError("Fill in \(missing.label.lowercased()).")
        }
        if let invalid = CapabilityCatalog.invalid(values, fields: fields) {
            throw InputError("Enter a valid \(invalid.label).")
        }
        let next = CapabilityCatalog.with(record, edit.spec, values: values)
        if RecordDiff.changes(record, next).isEmpty { throw InputError("Nothing changes.") }
        return RecordDiff.putChange(next)
    }
}
