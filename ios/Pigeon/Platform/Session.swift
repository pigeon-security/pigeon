import Observation

/// Whether a person is signed in, and the client that speaks for them.
@MainActor
@Observable
final class Session {
    private(set) var signedIn = Keychain.load(TokenPair.self, for: Keychain.Key.tokens) != nil
    @ObservationIgnored private(set) lazy var client = MCPClient { [weak self] in
        self?.signOut()
    }

    /// Runs the OAuth sign-in.
    func signIn() async throws {
        _ = try await OAuth.signIn()
        signedIn = true
    }

    /// The person's sign-out: forgets the push device first (best effort), then signs out.
    func userSignOut() async {
        await Push.shared.unregister(client)
        signOut()
    }

    /// Deletes the stored tokens and client id.
    func signOut() {
        Keychain.delete(Keychain.Key.tokens)
        Keychain.delete(Keychain.Key.clientId)
        client.forgetCost()
        signedIn = false
    }
}
