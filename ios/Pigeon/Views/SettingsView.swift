import SwiftUI

/// Settings: the instances at the top, then a section named after the current instance with
/// who is signed in, access, agents and sign out (or, for a linked instance, its facts).
struct SettingsView: View {
    let session: Session
    let sessions: SessionsModel
    let onSwitchToPlatform: () -> Void
    @Environment(InstanceStore.self) private var store
    @State private var whoami = ToolModel("users_whoami")
    @State private var secrets = ToolModel("secrets_list")
    @State private var users = UsersModel()
    @State private var confirmSignOut = false

    /// Whether the current instance is the platform, which has the full settings.
    private var onPlatform: Bool { store.selected?.isPlatform ?? true }

    var body: some View {
        Screen(
            title: "Settings", root: true,
            errors: [
                store.error, whoami.error, secrets.error, users.error, sessions.loadError,
                sessions.actionError,
            ].compactMap { $0 },
            refresh: { await reload() }
        ) {
            CardSection("Instances") { instancesCard }
            CardSection(store.selected?.title ?? "Pigeon Platform") {
                VStack(alignment: .leading, spacing: Metrics.gap) {
                    if onPlatform {
                        platformSettings
                    } else {
                        InstanceFactsCard(store: store)
                        if store.platform != nil {
                            Button("Switch to Pigeon Platform", action: onSwitchToPlatform)
                                .buttonStyle(ChipButtonStyle())
                        }
                    }
                }
            }
        }
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

    // MARK: Instances

    /// Every instance in one grouped card; the current one is ticked, a tap switches to it.
    @ViewBuilder
    private var instancesCard: some View {
        if store.instances.isEmpty {
            PlainCard {
                Text(store.error == nil ? "Loading…" : "No instances.")
                    .font(AppFont.note).foregroundStyle(Tokens.muted)
            }
        } else {
            GroupedCard {
                ForEach(Array(store.instances.enumerated()), id: \.element.id) { index, instance in
                    if index > 0 { RowSeparator(inset: 66) }
                    instanceRow(instance)
                }
            }
        }
    }

    /// One instance row: tile, title with a tick when current, and the subtitle.
    private func instanceRow(_ instance: InstanceInfo) -> some View {
        let current = instance.id == store.selected?.id
        return Button {
            store.select(instance.id)
        } label: {
            HStack(spacing: 12) {
                Tile(size: 40, radius: 10, fill: current ? Tokens.accent : Tokens.chip) {
                    Text(String(instance.title.prefix(1)).uppercased())
                        .font(AppFont.initial)
                        .foregroundStyle(current ? Tokens.onAccent : Tokens.ink)
                }
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(instance.title)
                            .font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
                            .lineLimit(1)
                        if current {
                            Image(systemName: "checkmark")
                                .font(.system(size: 13, weight: .semibold))
                                .foregroundStyle(Tokens.accent)
                        }
                    }
                    Text(instance.subtitle)
                        .font(AppFont.meta).foregroundStyle(Tokens.muted)
                        .lineLimit(1).truncationMode(.tail)
                }
                Spacer(minLength: 0)
            }
            .padding(14)
            .frame(minHeight: 64)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    // MARK: Platform settings

    /// The platform's settings: signed-in card, access, agents, sign out.
    @ViewBuilder
    private var platformSettings: some View {
        profile
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
        GroupedCard {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Pause all agents").font(AppFont.row).foregroundStyle(Tokens.ink)
                    Text(
                        sessions.paused
                            ? "Paused: no run takes another turn"
                            : "Stops every run between turns"
                    )
                    .font(AppFont.meta).foregroundStyle(Tokens.muted)
                }
                Spacer(minLength: 8)
                Toggle(
                    "Pause all agents",
                    isOn: Binding(
                        get: { sessions.paused },
                        set: { on in Task { await sessions.setPaused(on, session.client) } })
                )
                .labelsHidden()
                .tint(Tokens.warn)
                .disabled(sessions.busy)
            }
            .padding(.horizontal, 16)
            .frame(minHeight: 56)
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

    /// One row of the access card: a name, a count and a chevron.
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

    /// Loads everything on the screen. The platform's data loads only on the platform.
    private func reload() async {
        await store.load(session.client)
        guard onPlatform else { return }
        await whoami.load(session.client)
        await secrets.load(session.client)
        await users.load(session.client)
        await sessions.load(session.client)
    }
}
