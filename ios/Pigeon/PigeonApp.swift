import SwiftUI
import UIKit

/// The app's entry point.
@main
struct PigeonApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @State private var session = Session()

    init() {
        Self.styleNavigationTitles()
    }

    var body: some Scene {
        WindowGroup {
            Root(session: session)
        }
    }

    /// Sets the title fonts through the legacy proxies only, so the system bars keep their
    /// own look (Liquid Glass on iOS 26, transparent at the top on iOS 18).
    private static func styleNavigationTitles() {
        let bar = UINavigationBar.appearance()
        bar.largeTitleTextAttributes = [
            .font: UIFont.systemFont(ofSize: 30, weight: .bold),
            .kern: -0.4,
            .foregroundColor: UIColor(Tokens.ink),
        ]
        bar.titleTextAttributes = [
            .font: UIFont.systemFont(ofSize: 17, weight: .semibold),
            .foregroundColor: UIColor(Tokens.ink),
        ]
    }
}

/// Minimises the tab bar on scroll down, on iOS 26 and later.
private struct MinimizeTabBar: ViewModifier {
    func body(content: Content) -> some View {
        if #available(iOS 26.0, *) {
            content.tabBarMinimizeBehavior(.onScrollDown)
        } else {
            content
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

    /// The system tab bar with one navigation stack per tab.
    private var signedIn: some View {
        TabView(selection: $tab) {
            ForEach(store.tabs, id: \.self) { item in
                Tab(item.title, systemImage: item.symbol, value: item) {
                    NavigationStack { screen(item) }
                }
                .badge(item == .approvals ? approvals.count : 0)
            }
        }
        .tint(Tokens.accent)
        .modifier(MinimizeTabBar())
        .environment(store)
        .environment(\.showAllInstances) { tab = .instances }
        // Counts waiting items for the badge even before the Approvals tab is opened.
        .task { await approvals.load(session.client) }
        .task { await store.load(session.client) }
        // Asks to allow notifications and registers the device, at launch and after sign-in.
        .task { await Push.shared.start(session.client) }
        .onChange(of: store.selected?.id) { _, _ in keepTabValid() }
        .onChange(of: store.instances.count) { _, _ in keepTabValid() }
    }

    /// The root screen of a tab, with the instance menu in its navigation bar.
    @ViewBuilder
    private func screen(_ item: AppTab) -> some View {
        switch item {
        case .approvals:
            ApprovalsView(session: session, model: approvals).instanceMenu()
        case .status:
            StatusView(session: session, model: status, openApprovals: { tab = .approvals })
                .instanceMenu()
        case .cost:
            CostScreen(session: session).instanceMenu()
        case .instances:
            InstancesView(session: session, store: store, status: status).instanceMenu()
        case .home:
            HomeView(session: session, store: store, onSwitchToPlatform: switchToPlatform)
                .instanceMenu()
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
