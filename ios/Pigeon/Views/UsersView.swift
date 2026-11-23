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

/// "Who can sign in": the people, each with their passkey count.
struct UsersView: View {
    let session: Session
    let model: UsersModel

    var body: some View {
        Screen(
            errors: [model.error].compactMap { $0 },
            refresh: { await model.load(session.client) }
        ) {
            Text("Who can sign in · \(model.people.count)")
                .font(AppFont.screenTitle).foregroundStyle(Tokens.ink)
            if model.people.isEmpty && model.loaded {
                PlainCard {
                    Text("No people.").font(AppFont.body).foregroundStyle(Tokens.muted)
                }
            }
            ForEach(model.people) { person in
                NavigationLink {
                    UserDetailView(person: person)
                } label: {
                    PlainCard {
                        HStack(spacing: 12) {
                            Avatar(name: person.name, initials: person.initials, picture: person.picture)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(person.name).font(AppFont.row).foregroundStyle(Tokens.ink)
                                Text(
                                    "\(person.passkeyCount) passkeys · added \(dateText(person.addedMs))"
                                )
                                .font(AppFont.meta).foregroundStyle(Tokens.muted)
                            }
                            Spacer()
                            Image(systemName: "chevron.right").foregroundStyle(Tokens.muted)
                        }
                    }
                }
                .buttonStyle(.plain)
            }
            Text("Service users and apps appear under the person they act for.")
                .font(AppFont.meta).foregroundStyle(Tokens.muted)
        }
        .navigationTitle("Users")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.visible, for: .navigationBar)
        .task { await model.load(session.client) }
    }
}

/// One person: their passkeys and the apps and service users acting for them.
struct UserDetailView: View {
    let person: Person

    var body: some View {
        Screen(errors: [], refresh: {}) {
            HStack(spacing: 14) {
                Avatar(name: person.name, initials: person.initials, picture: person.picture, size: 64)
                VStack(alignment: .leading, spacing: 2) {
                    Text(person.name).font(AppFont.screenTitle).foregroundStyle(Tokens.ink)
                    Text("\(person.role) · added \(dateText(person.addedMs))")
                        .font(AppFont.meta).foregroundStyle(Tokens.muted)
                }
            }
            Text("Passkeys · \(person.passkeyCount)")
                .font(AppFont.label).foregroundStyle(Tokens.muted)
            ForEach(person.passkeys) { key in
                PlainCard {
                    Text(key.name).font(AppFont.row).foregroundStyle(Tokens.ink)
                    Text(
                        "Added \(dateText(key.createdMs)) · last used \(key.lastUsedMs.map { dateText($0) } ?? "never")"
                    )
                    .font(AppFont.meta).foregroundStyle(Tokens.muted)
                }
            }
            Text("Delegation · acts for \(person.name)")
                .font(AppFont.label).foregroundStyle(Tokens.muted)
            if person.delegates.isEmpty {
                PlainCard {
                    Text("Nothing acts for this person.")
                        .font(AppFont.body).foregroundStyle(Tokens.muted)
                }
            }
            ForEach(person.delegates) { d in
                PlainCard {
                    Text(d.name).font(AppFont.row).foregroundStyle(Tokens.ink)
                    Text("\(d.kind == "app" ? "App" : "Service user") · added \(dateText(d.addedMs))")
                        .font(AppFont.meta).foregroundStyle(Tokens.muted)
                }
            }
        }
        .navigationTitle(person.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.visible, for: .navigationBar)
    }
}
