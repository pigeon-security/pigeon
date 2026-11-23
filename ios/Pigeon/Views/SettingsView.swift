import SwiftUI

/// Settings: who is signed in, access, agents and sign out. Reached from the platform's row on
/// the Instances tab.
struct SettingsView: View {
    let session: Session
    let status: StatusModel
    @Environment(InstanceStore.self) private var store
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
            Text(store.platform?.title ?? "Pigeon Platform")
                .font(AppFont.body).foregroundStyle(Tokens.muted)
                .padding(.leading, Metrics.sectionIndent)
            profile
            CardSection("Access") {
                GroupedCard {
                    NavigationLink {
                        UsersView(session: session, model: users)
                    } label: {
                        row("Users", detail: users.loaded ? "\(users.people.count)" : "")
                    }
                    .buttonStyle(.plain)
                    RowSeparator(inset: 16)
                    NavigationLink {
                        SecretsView(session: session)
                    } label: {
                        row("Secrets", detail: secretCount)
                    }
                    .buttonStyle(.plain)
                }
            }
            CardSection("Agents") {
                GroupedCard {
                    HStack(spacing: 12) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Pause all agents").font(AppFont.row).foregroundStyle(Tokens.ink)
                            Text(
                                status.paused
                                    ? "Paused: no run takes another turn"
                                    : "Stops every run between turns"
                            )
                            .font(AppFont.meta).foregroundStyle(Tokens.muted)
                        }
                        Spacer(minLength: 8)
                        Toggle(
                            "Pause all agents",
                            isOn: Binding(
                                get: { status.paused },
                                set: { on in Task { await status.setPaused(on, session.client) } })
                        )
                        .labelsHidden()
                        .tint(Tokens.warn)
                        .disabled(status.busy)
                    }
                    .padding(.horizontal, 16)
                    .frame(minHeight: 56)
                }
            }
            GroupedCard {
                Button {
                    confirmSignOut = true
                } label: {
                    Text("Sign out")
                        .font(AppFont.row).foregroundStyle(Tokens.warn)
                        .padding(.horizontal, 16)
                        .frame(maxWidth: .infinity, minHeight: 48, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.large)
        .instanceMenu()
        .confirmationDialog(
            "Sign out of Pigeon?", isPresented: $confirmSignOut, titleVisibility: .visible
        ) {
            Button("Sign out", role: .destructive) {
                Task { await session.userSignOut() }
            }
            Button("Cancel", role: .cancel) {}
        }
        .task { await reload() }
    }

    /// The signed-in user: avatar and name.
    private var profile: some View {
        let dict = whoami.data as? [String: Any] ?? [:]
        let acting = (dict["acting_for"] as? [String: Any])?["name"] as? String
        let own = (dict["user"] as? [String: Any])?["name"] as? String
        let name = acting ?? own ?? "Signed in"
        let picture = dict["picture"] as? String
        return PlainCard {
            HStack(spacing: 12) {
                Avatar(name: name, initials: dict["initials"] as? String, picture: picture, size: 48)
                VStack(alignment: .leading, spacing: 2) {
                    Text(name).font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
                    Text("Standard user · this iPhone")
                        .font(AppFont.meta).foregroundStyle(Tokens.muted)
                }
            }
        }
    }

    /// How many secrets there are, or nothing before they load.
    private var secretCount: String {
        (secrets.data as? [[String: Any]]).map { "\($0.count)" } ?? ""
    }

    /// One row of the Access card: a name, a count and a chevron.
    private func row(_ title: String, detail: String) -> some View {
        HStack(spacing: 8) {
            Text(title).font(AppFont.row).foregroundStyle(Tokens.ink)
            Spacer()
            Text(detail).font(AppFont.row).foregroundStyle(Tokens.muted).monospacedDigit()
            Chevron()
        }
        .padding(.horizontal, 16)
        .frame(minHeight: 48)
        .contentShape(Rectangle())
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
            if let rows = secrets.data as? [[String: Any]] {
                if rows.isEmpty {
                    PlainCard {
                        Text("No secrets.").font(AppFont.note).foregroundStyle(Tokens.muted)
                    }
                } else {
                    GroupedCard {
                        ForEach(Array(rows.enumerated()), id: \.offset) { index, r in
                            if index > 0 { RowSeparator(inset: 16) }
                            VStack(alignment: .leading, spacing: 2) {
                                Text(r["name"] as? String ?? "Unnamed")
                                    .font(AppFont.row).foregroundStyle(Tokens.ink)
                                Text(
                                    "\((r["set"] as? Bool ?? false) ? "Set" : "Not set") · modified \(jsonText(r["modified"]))"
                                )
                                .font(AppFont.meta).foregroundStyle(Tokens.muted)
                            }
                            .padding(.horizontal, 16)
                            .frame(maxWidth: .infinity, minHeight: 56, alignment: .leading)
                        }
                    }
                }
            } else if secrets.data != nil {
                ErrorBanner(text: "secrets_list failed: the answer was not a list of secrets.")
            }
        }
        .navigationTitle("Secrets")
        .navigationBarTitleDisplayMode(.large)
        .instanceMenu()
        .task { await secrets.load(session.client) }
    }
}
