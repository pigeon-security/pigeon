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

/// The signed-in or signed-out screen, by whether a session exists.
struct Root: View {
    let session: Session

    var body: some View {
        NavigationStack {
            if session.signedIn {
                PasskeysView(session: session)
            } else {
                SignInView(session: session)
            }
        }
        .tint(Tokens.accent)
    }
}
