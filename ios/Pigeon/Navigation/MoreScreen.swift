import SwiftUI

/// What the More tab pushes: an area that is not in the bar, or the tab editor.
enum MoreRoute: Hashable {
    case area(String)
    case editTabs
}

/// The More tab: the selected instance's areas that are not in the bar, in the person's order.
/// Edit opens the tab editor.
struct MoreScreen: View {
    let manifest: ConsoleManifest
    let layout: TabLayout
    @Environment(ShellState.self) private var shell
    @Environment(ManifestStore.self) private var store

    var body: some View {
        @Bindable var shell = shell
        NavigationStack(path: $shell.morePath) {
            List {
                Section {
                    ForEach(layout.overflow.compactMap(manifest.area)) { area in
                        NavigationLink(value: MoreRoute.area(area.id)) {
                            Label {
                                Text(area.title).font(Theme.Fonts.body).foregroundStyle(Theme.ink)
                            } icon: {
                                Image(systemName: AreaSymbol.name(for: area.id))
                                    .foregroundStyle(Theme.accent)
                            }
                        }
                        .badge(area.id == "inbox" ? store.inboxCount : 0)
                    }
                    if layout.overflow.isEmpty {
                        Text("Every area is in the tab bar.")
                            .font(Theme.Fonts.body).foregroundStyle(Theme.muted)
                    }
                } footer: {
                    Text("Tap Edit to choose which areas sit in the tab bar, and their order.")
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle("More")
            .navigationBarTitleDisplayMode(.large)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Edit") { shell.morePath.append(MoreRoute.editTabs) }
                }
            }
            .navigationDestination(for: MoreRoute.self) { route in
                switch route {
                case .area(let id):
                    if let area = manifest.area(id) {
                        AreaRoot(area: area, instance: manifest.instance)
                    } else {
                        ContentUnavailableView(
                            "Not on \(manifest.instance.title)", systemImage: "square.dashed",
                            description: Text("This instance has no area called \(id).")
                        )
                    }
                case .editTabs:
                    EditTabsView(manifest: manifest)
                }
            }
        }
    }
}
