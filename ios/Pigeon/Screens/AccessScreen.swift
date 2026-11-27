import SwiftUI

/// Access (platform-wide, home only): Secrets, Tokens and People, as the web console's
/// `access.js`. Every write goes through the confirm sheet; a token or invite link comes back in
/// the show-once content.
///
/// - Secrets: `secrets_list`, `credentials_expiry`; `secret-set` (the value is typed in the
///   confirm sheet and never kept).
/// - Tokens: `app_proxy_tokens`; `token-issue`, `token-revoke`.
/// - People: `app_people`, each person with their passkeys and the apps acting for them, rolled
///   up one per real app (`app_groups`), and the apps acting for no one under them for an admin;
///   `invite`, `passkey-remove`, `app-disconnect` (one client at a time). The console's
///   `app-group-disconnect` and `user-remove` are its own: the server refuses them by bearer, so
///   the app neither disconnects a whole app of several clients at once nor removes a person.
struct AccessScreen: View {
    let context: AreaContext

    var body: some View {
        switch context.viewID.wrappedValue {
        case "tokens": AccessTokens(context: context)
        case "people": AccessPeople(context: context)
        default: AccessSecrets(context: context)
        }
    }
}

// MARK: - Secrets

struct AccessSecrets: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var state: Loadable<Listing> = .loading
    @State private var editing: Secret?

    struct Secret: Identifiable, Hashable {
        let name: String
        let owner: String?
        let purpose: String?
        let isSet: Bool
        let modified: String?
        var expiry: Expiry?
        var id: String { name }

        init?(_ j: JSON) {
            guard let name = j["name"].string else { return nil }
            self.name = name
            owner = j["owner"].string
            purpose = j["purpose"].string
            isSet = j["set"].bool ?? false
            modified = j["modified"].text.map { String($0.prefix(10)) }
        }
    }

    struct Expiry: Hashable {
        let status: String
        let expires: String?
        let daysLeft: Int?
        let refused: Bool

        var pill: (String, Tone)? {
            switch status {
            case "ok": return (expires.map { "Expires \($0)" } ?? "No expiry", Tone.neutral)
            case "soon":
                if daysLeft == 0 { return ("Expires today", Tone.warn) }
                return ("Expires in \(daysLeft ?? 0) days", Tone.warn)
            case "expired": return (refused ? "Refused by issuer" : "Expired", Tone.bad)
            case "missing": return nil
            default: return ("Unknown", Tone.neutral)
            }
        }
    }

    struct Listing {
        let missing: [Secret]
        let present: [Secret]
        let undeclared: [Secret]
    }

    var body: some View {
        List {
            PlatformViewPicker(context: context)
            Section {
                Text("Values are never shown. Setting one asks for your passkey, bound to that name and a hash of the value.")
                    .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            }
            LoadableSection(
                state: state, emptyTitle: "No secrets",
                emptyMessage: "No capability declares a secret.",
                isEmpty: { $0.missing.isEmpty && $0.present.isEmpty && $0.undeclared.isEmpty },
                retry: { await load() }
            ) { listing in
                if !listing.missing.isEmpty {
                    Section("Missing") {
                        ForEach(listing.missing) { secretRow($0) }
                    }
                }
                if !listing.present.isEmpty {
                    Section("Set") {
                        ForEach(listing.present) { secretRow($0) }
                    }
                }
                if !listing.undeclared.isEmpty {
                    Section {
                        DisclosureGroup {
                            ForEach(listing.undeclared) { s in
                                ListRow(title: s.name, subtitle: "Listed only. No capability reads it.", monoTitle: true) {
                                    Text(s.modified ?? "—").font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                                }
                            }
                        } label: {
                            Text(
                                "\(PlatformFormat.count(listing.undeclared.count, "secret", "secrets")) in the store that no capability declares"
                            )
                            .font(Theme.Fonts.body)
                        }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
        .sheet(item: $editing) { secret in
            SecretSetSheet(secret: secret) { Task { await load() } }
        }
    }

    private func secretRow(_ s: Secret) -> some View {
        Button {
            editing = s
        } label: {
            ListRow(
                title: s.name,
                subtitle: [s.owner, s.purpose, s.modified.map { "changed \($0)" }]
                    .compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · "),
                monoTitle: true
            ) {
                if !s.isSet {
                    StatusPill("Missing", tone: .bad)
                } else if let pill = s.expiry?.pill {
                    StatusPill(pill.0, tone: pill.1)
                } else {
                    StatusPill("Set", tone: .ok)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityHint(s.isSet ? "Replace the value" : "Set the value")
    }

    private func load() async {
        do {
            let list = try await api.op("secrets_list")
            let expiry = try? await api.op("credentials_expiry")
            var expiries: [String: Expiry] = [:]
            for r in expiry?["credentials"].items ?? [] where r["kind"].string == "secret" {
                guard let name = r["name"].string else { continue }
                expiries[name] = Expiry(
                    status: r["status"].string ?? "unknown", expires: r["expires"].string,
                    daysLeft: r["days_left"].int, refused: r["expiry"]["kind"].string == "refused")
            }
            let rows: [Secret] = (list.array ?? list["secrets"].items).compactMap { (j: JSON) -> Secret? in
                guard var s = Secret(j) else { return nil }
                s.expiry = expiries[s.name]
                return s
            }
            let declared = rows.filter { ($0.owner ?? "").isEmpty == false }
                .sorted { $0.name < $1.name }
            state = .loaded(
                Listing(
                    missing: declared.filter { !$0.isSet },
                    present: declared.filter { $0.isSet },
                    undeclared: rows.filter { ($0.owner ?? "").isEmpty }))
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            if state.value == nil { state = .failed(error.localizedDescription) }
        }
    }
}

/// The confirm sheet for `secret-set`, with the value typed in it. Begin carries only the
/// value's digest; the client sends the value once, at finish.
private struct SecretSetSheet: View {
    let secret: AccessSecrets.Secret
    let onDone: () -> Void
    @State private var value = ""

    var body: some View {
        ConfirmSheet(request, prepare: {
            let v = value.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !v.isEmpty else { throw InputError("Type the value first.") }
            return ["value": .string(v)]
        }) {
            FormField(
                label: secret.isSet ? "New value" : "Value",
                hint: "Your passkey is bound to this name and a hash of this value. The value is never shown or kept in the app."
            ) {
                SecureField("Value", text: $value)
                    .font(Theme.Fonts.idBody)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
            }
        }
    }

    private var request: WriteRequest {
        var facts = [Fact("Secret", secret.name, mono: true)]
        if let owner = secret.owner { facts.append(Fact("Owner", owner)) }
        facts.append(Fact("Purpose", secret.purpose ?? "—"))
        return WriteRequest(
            title: secret.isSet ? "Replace secret" : "Set secret",
            subtitle: secret.isSet ? "The new value replaces the stored one at once." : nil,
            facts: facts,
            action: "secret-set",
            target: .string(secret.name),
            confirmLabel: "Save with passkey",
            onDone: { _ in onDone() })
    }
}

// MARK: - Tokens

struct AccessTokens: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var state: Loadable<[Token]> = .loading
    @State private var issuing = false
    @State private var request: WriteRequest?

    struct Token: Identifiable, Hashable {
        let id: String
        let label: String
        let person: String?
        let createdAt: Double?
        let expiresAt: Double

        init?(_ j: JSON) {
            guard let id = j["id"].string, let expires = j["expires_at"].double else { return nil }
            self.id = id
            label = j["label"].string ?? id
            person = j["person"].string
            createdAt = j["created_at"].double
            expiresAt = expires
        }

        var live: Bool { expiresAt > nowMs() }
        var daysLeft: Int { Int(((expiresAt - nowMs()) / 86_400_000).rounded(.down)) }
    }

    var body: some View {
        List {
            PlatformViewPicker(context: context)
            Section {
                Text("A proxy token lets a cloud session reach the platform's git and Cloudflare proxies, and nothing else. Each is shown once, when issued.")
                    .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                Button {
                    issuing = true
                } label: {
                    Label("Issue token…", systemImage: "plus")
                }
                .font(Theme.Fonts.body)
            }
            LoadableSection(
                state: state, emptyTitle: "No proxy tokens",
                emptyMessage: "Issue one for a cloud session that needs the proxies.",
                isEmpty: { $0.isEmpty }, retry: { await load() }
            ) { tokens in
                let live = tokens.filter { $0.live }
                let ended = tokens.filter { !$0.live }
                Section("Live") {
                    if live.isEmpty {
                        Text("No live proxy tokens.").font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                    }
                    ForEach(live) { t in
                        Button {
                            request = revokeRequest(t)
                        } label: {
                            ListRow(title: t.label, subtitle: "\(t.id) · \(t.person ?? "someone removed")") {
                                let days = t.daysLeft
                                StatusPill(
                                    days <= 0 ? "Ends today" : PlatformFormat.count(days, "day left", "days left"),
                                    tone: days <= 7 ? .warn : .ok)
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityHint("Revoke this token")
                        .swipeActions(edge: .trailing) {
                            Button("Revoke", role: .destructive) { request = revokeRequest(t) }
                        }
                    }
                }
                if !ended.isEmpty {
                    Section("Expired in the last 14 days") {
                        ForEach(ended) { t in
                            ListRow(title: t.label, subtitle: "\(t.id) · \(t.person ?? "someone removed")") {
                                Text(PlatformFormat.day(t.expiresAt))
                                    .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                            }
                        }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
        .confirmSheet($request)
        .sheet(isPresented: $issuing) {
            IssueTokenSheet { Task { await load() } }
        }
    }

    private func revokeRequest(_ t: Token) -> WriteRequest {
        WriteRequest(
            title: "Revoke proxy token",
            subtitle: "Sessions using it lose proxy access at once.",
            facts: [
                Fact("Token", t.label), Fact("Id", t.id, mono: true),
                Fact("Issued by", t.person ?? "someone removed"),
                Fact("Expires", PlatformFormat.day(t.expiresAt)),
            ],
            action: "token-revoke",
            target: .string(t.id),
            destructive: true,
            confirmLabel: "Revoke with passkey",
            onDone: { _ in Task { await load() } })
    }

    private func load() async {
        do {
            let data = try await api.op("app_proxy_tokens")
            let all = data["tokens"].items.compactMap { Token($0) }
            state = .loaded(all.sorted { $0.expiresAt > $1.expiresAt })
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            if state.value == nil { state = .failed(error.localizedDescription) }
        }
    }
}

/// Issue a proxy token: a label and a lifetime, then the confirm sheet, then the token once.
private struct IssueTokenSheet: View {
    let onDone: () -> Void
    @State private var label = ""
    @State private var days = 30
    @State private var problem: String?
    @State private var request: WriteRequest?

    var body: some View {
        if let request {
            ConfirmSheet(request)
        } else {
            ReviewForm(
                title: "Issue proxy token",
                subtitle: "The token is shown once, after your passkey. Put it in the cloud session's environment then.",
                problem: problem, review: { review() }
            ) {
                FormField(label: "Label", hint: "Names the cloud session or machine that will hold it.") {
                    TextField("ci-gateway session", text: $label)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                }
                FormField(label: "Lifetime", hint: "1 to 90 days.") {
                    Stepper(PlatformFormat.count(days, "day", "days"), value: $days, in: 1...90)
                }
            }
        }
    }

    private func review() {
        let text = label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else {
            problem = "Give the token a label."
            return
        }
        guard text.count <= 64 else {
            problem = "A label is at most 64 characters."
            return
        }
        request = WriteRequest(
            title: "Issue proxy token",
            subtitle: "Shown once, after your passkey.",
            facts: [Fact("Label", text), Fact("Lifetime", PlatformFormat.count(days, "day", "days"))],
            action: "token-issue",
            target: .string(text),
            change: ["days": .number(Double(days))],
            confirmLabel: "Issue with passkey",
            onDone: { _ in onDone() })
    }
}

// MARK: - People

/// What `app_people` answers: the people, their passkeys, and the apps acting for them, rolled
/// up one per real app and person (`app_groups`, src/capabilities/src/delegates.rs), with each
/// app's clients and service users as its members.
struct PeopleListing {
    struct Passkey: Identifiable, Hashable {
        let id: String
        let name: String
        let createdAt: Double?
        let lastUsedAt: Double?
    }

    /// One role an app's clients take, such as the AI Gateway's planner and executor.
    struct Role: Identifiable, Hashable {
        let id: String
        let label: String
    }

    /// One client or service user of an app.
    struct Member: Identifiable, Hashable {
        let id: String
        let name: String
        let kind: String
        let role: String?
        let redirectHost: String?
        let tokens: Int
        let lastUsedAt: Double?
    }

    /// One real app acting for one person, or for no one.
    struct App: Identifiable, Hashable {
        let app: String
        let label: String
        let person: String?
        let kinds: [String]
        let roles: [Role]
        let clients: Int
        let tokens: Int
        let lastUsedAt: Double?
        let members: [Member]
        var id: String { "\(app)|\(person ?? "")" }

        init?(_ j: JSON) {
            guard let app = j["app"].string else { return nil }
            self.app = app
            label = j["label"].string ?? app
            person = j["person"].string
            kinds = j["kinds"].items.compactMap { $0.string }
            roles = j["roles"].items.compactMap { (r: JSON) -> Role? in
                guard let id = r["id"].string else { return nil }
                return Role(id: id, label: r["label"].string ?? id)
            }
            let members = j["members"].items.compactMap { (m: JSON) -> Member? in
                guard let id = m["id"].string else { return nil }
                return Member(
                    id: id, name: m["name"].string ?? id, kind: m["kind"].string ?? "oauth",
                    role: m["role"].string, redirectHost: m["redirect_host"].string,
                    tokens: m["tokens"].int ?? 0, lastUsedAt: m["last_used_at"].double)
            }
            self.members = members
            clients = j["clients"].int ?? members.count
            tokens = j["tokens"].int ?? 0
            lastUsedAt = j["last_used_at"].double
        }

        var kindText: String { kinds.map(PeopleListing.kindText).joined(separator: ", ") }

        /// The label of the role `id`, as the app names it.
        func roleLabel(_ id: String?) -> String? {
            guard let id else { return nil }
            return roles.first { $0.id == id }?.label ?? id
        }
    }

    struct Person: Identifiable, Hashable {
        let id: String
        let name: String
        let initials: String
        let isMe: Bool
        let isAdmin: Bool
        let passkeys: [Passkey]
        let apps: [App]

        var lastSignIn: Double? { passkeys.compactMap { $0.lastUsedAt }.max() }
    }

    /// Whether the one asking is an admin, from the read; the server checks every write again.
    let meAdmin: Bool
    let people: [Person]
    /// The apps acting for no one: an admin's to see and disconnect, under the people.
    let unassigned: [App]

    static func kindText(_ kind: String) -> String {
        switch kind {
        case "oauth": return "OAuth app"
        case "service": return "Service user"
        case "leftover": return "No client"
        default: return kind
        }
    }

    init(_ data: JSON) {
        let me = data["me"].string
        meAdmin = data["me_admin"].bool ?? false
        let groups = data["app_groups"].items.compactMap { App($0) }
        people = data["people"].items.compactMap { (p: JSON) -> Person? in
            guard let id = p["id"].string else { return nil }
            let name = p["name"].string ?? id
            let keys = p["passkeys"].items.compactMap { (k: JSON) -> Passkey? in
                guard let kid = k["id"].string else { return nil }
                return Passkey(
                    id: kid, name: k["name"].string ?? "Unnamed passkey",
                    createdAt: k["created_at"].double, lastUsedAt: k["last_used_at"].double)
            }
            return Person(
                id: id, name: name, initials: p["initials"].string ?? initialsOf(name),
                isMe: id == me, isAdmin: p["admin"].bool ?? false, passkeys: keys,
                apps: groups.filter { $0.person == id })
        }
        .sorted { a, b in a.isMe && !b.isMe }
        unassigned = groups.filter { $0.person == nil }
    }
}

struct AccessPeople: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var state: Loadable<PeopleListing> = .loading
    @State private var inviting = false
    @State private var request: WriteRequest?

    var body: some View {
        List {
            PlatformViewPicker(context: context)
            Section {
                Text("Who can sign in to the platform and approve with a passkey, and the apps acting for them. Expand a person for their passkeys and apps.")
                    .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                if state.value?.meAdmin == true {
                    Button {
                        inviting = true
                    } label: {
                        Label("Invite person…", systemImage: "person.badge.plus")
                    }
                    .font(Theme.Fonts.body)
                }
            }
            LoadableSection(
                state: state, emptyTitle: "No people yet", isEmpty: { $0.people.isEmpty },
                retry: { await load() }
            ) { listing in
                Section("People") {
                    ForEach(listing.people) { person in
                        DisclosureGroup {
                            if person.passkeys.isEmpty {
                                Text("No passkeys registered.").font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                            }
                            ForEach(person.passkeys) { key in
                                passkeyRow(key, of: person, meAdmin: listing.meAdmin)
                            }
                            if person.apps.isEmpty {
                                Text("No apps act for them.").font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                            }
                            ForEach(person.apps) { app in
                                appRow(app, for: person.name, meAdmin: listing.meAdmin)
                            }
                        } label: {
                            personRow(person)
                        }
                    }
                }
                if listing.meAdmin && !listing.unassigned.isEmpty {
                    Section {
                        ForEach(listing.unassigned) { app in
                            appRow(app, for: nil, meAdmin: true)
                        }
                    } header: {
                        Text("Apps acting for no one")
                    } footer: {
                        Text("Only admins see this section.")
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
        .confirmSheet($request)
        .sheet(isPresented: $inviting) {
            InviteSheet { Task { await load() } }
        }
    }

    private func personRow(_ p: PeopleListing.Person) -> some View {
        HStack(spacing: 10) {
            Text(p.initials)
                .font(.footnote.weight(.semibold))
                .foregroundStyle(Theme.ink)
                .frame(width: 30, height: 30)
                .background(Theme.hover, in: Circle())
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 6) {
                    Text(p.name).font(Theme.Fonts.body).foregroundStyle(Theme.ink).lineLimit(1)
                    if p.isMe { StatusPill("You", tone: .accent) }
                    if p.isAdmin { StatusPill("Admin", tone: .neutral) }
                }
                Text(personDetail(p)).font(Theme.Fonts.meta).foregroundStyle(Theme.muted).lineLimit(1)
            }
        }
    }

    private func personDetail(_ p: PeopleListing.Person) -> String {
        var parts = [PlatformFormat.count(p.passkeys.count, "passkey", "passkeys")]
        parts.append("last sign-in \(p.lastSignIn.map { PlatformFormat.day($0) } ?? "never")")
        if !p.apps.isEmpty { parts.append(PlatformFormat.count(p.apps.count, "app", "apps")) }
        return parts.joined(separator: " · ")
    }

    private func passkeyRow(
        _ key: PeopleListing.Passkey, of person: PeopleListing.Person, meAdmin: Bool
    ) -> some View {
        ListRow(
            title: key.name,
            subtitle: "Added \(PlatformFormat.day(key.createdAt)) · last used \(key.lastUsedAt.map { PlatformFormat.day($0) } ?? "never")"
        ) {
            if person.passkeys.count <= 1 {
                Text("Only passkey").font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            } else if person.isMe || meAdmin {
                Button("Remove") { request = removeRequest(key, of: person) }
                    .buttonStyle(RowButtonStyle(.danger))
            }
        }
    }

    /// One app acting for a person (or for no one). An app with one client disconnects it with
    /// `app-disconnect`; one with several expands to its clients, each disconnected on its own.
    /// `app-group-disconnect`, which disconnects every client at once, is the console's only: the
    /// server refuses it by bearer.
    @ViewBuilder
    private func appRow(_ a: PeopleListing.App, for personName: String?, meAdmin: Bool) -> some View {
        if a.members.count > 1 {
            DisclosureGroup {
                ForEach(a.members) { m in
                    ListRow(title: m.name, subtitle: clientDetail(m, of: a)) {
                        if meAdmin {
                            Button("Disconnect") { request = disconnectRequest(m, of: a, for: personName) }
                                .buttonStyle(RowButtonStyle(.danger))
                        }
                    }
                }
            } label: {
                ListRow(title: a.label, subtitle: appDetail(a))
            }
        } else {
            ListRow(title: a.label, subtitle: appDetail(a)) {
                if meAdmin, let m = a.members.first {
                    Button("Disconnect") { request = disconnectRequest(m, of: a, for: personName) }
                        .buttonStyle(RowButtonStyle(.danger))
                }
            }
        }
    }

    private func appDetail(_ a: PeopleListing.App) -> String {
        var parts = [String]()
        if !a.kinds.isEmpty { parts.append(a.kindText) }
        parts.append(PlatformFormat.count(a.clients, "client", "clients"))
        parts.append(PlatformFormat.count(a.tokens, "token", "tokens"))
        parts.append("last active \(a.lastUsedAt.map { PlatformFormat.day($0) } ?? "never")")
        if !a.roles.isEmpty { parts.append(a.roles.map { $0.label.lowercased() }.joined(separator: ", ")) }
        return parts.joined(separator: " · ")
    }

    private func clientDetail(_ m: PeopleListing.Member, of a: PeopleListing.App) -> String {
        var parts = [PeopleListing.kindText(m.kind)]
        if let role = a.roleLabel(m.role) { parts.append(role.lowercased()) }
        if let host = m.redirectHost { parts.append(host) }
        parts.append(PlatformFormat.count(m.tokens, "token", "tokens"))
        parts.append("last active \(m.lastUsedAt.map { PlatformFormat.day($0) } ?? "never")")
        return parts.joined(separator: " · ")
    }

    private func removeRequest(_ key: PeopleListing.Passkey, of person: PeopleListing.Person) -> WriteRequest {
        WriteRequest(
            title: "Remove passkey",
            subtitle: "That device can no longer sign in or approve.",
            facts: [
                Fact("Person", person.name), Fact("Passkey", key.name),
                Fact("Added", PlatformFormat.day(key.createdAt)),
            ],
            action: "passkey-remove",
            target: .string(key.id),
            destructive: true,
            confirmLabel: "Remove with passkey",
            onDone: { _ in Task { await load() } })
    }

    /// Disconnects one client of `a`: the whole app when it is its only one.
    private func disconnectRequest(
        _ m: PeopleListing.Member, of a: PeopleListing.App, for personName: String?
    ) -> WriteRequest {
        let only = a.members.count <= 1
        return WriteRequest(
            title: only ? "Disconnect app" : "Disconnect client",
            subtitle: only
                ? "Revokes its tokens and its link, so it acts for no one. Signing in again with a passkey reconnects it. Proxy tokens a person issued stay."
                : "Revokes its tokens and its link, so it acts for no one. The app's other clients stay connected.",
            facts: [
                Fact("App", a.label), Fact("Client", m.name), Fact("Id", m.id, mono: true),
                Fact("Acting for", personName ?? "No one"), Fact("Live tokens", String(m.tokens)),
            ],
            action: "app-disconnect",
            target: .string(m.id),
            destructive: true,
            confirmLabel: "Disconnect with passkey",
            onDone: { _ in Task { await load() } })
    }

    private func load() async {
        do {
            state = .loaded(PeopleListing(try await api.op("app_people")))
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            if state.value == nil { state = .failed(error.localizedDescription) }
        }
    }
}

/// Invite a person: their name, then the confirm sheet, then the one-time link once.
private struct InviteSheet: View {
    let onDone: () -> Void
    @State private var name = ""
    @State private var problem: String?
    @State private var request: WriteRequest?

    var body: some View {
        if let request {
            ConfirmSheet(request)
        } else {
            ReviewForm(
                title: "Invite person",
                subtitle: "They register their own passkey from the link, which works once, within 15 minutes.",
                problem: problem, review: { review() }
            ) {
                FormField(label: "Name") {
                    TextField("Their name", text: $name)
                        .textInputAutocapitalization(.words)
                }
            }
        }
    }

    private func review() {
        let text = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else {
            problem = "Give them a name first."
            return
        }
        guard text.count <= 64 else {
            problem = "A name is at most 64 characters."
            return
        }
        request = WriteRequest(
            title: "Invite person",
            subtitle: "The invite link is shown once, after your passkey.",
            facts: [Fact("Name", text)],
            action: "invite",
            target: .string(text),
            confirmLabel: "Create invite with passkey",
            onDone: { _ in onDone() })
    }
}
