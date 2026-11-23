import SwiftUI

/// Settings: who is signed in, access, agents and sign out. Reached from the platform's row on
/// the Instances tab.
struct SettingsView: View {
    let session: Session
    let status: StatusModel
    @State private var whoami = ToolModel("users_whoami")
    @State private var secrets = ToolModel("secrets_list")
    @State private var users = UsersModel()
    @State private var confirmSignOut = false

    var body: some View {
        Screen(
            errors: [whoami.error, secrets.error, users.error, status.loadError, status.actionError]
                .compactMap { $0 },
            refresh: { await reload() }
        ) {
            header
            Text("Access").font(AppFont.label).foregroundStyle(Tokens.muted)
            PlainCard {
                NavigationLink {
                    UsersView(session: session, model: users)
                } label: {
                    row("Users", detail: users.loaded ? "\(users.people.count)" : "", chevron: true)
                }
                .buttonStyle(.plain)
                Divider()
                NavigationLink {
                    SecretsView(session: session)
                } label: {
                    row("Secrets", detail: secretCount, chevron: true)
                }
                .buttonStyle(.plain)
            }
            Text("Agents").font(AppFont.label).foregroundStyle(Tokens.muted)
            PlainCard {
                Toggle(
                    isOn: Binding(
                        get: { status.paused },
                        set: { on in Task { await status.setPaused(on, session.client) } })
                ) {
                    Text("Pause all agents").font(AppFont.row).foregroundStyle(Tokens.ink)
                }
                .tint(Tokens.accent)
                .disabled(status.busy)
            }
            PlainCard {
                Button(action: signOutTapped) {
                    Text(confirmSignOut ? "Tap again to sign out" : "Sign out")
                        .font(AppFont.row)
                        .foregroundStyle(confirmSignOut ? Tokens.warn : Tokens.ink)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.visible, for: .navigationBar)
        .task { await reload() }
    }

    /// The signed-in user: avatar and name.
    private var header: some View {
        let dict = whoami.data as? [String: Any] ?? [:]
        let acting = (dict["acting_for"] as? [String: Any])?["name"] as? String
        let own = (dict["user"] as? [String: Any])?["name"] as? String
        let name = acting ?? own ?? "Signed in"
        let picture = dict["picture"] as? String
        return HStack(spacing: 14) {
            Avatar(name: name, initials: dict["initials"] as? String, picture: picture, size: 64)
            VStack(alignment: .leading, spacing: 2) {
                Text(name).font(AppFont.screenTitle).foregroundStyle(Tokens.ink)
                Text("Signed in").font(AppFont.meta).foregroundStyle(Tokens.muted)
            }
        }
    }

    /// How many secrets there are, or nothing before they load.
    private var secretCount: String {
        (secrets.data as? [[String: Any]]).map { "\($0.count)" } ?? ""
    }

    /// One row of a settings card: a name, a count and an optional chevron.
    private func row(_ title: String, detail: String, chevron: Bool) -> some View {
        HStack {
            Text(title).font(AppFont.row).foregroundStyle(Tokens.ink)
            Spacer()
            Text(detail).font(AppFont.row).foregroundStyle(Tokens.muted)
            if chevron { Image(systemName: "chevron.right").foregroundStyle(Tokens.muted) }
        }
        .contentShape(Rectangle())
    }

    /// The first tap arms the row for 3 seconds; the second signs out.
    private func signOutTapped() {
        if confirmSignOut {
            Task { await session.userSignOut() }
            return
        }
        confirmSignOut = true
        Task {
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            confirmSignOut = false
        }
    }

    /// Loads everything on the screen.
    private func reload() async {
        await whoami.load(session.client)
        await secrets.load(session.client)
        await users.load(session.client)
        await status.load(session.client)
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
            Text("Secrets").font(AppFont.screenTitle).foregroundStyle(Tokens.ink)
            if let rows = secrets.data as? [[String: Any]] {
                if rows.isEmpty {
                    PlainCard {
                        Text("No secrets.").font(AppFont.body).foregroundStyle(Tokens.muted)
                    }
                }
                ForEach(Array(rows.enumerated()), id: \.offset) { _, r in
                    PlainCard {
                        Text(r["name"] as? String ?? "Unnamed")
                            .font(AppFont.row).foregroundStyle(Tokens.ink)
                        Text(
                            "\((r["set"] as? Bool ?? false) ? "Set" : "Not set") · modified \(jsonText(r["modified"]))"
                        )
                        .font(AppFont.meta).foregroundStyle(Tokens.muted)
                    }
                }
            } else if secrets.data != nil {
                ErrorBanner(text: "secrets_list failed: the answer was not a list of secrets.")
            }
        }
        .navigationTitle("Secrets")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.visible, for: .navigationBar)
        .task { await secrets.load(session.client) }
    }
}
