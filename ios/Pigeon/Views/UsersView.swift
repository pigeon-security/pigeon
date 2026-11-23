import SwiftUI

/// One passkey of a person.
struct PasskeyInfo: Identifiable {
    let id: String
    let name: String
    let createdMs: Double?
    let lastUsedMs: Double?
}

/// A service user or app that acts for a person.
struct Delegate: Identifiable {
    let id: String
    let name: String
    /// "app" or "service".
    let kind: String
    let addedMs: Double?
}

/// A person who can sign in.
struct Person: Identifiable {
    let id: String
    let name: String
    let initials: String?
    let picture: String?
    let role: String
    let addedMs: Double?
    let passkeyCount: Int
    let passkeys: [PasskeyInfo]
    let delegates: [Delegate]
}

/// Loads the people with `users_people`, or with `users_list` on a server without it.
@MainActor
@Observable
final class UsersModel {
    private(set) var people: [Person] = []
    private(set) var loaded = false
    private(set) var error: String?

    /// Fetches the people.
    func load(_ client: MCPClient) async {
        do {
            let data = try await client.call("users_people")
            let rows = (data as? [String: Any])?["people"] as? [[String: Any]] ?? []
            people = rows.compactMap(Self.person)
            loaded = true
            error = nil
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            await loadFallback(client)
        }
    }

    /// Builds the people from `users_list`: its standard users, passkey counts and links.
    private func loadFallback(_ client: MCPClient) async {
        do {
            let data = try await client.call("users_list")
            guard let dict = data as? [String: Any] else {
                throw PlatformError.badResponse("users_list shape")
            }
            let users = dict["users"] as? [[String: Any]] ?? []
            let links = dict["links"] as? [[String: Any]] ?? []
            var counts: [String: Int] = [:]
            for row in dict["passkeys"] as? [[String: Any]] ?? [] {
                if let id = row["user_id"] as? String, let n = num(row["passkeys"]) {
                    counts[id] = Int(n)
                }
            }
            people = users.filter { $0["kind"] as? String == "standard" }.compactMap { u in
                guard let id = u["id"] as? String else { return nil }
                let delegates = links.filter { $0["standard_user_id"] as? String == id }
                    .compactMap { link -> Delegate? in
                        guard let sid = link["service_user_id"] as? String,
                            let s = users.first(where: { $0["id"] as? String == sid })
                        else { return nil }
                        return Delegate(
                            id: sid, name: s["name"] as? String ?? sid, kind: "service",
                            addedMs: num(s["created_at"]))
                    }
                return Person(
                    id: id, name: u["name"] as? String ?? "Unnamed", initials: nil, picture: nil,
                    role: "Person", addedMs: num(u["created_at"]), passkeyCount: counts[id] ?? 0,
                    passkeys: [], delegates: delegates)
            }
            loaded = true
            error = nil
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            self.error = "users_people failed: \(error.localizedDescription)"
        }
    }

    /// Reads one element of `users_people`'s `people`; nil when it has no id.
    private static func person(_ p: [String: Any]) -> Person? {
        guard let id = p["id"] as? String else { return nil }
        let keys = (p["passkeys"] as? [[String: Any]] ?? []).compactMap { k -> PasskeyInfo? in
            guard let kid = k["id"] as? String else { return nil }
            return PasskeyInfo(
                id: kid, name: k["name"] as? String ?? "Passkey", createdMs: num(k["created_at"]),
                lastUsedMs: num(k["last_used_at"]))
        }
        let delegates = (p["acting_for_me"] as? [[String: Any]] ?? []).compactMap { d -> Delegate? in
            guard let did = d["id"] as? String else { return nil }
            return Delegate(
                id: did, name: d["name"] as? String ?? did, kind: d["kind"] as? String ?? "service",
                addedMs: num(d["added_ms"]))
        }
        return Person(
            id: id, name: p["name"] as? String ?? "Unnamed", initials: p["initials"] as? String,
            picture: p["picture"] as? String, role: p["role"] as? String ?? "Person",
            addedMs: num(p["added_ms"]), passkeyCount: keys.count, passkeys: keys,
            delegates: delegates)
    }
}

/// "d MMM yyyy" for a date in ms, or "unknown".
private func shortDate(_ ms: Double?) -> String {
    guard let ms else { return "unknown" }
    let f = DateFormatter()
    f.locale = Locale(identifier: "en_GB")
    f.dateFormat = "d MMM yyyy"
    return f.string(from: Date(timeIntervalSince1970: ms / 1000))
}

/// "Who can sign in": the people, each with their passkey count.
struct UsersView: View {
    let session: Session
    let model: UsersModel

    var body: some View {
        Screen(
            errors: [model.error].compactMap { $0 },
            side: Metrics.sideWide,
            refresh: { await model.load(session.client) }
        ) {
            if model.people.isEmpty && model.loaded {
                PlainCard {
                    Text("No people.").font(AppFont.note).foregroundStyle(Tokens.muted)
                }
            }
            CardSection("Who can sign in · \(model.people.count)") {
                GroupedCard {
                    ForEach(Array(model.people.enumerated()), id: \.element.id) { index, person in
                        if index > 0 { RowSeparator(inset: 68) }
                        NavigationLink {
                            UserDetailView(person: person)
                        } label: {
                            personRow(person)
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
            Footnote("Service users and apps appear under the person they act for.")
        }
        .navigationTitle("Users")
        .navigationBarTitleDisplayMode(.large)
        .instanceMenu()
        .task { await model.load(session.client) }
    }

    /// One person's row: avatar, name, passkey count and date, chevron.
    private func personRow(_ person: Person) -> some View {
        let keys = person.passkeyCount == 1 ? "1 passkey" : "\(person.passkeyCount) passkeys"
        return HStack(spacing: 12) {
            Avatar(name: person.name, initials: person.initials, picture: person.picture, size: 40)
            VStack(alignment: .leading, spacing: 2) {
                Text(person.name).font(AppFont.row).foregroundStyle(Tokens.ink)
                Text("\(keys) · added \(shortDate(person.addedMs))")
                    .font(AppFont.meta).foregroundStyle(Tokens.muted)
            }
            Spacer(minLength: 8)
            Chevron()
        }
        .padding(.horizontal, 16)
        .frame(minHeight: 64)
        .contentShape(Rectangle())
    }
}

/// One person: their passkeys and the apps and service users acting for them.
struct UserDetailView: View {
    let person: Person

    var body: some View {
        Screen(errors: [], side: Metrics.sideWide, refresh: {}) {
            VStack(spacing: 8) {
                Avatar(name: person.name, initials: person.initials, picture: person.picture, size: 76)
                Text(person.name).font(AppFont.name).foregroundStyle(Tokens.ink)
                Text("Standard user · added \(shortDate(person.addedMs))")
                    .font(AppFont.note).foregroundStyle(Tokens.muted)
            }
            .frame(maxWidth: .infinity)
            .padding(.top, 8)
            CardSection("Passkeys · \(person.passkeyCount)") {
                GroupedCard {
                    if person.passkeys.isEmpty {
                        plainRow("\(person.passkeyCount) passkeys")
                    }
                    ForEach(Array(person.passkeys.enumerated()), id: \.element.id) { index, key in
                        if index > 0 { RowSeparator(inset: 58) }
                        tileRow(
                            symbol: "key", fill: Tokens.accentSoft, colour: Tokens.accent,
                            title: key.name, sub: passkeySub(key))
                    }
                }
            }
            CardSection("Delegation · acts for \(person.name)") {
                GroupedCard {
                    if person.delegates.isEmpty { plainRow("Nothing acts for this person.") }
                    ForEach(Array(person.delegates.enumerated()), id: \.element.id) { index, d in
                        if index > 0 { RowSeparator(inset: 58) }
                        tileRow(
                            symbol: "cpu", fill: Tokens.chip, colour: Tokens.ink, title: d.name,
                            sub: "\(d.kind == "app" ? "App" : "Service user") · added \(shortDate(d.addedMs))")
                    }
                }
            }
            Footnote("Passkeys are added and removed in the console.")
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        .instanceMenu()
    }

    /// "Added 3 Mar 2026 · last used 5 Mar 2026" for a passkey.
    private func passkeySub(_ key: PasskeyInfo) -> String {
        let used = key.lastUsedMs.map { shortDate($0) } ?? "never"
        return "Added \(shortDate(key.createdMs)) · last used \(used)"
    }

    /// A muted single-line row, for an empty list.
    private func plainRow(_ text: String) -> some View {
        Text(text)
            .font(AppFont.row).foregroundStyle(Tokens.muted)
            .padding(.horizontal, 16)
            .frame(maxWidth: .infinity, minHeight: 48, alignment: .leading)
    }

    /// A 60 pt row with a 30 pt symbol tile, a title and a sub line.
    private func tileRow(
        symbol: String, fill: Color, colour: Color, title: String, sub: String
    ) -> some View {
        HStack(spacing: 12) {
            Tile(size: 30, radius: 8, fill: fill) {
                Image(systemName: symbol).font(.system(size: 14)).foregroundStyle(colour)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(AppFont.row).foregroundStyle(Tokens.ink)
                Text(sub).font(AppFont.meta).foregroundStyle(Tokens.muted)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .frame(minHeight: 60)
    }
}
