import SwiftUI

/// The app's entry point.
@main
struct PigeonApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @State private var session = Session()

    var body: some Scene {
        WindowGroup {
            Root(session: session)
        }
    }
}

/// The signed-in tabs or the sign-in screen, by whether a session exists.
struct Root: View {
    let session: Session
    @State private var approvals = ApprovalsModel()
    @State private var status = StatusModel()
    @State private var store = InstanceStore()
    @State private var tab: AppTab = .approvals
    @State private var switching = false

    var body: some View {
        Group {
            if session.signedIn {
                signedIn
            } else {
                NavigationStack { SignInView(session: session) }
            }
        }
        .tint(Tokens.accent)
    }

    /// The selected tab's screen, the tab bar under it and the switcher over it.
    private var signedIn: some View {
        content
            .safeAreaInset(edge: .bottom, spacing: 0) {
                TabBar(
                    tabs: store.tabs, selected: $tab, badge: approvals.count,
                    onHoldInstances: { switching = true })
            }
            .overlay(alignment: .bottomTrailing) { switcher }
            // Counts waiting items for the badge even before the Approvals tab is opened.
            .task { await approvals.load(session.client) }
            .task { await store.load(session.client) }
            // Asks to allow notifications and registers the device, at launch and after sign-in.
            .task { await Push.shared.start(session.client) }
            .onChange(of: store.selected?.id) { _, _ in keepTabValid() }
            .onChange(of: store.instances.count) { _, _ in keepTabValid() }
    }

    /// The screen of the selected tab.
    @ViewBuilder
    private var content: some View {
        switch tab {
        case .approvals:
            NavigationStack { ApprovalsView(session: session, model: approvals) }
        case .status:
            NavigationStack {
                StatusView(session: session, model: status, openApprovals: { tab = .approvals })
            }
        case .cost:
            NavigationStack { CostScreen(session: session).toolbar(.hidden, for: .navigationBar) }
        case .instances:
            NavigationStack { InstancesView(session: session, store: store, status: status) }
        case .home:
            NavigationStack {
                HomeView(session: session, store: store, onSwitchToPlatform: switchToPlatform)
            }
        }
    }

    /// The instance switcher, above the tab bar, with a tap-away layer behind it.
    @ViewBuilder
    private var switcher: some View {
        if switching {
            ZStack(alignment: .bottomTrailing) {
                Color.black.opacity(0.001)
                    .ignoresSafeArea()
                    .onTapGesture { switching = false }
                InstanceSwitcher(
                    store: store,
                    onPick: { instance in
                        store.select(instance.id)
                        switching = false
                    },
                    onAll: {
                        tab = .instances
                        switching = false
                    }
                )
                .padding(.trailing, 12)
                .padding(.bottom, 64)
            }
        }
    }

    /// Selects the platform instance.
    private func switchToPlatform() {
        if let platform = store.platform { store.select(platform.id) }
    }

    /// Moves to the first tab of the selected instance when the current one is not among them.
    private func keepTabValid() {
        if !store.tabs.contains(tab), let first = store.tabs.first { tab = first }
    }
}
