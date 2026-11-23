import SwiftUI

/// The Instances tab: every instance, the selected one ticked. Tap a row to switch to it.
struct InstancesView: View {
    let session: Session
    let store: InstanceStore
    let status: StatusModel

    var body: some View {
        Screen(
            errors: [store.error].compactMap { $0 },
            refresh: { await store.load(session.client) }
        ) {
            TitleRow(title: "Instances")
            if store.instances.isEmpty && store.error == nil {
                PlainCard {
                    Text("Loading…").font(AppFont.body).foregroundStyle(Tokens.muted)
                }
            }
            ForEach(store.instances) { instance in
                PlainCard {
                    HStack(alignment: .top) {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(instance.title).font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
                            Text(instance.id).font(AppFont.meta).foregroundStyle(Tokens.muted)
                            Text(detail(instance)).font(AppFont.meta).foregroundStyle(Tokens.muted)
                        }
                        Spacer()
                        if instance.id == store.selected?.id {
                            Image(systemName: "checkmark").foregroundStyle(Tokens.accent)
                        }
                    }
                    .contentShape(Rectangle())
                    .onTapGesture { store.select(instance.id) }
                    if instance.isPlatform {
                        NavigationLink {
                            SettingsView(session: session, status: status)
                        } label: {
                            Text("Settings")
                                .font(AppFont.button)
                                .foregroundStyle(Tokens.accent)
                                .padding(.vertical, 10)
                                .padding(.horizontal, 18)
                                .background(
                                    RoundedRectangle(cornerRadius: Tokens.radiusButton)
                                        .strokeBorder(Tokens.accent, lineWidth: 1.5))
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
        }
        .toolbar(.hidden, for: .navigationBar)
        .task { await store.load(session.client) }
    }

    /// "Home account · pigeon.dev": the account kind, then the zone when there is one.
    private func detail(_ i: InstanceInfo) -> String {
        let account = i.isPlatform ? "Home account" : "Linked account"
        guard let zone = i.zones.first else { return account }
        return "\(account) · \(zone)"
    }
}

/// The Home tab of a linked instance: nothing to act on, its facts, and a way back.
struct HomeView: View {
    let session: Session
    let store: InstanceStore
    let onSwitchToPlatform: () -> Void

    var body: some View {
        Screen(
            errors: [store.error].compactMap { $0 },
            refresh: { await store.load(session.client) }
        ) {
            TitleRow(title: store.selected?.title ?? "Home")
            PlainCard {
                Text("Nothing to act on here yet")
                    .font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
            }
            if let i = store.selected {
                PlainCard {
                    Text("Facts").font(AppFont.label).foregroundStyle(Tokens.muted)
                    if let site = i.website, let url = URL(string: site) {
                        fact("Website") {
                            Link(site, destination: url).font(AppFont.body).tint(Tokens.accent)
                        }
                    }
                    if let site = i.site { fact("Site") { plain(site) } }
                    if let repo = i.repository { fact("Repository") { plain(repo) } }
                    if i.website == nil && i.site == nil && i.repository == nil {
                        plain("No facts are known yet.")
                    }
                }
            }
            if store.platform != nil {
                Button("Switch to Pigeon Platform", action: onSwitchToPlatform)
                    .buttonStyle(PrimaryButtonStyle())
            }
        }
        .toolbar(.hidden, for: .navigationBar)
    }

    /// A fact: its muted name, then its value.
    private func fact<V: View>(_ name: String, @ViewBuilder _ value: () -> V) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(name).font(AppFont.meta).foregroundStyle(Tokens.muted)
            value()
        }
    }

    /// A line of body text.
    private func plain(_ text: String) -> some View {
        Text(text).font(AppFont.body).foregroundStyle(Tokens.ink)
    }
}
