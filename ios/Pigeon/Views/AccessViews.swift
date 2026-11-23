import SwiftUI

/// The Access tab: a list of its four sections.
struct AccessView: View {
    let session: Session

    var body: some View {
        ZStack {
            DotGrid()
            ScrollView {
                VStack(alignment: .leading, spacing: Tokens.spaceGap) {
                    Eyebrow(text: "Access")
                    link("Passkeys", "Your passkeys.") { PasskeysView(session: session) }
                    link("People", "Who can sign in, and how many passkeys each has.") {
                        PeopleView(session: session)
                    }
                    link("Delegation", "The apps and agents acting for each person.") {
                        DelegationView(session: session)
                    }
                    link("Secrets", "What is set and when it changed. Never the values.") {
                        SecretsView(session: session)
                    }
                    Button("Sign out") { Task { await session.userSignOut() } }
                        .font(.token(Tokens.typeButton))
                }
                .padding(Tokens.spacePage)
            }
        }
        .navigationTitle("Access")
        .navigationBarTitleDisplayMode(.inline)
    }

    private func link<D: View>(_ title: String, _ about: String, @ViewBuilder _ to: () -> D)
        -> some View
    {
        NavigationLink(destination: to()) {
            Card {
                CardTitle(text: title)
                Text(about)
            }
        }
        .buttonStyle(.plain)
    }
}

/// `users_list` as people with passkey counts.
struct PeopleView: View {
    let session: Session
    @State private var users = ToolModel("users_list")

    var body: some View {
        Screen(errors: [users.error].compactMap { $0 }, refresh: { await users.load(session.client) }) {
            Eyebrow(text: "People")
            if let dict = users.data as? [String: Any] {
                let counts = Directory.counts(dict)
                let people = Directory.users(dict).filter { $0["kind"] as? String == "standard" }
                if people.isEmpty { Card { Text("No people.") } }
                ForEach(Array(people.enumerated()), id: \.offset) { _, p in
                    Card {
                        CardTitle(text: p["name"] as? String ?? "Unnamed")
                        Text("Passkeys: \(counts[p["id"] as? String ?? ""] ?? 0)")
                        Text("Added \(whenText(p["created_at"] as? Double))")
                    }
                }
            } else if users.error == nil {
                Card { Text("Loading…") }
            }
        }
        .navigationTitle("People")
        .navigationBarTitleDisplayMode(.inline)
        .task { await users.load(session.client) }
    }
}

/// Reads of the `users_list` answer, parsed defensively.
enum Directory {
    /// The `users` array's objects; empty when it is missing.
    static func users(_ dict: [String: Any]) -> [[String: Any]] {
        dict["users"] as? [[String: Any]] ?? []
    }

    /// Passkey count by user id.
    static func counts(_ dict: [String: Any]) -> [String: Int] {
        var out: [String: Int] = [:]
        for row in dict["passkeys"] as? [[String: Any]] ?? [] {
            if let id = row["user_id"] as? String, let n = row["passkeys"] as? Int { out[id] = n }
        }
        return out
    }
}

/// Service users and apps, and whom each acts for.
struct DelegationView: View {
    let session: Session
    @State private var users = ToolModel("users_list")
    @State private var clients = ToolModel("oauth_clients")

    var body: some View {
        Screen(
            errors: [users.error, clients.error].compactMap { $0 },
            refresh: {
                await users.load(session.client)
                await clients.load(session.client)
            }
        ) {
            Eyebrow(text: "Delegation")
            if let dict = users.data as? [String: Any] {
                let all = Directory.users(dict)
                let names = Dictionary(
                    all.compactMap { u -> (String, String)? in
                        guard let id = u["id"] as? String else { return nil }
                        return (id, u["name"] as? String ?? id)
                    }, uniquingKeysWith: { a, _ in a })
                let links = dict["links"] as? [[String: Any]] ?? []
                let services = all.filter { $0["kind"] as? String != "standard" }
                if services.isEmpty { Card { Text("No service users.") } }
                ForEach(Array(services.enumerated()), id: \.offset) { _, s in
                    let id = s["id"] as? String ?? ""
                    let people = links.filter { $0["service_user_id"] as? String == id }
                        .compactMap { names[$0["standard_user_id"] as? String ?? ""] }
                    Card {
                        CardTitle(text: s["name"] as? String ?? "Unnamed")
                        Text(
                            people.isEmpty
                                ? "Acts for no one" : "Acts for \(people.joined(separator: ", "))")
                    }
                }
            } else if users.error == nil {
                Card { Text("Loading…") }
            }
            if clients.data != nil {
                Eyebrow(text: "Apps")
                JSONCards(value: clients.data)
            }
        }
        .navigationTitle("Delegation")
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await users.load(session.client)
            await clients.load(session.client)
        }
    }
}

/// `secrets_list`: names, whether set, and when changed. Never values.
struct SecretsView: View {
    let session: Session
    @State private var secrets = ToolModel("secrets_list")

    var body: some View {
        Screen(
            errors: [secrets.error].compactMap { $0 },
            refresh: { await secrets.load(session.client) }
        ) {
            Eyebrow(text: "Secrets")
            if let rows = secrets.data as? [[String: Any]] {
                if rows.isEmpty { Card { Text("No secrets.") } }
                ForEach(Array(rows.enumerated()), id: \.offset) { _, r in
                    Card {
                        CardTitle(text: r["name"] as? String ?? "Unnamed")
                        Text((r["set"] as? Bool ?? false) ? "Set" : "Not set")
                        Text("Owner: \(jsonText(r["owner"]))")
                        Text("Purpose: \(jsonText(r["purpose"]))")
                        Text("Modified: \(jsonText(r["modified"]))")
                    }
                }
            } else if secrets.data != nil {
                ErrorBanner(text: "secrets_list failed: the answer was not a list of secrets.")
            } else if secrets.error == nil {
                Card { Text("Loading…") }
            }
        }
        .navigationTitle("Secrets")
        .navigationBarTitleDisplayMode(.inline)
        .task { await secrets.load(session.client) }
    }
}
