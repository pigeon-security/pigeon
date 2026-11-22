import SwiftUI

/// The screen before sign-in: one line and one button.
struct SignInView: View {
    let session: Session
    @State private var error: String?
    @State private var busy = false

    var body: some View {
        ZStack {
            DotGrid()
            VStack(spacing: Tokens.spaceGap) {
                Eyebrow(text: "Pigeon")
                Text("Sign in with your passkey on platform.pigeon.dev.")
                    .font(.token(Tokens.typeBody))
                    .foregroundStyle(Tokens.ink)
                    .multilineTextAlignment(.center)
                Button(busy ? "Signing in…" : "Sign in") { Task { await signIn() } }
                    .buttonStyle(PrimaryButtonStyle())
                    .disabled(busy)
                if let error {
                    Text(error)
                        .font(.token(Tokens.typeSmall))
                        .foregroundStyle(Tokens.warn)
                        .multilineTextAlignment(.center)
                }
            }
            .padding(Tokens.spacePage)
        }
    }

    private func signIn() async {
        busy = true
        defer { busy = false }
        error = nil
        do {
            try await session.signIn()
        } catch {
            // Every failure is shown, a closed sheet included: a silent button helps no one.
            self.error = error.localizedDescription
        }
    }
}
