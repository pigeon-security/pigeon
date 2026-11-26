import SwiftUI
import UIKit

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

/// The signed-in app or the sign-in screen, by whether a session exists, in the person's theme.
struct Root: View {
    let session: Session
    @AppStorage("theme") private var theme = AppearanceChoice.system.rawValue

    var body: some View {
        Group {
            if session.signedIn {
                AppShell(session: session)
            } else {
                SignInView(session: session)
            }
        }
        .tint(Theme.accent)
        .preferredColorScheme(AppearanceChoice(rawValue: theme)?.scheme)
    }
}
