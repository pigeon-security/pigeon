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
            if store.instances.isEmpty && store.error == nil {
                PlainCard {
                    Text("Loading…").font(AppFont.note).foregroundStyle(Tokens.muted)
                }
            }
            if !store.instances.isEmpty {
                GroupedCard {
                    ForEach(Array(store.instances.enumerated()), id: \.element.id) { index, instance in
                        if index > 0 { RowSeparator(inset: 66) }
                        row(instance)
                    }
                }
            }
            Footnote(
                "Tap an instance to switch to it. The instance button at the top of every screen switches too."
            )
        }
        .navigationTitle("Instances")
        .navigationBarTitleDisplayMode(.large)
        .task { await store.load(session.client) }
    }

    /// One instance row: tile, title and sub line, and Settings on the platform's row.
    private func row(_ instance: InstanceInfo) -> some View {
        let current = instance.id == store.selected?.id
        return HStack(spacing: 12) {
            Button {
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
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if instance.isPlatform {
                NavigationLink {
                    SettingsView(session: session, status: status)
                } label: {
                    Text("Settings")
                }
                .buttonStyle(ActionButtonStyle(.secondary, small: true, fill: false))
            }
        }
        .padding(14)
        .frame(minHeight: 64)
    }
}

/// The chip button that switches back to the platform.
private struct ChipButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(AppFont.button)
            .foregroundStyle(Tokens.ink)
            .frame(maxWidth: .infinity)
            .frame(height: 44)
            .background(RoundedRectangle(cornerRadius: 12).fill(Tokens.chip))
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

/// The Home tab of a linked instance: nothing to act on, its facts, and a way back.
struct HomeView: View {
    let session: Session
    let store: InstanceStore
    let onSwitchToPlatform: () -> Void

    /// One fact row: a name, a value and a link when the value is a web address.
    private struct Fact: Identifiable {
        let id: String
        let value: String
        let url: URL?
    }

    /// The facts the selected instance has.
    private var facts: [Fact] {
        guard let i = store.selected else { return [] }
        var list: [Fact] = []
        if let site = i.website, let url = URL(string: site) {
            list.append(Fact(id: "Website", value: site, url: url))
        }
        if let site = i.site { list.append(Fact(id: "Site", value: site, url: nil)) }
        if let repo = i.repository { list.append(Fact(id: "Repository", value: repo, url: nil)) }
        return list
    }

    var body: some View {
        Screen(
            errors: [store.error].compactMap { $0 },
            refresh: { await store.load(session.client) }
        ) {
            PlainCard(radius: Metrics.cardRadiusLarge) {
                VStack(spacing: 8) {
                    Tile(size: 64, radius: 16, fill: Tokens.chip) {
                        Image(systemName: "globe")
                            .font(.system(size: 26)).foregroundStyle(Tokens.ink)
                    }
                    Text("Nothing to act on here yet")
                        .font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
                        .multilineTextAlignment(.center)
                    Text(
                        "This instance has no agents, approvals or costs that the app can show yet. Its site and repository are below."
                    )
                    .font(AppFont.body).foregroundStyle(Tokens.muted)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 20)
            }
            if store.selected != nil {
                CardSection("This instance") {
                    GroupedCard {
                        if facts.isEmpty {
                            Text("No facts are known yet.")
                                .font(AppFont.row).foregroundStyle(Tokens.muted)
                                .padding(.horizontal, 16)
                                .frame(maxWidth: .infinity, minHeight: 48, alignment: .leading)
                        }
                        ForEach(Array(facts.enumerated()), id: \.element.id) { index, fact in
                            if index > 0 { RowSeparator(inset: 16) }
                            factRow(fact)
                        }
                    }
                }
            }
            if store.platform != nil {
                Button("Switch to Pigeon Platform", action: onSwitchToPlatform)
                    .buttonStyle(ChipButtonStyle())
            }
        }
        .navigationTitle(store.selected?.title ?? "Home")
        .navigationBarTitleDisplayMode(.large)
    }

    /// A fact: its name at the left, its value at the right.
    private func factRow(_ fact: Fact) -> some View {
        HStack(spacing: 12) {
            Text(fact.id).font(AppFont.row).foregroundStyle(Tokens.ink)
            Spacer(minLength: 8)
            if let url = fact.url {
                Link(fact.value, destination: url)
                    .font(AppFont.body).tint(Tokens.accent)
                    .lineLimit(1).truncationMode(.middle)
            } else {
                Text(fact.value)
                    .font(AppFont.body).foregroundStyle(Tokens.muted)
                    .lineLimit(1).truncationMode(.middle)
            }
        }
        .padding(.horizontal, 16)
        .frame(minHeight: 48)
    }
}
