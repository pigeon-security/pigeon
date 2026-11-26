import SwiftUI

/// The bar at the bottom of the app: the switcher, pinned at the leading edge, then the pinned
/// areas and More. Drawn by the app in place of the system tab bar, which has no room for the
/// switcher and cannot be customized on iPhone.
struct ShellBar: View {
    /// The areas in the bar, in order.
    let pinned: [Area]
    @Environment(ShellState.self) private var shell
    @Environment(ManifestStore.self) private var store

    /// The bar's height, which each tab's scroll content keeps clear of.
    static let height: CGFloat = 62
    /// The room the bar takes at the bottom of each tab's scroll content.
    static let reserved: CGFloat = height + 12

    var body: some View {
        HStack(spacing: 8) {
            SwitcherButton()
            HStack(spacing: 2) {
                ForEach(pinned) { area in
                    item(id: area.id, title: area.title, badge: area.id == "inbox" ? store.inboxCount : 0)
                }
                item(id: ShellState.moreTab, title: "More", badge: moreBadge)
            }
            .padding(4)
            .frame(height: Self.height)
            .glass(in: Capsule())
        }
        .padding(.horizontal, 16)
        .padding(.bottom, 2)
        .modifier(GlassGroup())
        .sensoryFeedback(.selection, trigger: shell.selection)
        .ignoresSafeArea(.keyboard)
    }

    /// The Inbox badge moves to More while Inbox is not pinned.
    private var moreBadge: Int {
        pinned.contains(where: { $0.id == "inbox" }) ? 0 : store.inboxCount
    }

    private func item(id: String, title: String, badge: Int) -> some View {
        let selected = shell.selection == id
        return Button {
            if selected, id == ShellState.moreTab {
                shell.morePath = NavigationPath()
            } else {
                shell.selection = id
            }
        } label: {
            VStack(spacing: 2) {
                Image(systemName: id == ShellState.moreTab ? "ellipsis" : AreaSymbol.name(for: id))
                    .font(.system(size: 18, weight: .medium))
                    .frame(height: 24)
                    .overlay(alignment: .topTrailing) {
                        if badge > 0 {
                            Text(badge > 99 ? "99+" : String(badge))
                                .font(.system(size: 10, weight: .bold).monospacedDigit())
                                .foregroundStyle(Theme.accentInk)
                                .padding(.horizontal, 4)
                                .frame(minWidth: 16, minHeight: 16)
                                .background(Theme.accent, in: Capsule())
                                .offset(x: 10, y: -4)
                        }
                    }
                Text(title)
                    .font(.caption2.weight(.medium))
                    .lineLimit(1)
                    .minimumScaleFactor(0.8)
            }
            .foregroundStyle(selected ? Theme.accent : Theme.ink)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background {
                if selected { Capsule().fill(Theme.hover) }
            }
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
        .accessibilityValue(badge > 0 ? "\(badge) waiting" : "")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// The switcher: the selected instance's letter with the signed-in person's initials on its
/// corner. Opens the instance and account sheet. `compact` is the toolbar size, shown while the
/// instances load.
struct SwitcherButton: View {
    var compact = false
    @Environment(ShellState.self) private var shell
    @Environment(ManifestStore.self) private var store
    @AppStorage("profileInitials") private var initials = ""

    var body: some View {
        Button {
            shell.sheet = .switcher
        } label: {
            if compact {
                mark(size: 30)
            } else {
                mark(size: 38)
                    .frame(width: ShellBar.height, height: ShellBar.height)
                    .glass(in: Circle(), interactive: true)
                    .contentShape(Circle())
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Switch instance or account")
        .accessibilityValue(store.selected?.title ?? "No instance")
    }

    private func mark(size: CGFloat) -> some View {
        InstanceInitial(text: store.selected?.initial ?? "?", size: size)
            .overlay(alignment: .bottomTrailing) {
                Text(initials.isEmpty ? "?" : initials)
                    .font(.system(size: size * 0.24, weight: .bold))
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                    .minimumScaleFactor(0.7)
                    .frame(width: size * 0.5, height: size * 0.5)
                    .background(Theme.hover, in: Circle())
                    .overlay(Circle().stroke(Theme.bg, lineWidth: 2))
                    .offset(x: size * 0.14, y: size * 0.1)
            }
    }
}

/// On iOS 26 the switcher and the tab strip share one glass container, so their glass blends.
private struct GlassGroup: ViewModifier {
    func body(content: Content) -> some View {
        if #available(iOS 26.0, *) {
            GlassEffectContainer(spacing: 8) { content }
        } else {
            content
        }
    }
}
