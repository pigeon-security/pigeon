import Observation

/// Whether a person is signed in, and the client that speaks for them.
@MainActor
@Observable
final class Session {
    private(set) var signedIn = Keychain.load(TokenPair.self, for: Keychain.Key.tokens) != nil
    /// Why the platform ended the session, for the sign-in screen; nil after the person's own
    /// sign-out.
    private(set) var signedOutNotice: String?
    @ObservationIgnored private(set) lazy var client = MCPClient { [weak self] in
        self?.signOut(notice: "Your session ended. Sign in again with your passkey.")
    }
    /// The console's read and write paths, for every screen.
    @ObservationIgnored private(set) lazy var api = ConsoleAPI(client: client)

    /// Runs the OAuth sign-in.
    func signIn() async throws {
        _ = try await OAuth.signIn()
        signedOutNotice = nil
        signedIn = true
    }

    /// The person's sign-out: forgets the push device first (best effort), then signs out.
    func userSignOut() async {
        await Push.shared.unregister(client)
        signOut()
    }

    /// Deletes the stored tokens and client id. `notice` says why, when it was not the person.
    func signOut(notice: String? = nil) {
        Keychain.delete(Keychain.Key.tokens)
        Keychain.delete(Keychain.Key.clientId)
        signedOutNotice = notice
        signedIn = false
    }
}
