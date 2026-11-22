import SwiftUI

/// The app's entry point.
@main
struct PigeonApp: App {
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

    var body: some View {
        Group {
            if session.signedIn {
                TabView {
                    NavigationStack { ApprovalsView(session: session, model: approvals) }
                        .tabItem { Label("Approvals", systemImage: "checkmark.seal") }
                        .badge(approvals.count)
                    NavigationStack { ArchitectureView(session: session) }
                        .tabItem { Label("Architecture", systemImage: "square.grid.2x2") }
                    NavigationStack { AccessView(session: session) }
                        .tabItem { Label("Access", systemImage: "key") }
                    NavigationStack { CostView(session: session) }
                        .tabItem { Label("Cost", systemImage: "chart.bar") }
                }
                // Counts waiting items for the badge even before the Approvals tab is opened.
                .task { await approvals.load(session.client) }
            } else {
                NavigationStack { SignInView(session: session) }
            }
        }
        .tint(Tokens.accent)
    }
}
