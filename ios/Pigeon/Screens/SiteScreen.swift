import SwiftUI

/// Site: the views publishing and site_passkeys add to an instance, as the web's `views/site.js`.
/// - Sites: the sites on the approved record (`instances_list`).
/// - Deployments: the immutable releases (`site_releases`), live first; Publish is `site-deploy`
///   and Roll back the destructive `site-rollback`.
/// - Visitors: each passkey-protected hostname (`app_site_hosts`) with its visitors
///   (`app_site_visitors`), loaded with the view; Invite is `visitor-invite` (its link is shown
///   once) and Remove is `visitor-revoke`.
struct SiteScreen: View {
    let context: AreaContext

    var body: some View {
        switch context.view?.id ?? "sites" {
        case "deployments": SiteDeploymentsView(context: context)
        case "visitors": SiteVisitorsView(context: context)
        default: SiteSitesView(context: context)
        }
    }
}

// MARK: - Sites

struct SiteSitesView: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var state: Loadable<JSON?> = .loading

    var body: some View {
        List {
            ViewPickerSection(context: context)
            UpgradeSection(context: context)
            Section {
                MutedNoteRow("The sites on \(context.instance.title)’s approved record. They change with the record; publish and roll back in Deployments.")
            }
            switch state {
            case .loading:
                Section { LoadingRows() }
            case .failed(let message):
                Section { ErrorState(message: message) { await load() } }
            case .loaded(let record):
                let r = record ?? JSON.null
                let live = Self.live(r)
                let sites = Self.sites(r)
                if sites.isEmpty {
                    Section { EmptyState(title: "No sites yet", systemImage: "globe", message: "This instance’s record has no sites yet.") }
                } else {
                    ForEach(sites, id: \.self) { site in
                        siteSection(site, published: live != nil && site["name"].string == live)
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
    }

    /// The site publishing names, when publishing is enabled.
    static func live(_ r: JSON) -> String? {
        r["publishing"]["state"].string == "enabled" ? r["publishing"]["site"].string : nil
    }

    /// The record's sites, the one publishing names first.
    static func sites(_ r: JSON) -> [JSON] {
        let live = Self.live(r)
        let all = r["sites"].items
        return all.filter { $0["name"].string == live } + all.filter { $0["name"].string != live }
    }

    private func load() async {
        do {
            let r = try await InstanceRecordLoader.load(api, id: context.instance.id)
            state = .loaded(r)
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            if state.value == nil { state = .failed(error.localizedDescription) }
        }
    }

    private static func targetText(_ t: JSON) -> String {
        switch t["kind"].string {
        case "subdomain": return "\(t["label"].string ?? "?").\(ConsoleFormat.platformZone)"
        case "preview": return "Preview"
        default: return t["hostname"].string ?? "—"
        }
    }

    private func siteSection(_ s: JSON, published: Bool) -> some View {
        let passkey = s["auth"].string == "passkey"
        let allow = s["allow"].items.compactMap(\.string)
        return Section {
            ListRow(title: s["name"].string ?? "—", subtitle: s["build"].string) {
                if published { StatusPill("Published", tone: .accent) }
            }
            KeyValueRow("Bucket", s["bucket"].string ?? "—", mono: true)
            if let j = s["jurisdiction"].string { KeyValueRow("Jurisdiction", j) }
            ForEach(s["targets"].items, id: \.self) { t in
                ListRow(title: Self.targetText(t), subtitle: (t["kind"].string ?? "").capitalized, monoTitle: true) {
                    if t["kind"].string == "zone" {
                        let on = t["enabled"].bool ?? false
                        StatusPill(on ? "Route on" : "Route off", tone: on ? .ok : .warn)
                    }
                }
            }
            if s["targets"].items.isEmpty {
                KeyValueRow("Targets", "None")
            }
            ListRow(title: "Visitors", subtitle: passkey ? (allow.contains("site") ? "Invited visitors" : "\(allow.count) allowed") : nil) {
                StatusPill(passkey ? "Passkey" : "Public", tone: passkey ? .info : .neutral)
            }
        }
    }
}

// MARK: - Deployments

/// Which sheet Deployments shows.
private struct PublishTarget: Identifiable {
    let site: String?
    var id: String { site ?? "" }
}

struct SiteDeploymentsView: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var releases = OpResource<JSON>("site_releases")
    @State private var request: WriteRequest?
    @State private var publishing: PublishTarget?

    private var id: String { context.instance.id }

    var body: some View {
        List {
            ViewPickerSection(context: context)
            UpgradeSection(context: context)
            Section {
                HStack(alignment: .center, spacing: 10) {
                    MutedNoteRow(about)
                    Spacer(minLength: 8)
                    Button("Publish…") { publishing = PublishTarget(site: releases.value?["site"].string) }
                        .buttonStyle(RowButtonStyle(.primary))
                        .disabled(releases.value == nil)
                }
            }
            LoadableSection(
                state: releases.state,
                emptyTitle: "No releases yet",
                emptyMessage: "Publish makes the first one.",
                isEmpty: { $0["releases"].items.isEmpty },
                retry: { await load() }
            ) { data in
                Section("Releases") {
                    ForEach(data["releases"].items, id: \.self) { r in
                        releaseRow(r, data)
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
        .confirmSheet($request)
        .sheet(item: $publishing) { target in
            PublishSheet(instance: id, site: target.site) {
                Task { await load() }
            }
        }
    }

    private var about: String {
        guard let data = releases.value, let site = data["site"].string else {
            return "Immutable releases of this instance’s site, live first."
        }
        let shown = data["releases"].items.count
        if let total = data["total"].int, total > shown {
            return "Immutable releases of \(site), live first (the newest \(shown) of \(total))."
        }
        return "Immutable releases of \(site), live first."
    }

    private func load() async {
        await releases.load(api, args: ["instance": .string(id)])
    }

    private func releaseRow(_ r: JSON, _ data: JSON) -> some View {
        let live = r["live"].bool ?? false
        var meta: [String] = [ConsoleFormat.short(r["commit"].string).isEmpty ? "—" : ConsoleFormat.short(r["commit"].string)]
        if let t = r["template"].string {
            meta.append([t, r["template_version"].string].compactMap { $0 }.joined(separator: " "))
        }
        return VStack(alignment: .leading, spacing: 4) {
            ListRow(title: ConsoleFormat.short(r["release"].string, 20), subtitle: meta.joined(separator: " · "), monoTitle: true) {
                if live {
                    StatusPill("Live", tone: .ok)
                } else {
                    Button("Roll back") { rollback(r, data) }
                        .buttonStyle(RowButtonStyle(.danger))
                }
            }
            if let url = r["base_url"].string, let link = URL(string: url), url.hasPrefix("https://") {
                Link(destination: link) {
                    Text(url).idStyle().foregroundStyle(Theme.accent)
                }
            }
        }
    }

    private func rollback(_ r: JSON, _ data: JSON) {
        let site = data["site"].string
        let release = r["release"].string ?? ""
        var lines: [DiffLine] = []
        if let live = data["live"].string { lines.append(.remove("live: \(live)")) }
        lines.append(.add("live: \(release)"))
        request = WriteRequest(
            title: "Roll back \(site ?? "the site")",
            subtitle: "Visitors get this release as soon as it is live. Nothing is deleted: the release live now stays here, and you can roll forward to it the same way.",
            facts: [
                Fact("Instance", id, mono: true),
                Fact("Site", site ?? "—"),
                Fact("Release", release, mono: true),
                Fact("Commit", r["commit"].string ?? "—", mono: true),
            ],
            lines: lines,
            action: "site-rollback",
            target: .string(id),
            change: ["release": .string(release)],
            destructive: true,
            confirmLabel: "Roll back with passkey",
            onDone: { _ in Task { await load() } })
    }
}

/// Publish: the revision to build, then the passkey through `site-deploy`.
struct PublishSheet: View {
    let instance: String
    let site: String?
    let onDone: () -> Void
    @State private var revision = "main"

    private var request: WriteRequest {
        let done = onDone
        return WriteRequest(
            title: "Publish \(site ?? "the site")",
            subtitle: "Builds the site at this revision, keeps it as a new immutable release and makes it live. The release live now stays available to roll back to.",
            facts: [Fact("Instance", instance, mono: true), Fact("Site", site ?? "—")],
            lines: [.add("new release built from the revision"), .note("live moves to the new release")],
            action: "site-deploy",
            target: .string(instance),
            confirmLabel: "Publish with passkey",
            onDone: { _ in done() })
    }

    var body: some View {
        ConfirmSheet(request, prepare: { try change() }) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Revision").font(Theme.Fonts.meta.weight(.semibold)).foregroundStyle(Theme.ink)
                TextField("main", text: $revision)
                    .font(Theme.Fonts.idBody)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .textFieldStyle(.roundedBorder)
                Text("A commit or a branch. Publishing builds it as it is now.")
                    .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            }
        }
    }

    /// `[A-Za-z0-9._/-]`, as the web checks a revision.
    private static func revisionScalar(_ c: Unicode.Scalar) -> Bool {
        switch c {
        case "a"..."z", "A"..."Z", "0"..."9", ".", "_", "/", "-": return true
        default: return false
        }
    }

    private func change() throws -> JSON {
        let value = revision.trimmingCharacters(in: .whitespacesAndNewlines)
        let rev = value.isEmpty ? "main" : value
        let allowed = rev.count <= 200 && rev.unicodeScalars.allSatisfy(Self.revisionScalar)
        guard allowed else { throw InputError("A revision is a commit or a branch name.") }
        return ["rev": .string(rev)]
    }
}

// MARK: - Visitors

struct SiteVisitorsView: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @Environment(\.openArea) private var openArea
    @State private var hosts = OpResource<JSON>("app_site_hosts")
    @State private var request: WriteRequest?
    @State private var invite: VisitorInvite?

    var body: some View {
        List {
            ViewPickerSection(context: context)
            UpgradeSection(context: context)
            LoadableSection(
                state: hosts.state,
                emptyTitle: "No protected hostnames yet",
                emptyMessage: "A hostname appears here once a site gated by visitor passkeys is published on a platform subdomain.",
                isEmpty: { $0["hosts"].items.isEmpty },
                retry: { await load() }
            ) { data in
                ForEach(data["hosts"].items, id: \.self) { h in
                    VisitorHostSection(
                        instance: context.instance.id,
                        host: h["host"].string ?? "",
                        site: h["site"].string ?? "",
                        request: $request,
                        invite: $invite)
                }
            }
            Section {
                Button("Visitor passkeys come from site_passkeys; change it in Capabilities") {
                    openArea("capabilities")
                }
                .font(Theme.Fonts.meta)
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
        .confirmSheet($request)
        .sheet(item: $invite) { i in
            InviteVisitorSheet(target: i.target, host: i.host, whereFacts: i.whereFacts, onDone: i.onDone)
        }
    }

    private func load() async {
        await hosts.load(api, args: ["instance": .string(context.instance.id)])
    }
}

/// A hostname to invite a visitor to, and what to reload after.
struct VisitorInvite: Identifiable {
    let id = UUID()
    let target: JSON
    let host: String
    let whereFacts: [Fact]
    let onDone: () -> Void
}

/// One protected hostname and its visitors, loaded with the view.
struct VisitorHostSection: View {
    let instance: String
    let host: String
    let site: String
    @Binding var request: WriteRequest?
    @Binding var invite: VisitorInvite?
    @Environment(ConsoleAPI.self) private var api
    @State private var visitors = OpResource<JSON>("app_site_visitors")

    private var target: JSON {
        ["instance": .string(instance), "site": .string(site), "host": .string(host)]
    }

    private var whereFacts: [Fact] {
        [Fact("Hostname", host, mono: true), Fact("Site", site), Fact("Instance", instance, mono: true)]
    }

    var body: some View {
        Section {
            switch visitors.state {
            case .loading:
                LoadingRows(count: 2)
            case .failed(let message):
                SectionRetryError(message: message) { await load() }
            case .loaded(let data):
                let list = data["visitors"].items
                if list.isEmpty {
                    MutedNoteRow("No visitors invited for this hostname.")
                }
                ForEach(list, id: \.self) { v in
                    let enrolled = (v["enrolled"].double ?? 0) != 0 || v["enrolled"].bool == true
                    ListRow(
                        title: v["name"].string ?? "—",
                        subtitle: enrolled ? "Passkey registered" : "Invite pending"
                    ) {
                        HStack(spacing: 6) {
                            if !enrolled { StatusPill("Pending", tone: .warn) }
                            Button("Remove") { revoke(v) }
                                .buttonStyle(RowButtonStyle(.danger))
                        }
                    }
                }
                if data["can_invite"].bool == true {
                    Button("Invite visitor…") { startInvite() }
                        .buttonStyle(RowButtonStyle(.primary))
                } else {
                    MutedNoteRow("This hostname’s allowlist does not admit new visitors, so none can be invited.")
                }
            }
        } header: {
            HStack(spacing: 6) {
                Text(host).font(.system(.footnote, design: .monospaced)).textCase(nil)
                Text("· \(site)").textCase(nil)
            }
            .lineLimit(1)
        }
        .task { await load() }
    }

    private func startInvite() {
        let resource = visitors
        let api = api
        let args = target
        invite = VisitorInvite(target: target, host: host, whereFacts: whereFacts) {
            Task { await resource.load(api, args: args) }
        }
    }

    private func load() async {
        await visitors.load(api, args: target)
    }

    private func revoke(_ v: JSON) {
        let name = v["name"].string ?? "this visitor"
        request = WriteRequest(
            title: "Remove \(name)",
            subtitle: "\(name) can no longer sign in to \(host). To let them back, invite them again.",
            facts: [Fact("Visitor", name), Fact("Id", v["id"].text ?? "—", mono: true)] + whereFacts,
            lines: [.remove("visitor \(name) and their passkeys")],
            action: "visitor-revoke",
            target: target,
            change: ["user": v["id"]],
            destructive: true,
            confirmLabel: "Remove with passkey",
            onDone: { _ in Task { await load() } })
    }
}

/// Invite a visitor: their name, then the passkey through `visitor-invite`; the one-time link
/// comes back as the show-once content.
struct InviteVisitorSheet: View {
    let target: JSON
    let host: String
    let whereFacts: [Fact]
    let onDone: () -> Void
    @State private var name = ""

    private var request: WriteRequest {
        let done = onDone
        return WriteRequest(
            title: "Invite a visitor to \(host)",
            subtitle: "Makes a one-time link for them to register a passkey for this hostname. It is shown once, next.",
            facts: whereFacts,
            lines: [.add("visitor, invited")],
            action: "visitor-invite",
            target: target,
            confirmLabel: "Create invite with passkey",
            onDone: { _ in done() })
    }

    var body: some View {
        ConfirmSheet(request, prepare: { try change() }) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Visitor name").font(Theme.Fonts.meta.weight(.semibold)).foregroundStyle(Theme.ink)
                TextField("Their name", text: $name)
                    .font(Theme.Fonts.body)
                    .autocorrectionDisabled()
                    .textFieldStyle(.roundedBorder)
                Text("Shown to you here; the visitor registers their own passkey.")
                    .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            }
        }
    }

    private func change() throws -> JSON {
        let who = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !who.isEmpty else { throw InputError("Give the visitor a name.") }
        guard who.count <= 64 else { throw InputError("A visitor’s name is at most 64 characters.") }
        return ["name": .string(who)]
    }
}
