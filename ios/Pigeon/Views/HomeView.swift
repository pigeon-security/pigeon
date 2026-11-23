import SwiftUI

/// The facts of the selected instance (website, site, repository) as one grouped card.
struct InstanceFactsCard: View {
    let store: InstanceStore

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

/// The chip button that switches back to the platform.
struct ChipButtonStyle: ButtonStyle {
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

    var body: some View {
        Screen(
            title: "Home", trailing: AnyView(InstanceSwitcher()), root: true,
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
                CardSection(store.selected?.title ?? "This instance") {
                    InstanceFactsCard(store: store)
                }
            }
            if store.platform != nil {
                Button("Switch to Pigeon Platform", action: onSwitchToPlatform)
                    .buttonStyle(ChipButtonStyle())
            }
        }
    }
}
