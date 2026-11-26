import SwiftUI

/// The tab editor, for the selected instance: which areas sit in the bar (up to three), their
/// order, and the order of the rest in More. Changes apply at once, so the bar updates
/// underneath.
struct EditTabsView: View {
    let manifest: ConsoleManifest
    @Environment(TabLayoutStore.self) private var layouts

    private var layout: TabLayout { layouts.layout(for: manifest.instance.id, areas: manifest.areas) }

    private func change(_ edit: (inout TabLayout) -> Void) {
        withAnimation {
            layouts.update(manifest.instance.id, areas: manifest.areas, edit)
        }
    }

    var body: some View {
        let layout = self.layout
        List {
            Section {
                ForEach(layout.pinned, id: \.self) { id in
                    row(id, pinned: true, enabled: layout.canUnpin)
                }
                .onMove { source, destination in
                    change { $0.movePinned(from: source, to: destination) }
                }
            } header: {
                HStack {
                    Text("Tab bar")
                    Spacer()
                    Text("\(layout.pinned.count) of \(TabLayout.maxPinned)")
                }
            } footer: {
                Text("These sit beside the switcher. More always comes last.")
            }

            Section {
                ForEach(layout.overflow, id: \.self) { id in
                    row(id, pinned: false, enabled: layout.canPin)
                }
                .onMove { source, destination in
                    change { $0.moveOverflow(from: source, to: destination) }
                }
            } header: {
                Text("In More")
            } footer: {
                if !layout.canPin, !layout.overflow.isEmpty {
                    Text("The tab bar is full. Move a tab to More to pin another.")
                }
            }

            Section {
                Button("Reset to default") {
                    withAnimation { layouts.reset(manifest.instance.id) }
                }
                .frame(maxWidth: .infinity)
            } footer: {
                Text("Tabs are kept for \(manifest.instance.title) only.")
            }
        }
        .listStyle(.insetGrouped)
        .environment(\.editMode, .constant(.active))
        .navigationTitle("Edit tabs")
        .navigationBarTitleDisplayMode(.inline)
    }

    private func row(_ id: String, pinned: Bool, enabled: Bool) -> some View {
        let title = manifest.area(id)?.title ?? id
        return HStack(spacing: 12) {
            Button {
                change { layout in
                    if pinned { layout.unpin(id) } else { layout.pin(id) }
                }
            } label: {
                Image(systemName: pinned ? "minus.circle.fill" : "plus.circle.fill")
                    .font(.title3)
                    .foregroundStyle(pinned ? Theme.bad : Theme.accent)
                    .frame(minWidth: 32, minHeight: 32)
            }
            .buttonStyle(.borderless)
            .disabled(!enabled)
            .opacity(enabled ? 1 : 0.35)
            .accessibilityLabel(pinned ? "Move \(title) to More" : "Pin \(title) to the tab bar")
            Image(systemName: AreaSymbol.name(for: id))
                .foregroundStyle(Theme.accent)
                .frame(width: 24)
                .accessibilityHidden(true)
            Text(title).font(Theme.Fonts.body).foregroundStyle(Theme.ink)
        }
    }
}
