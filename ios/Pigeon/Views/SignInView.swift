import SwiftUI

/// The screen before sign-in: one line and one button.
struct SignInView: View {
    let session: Session
    @State private var error: String?
    @State private var busy = false

    var body: some View {
        ZStack {
            DotGrid()
            VStack(spacing: Metrics.gap) {
                Text("Pigeon").font(AppFont.largeTitle).foregroundStyle(Tokens.ink)
                Text("Sign in with your passkey on platform.pigeon.dev.")
                    .font(AppFont.body)
                    .foregroundStyle(Tokens.muted)
                    .multilineTextAlignment(.center)
                Button(busy ? "Signing in…" : "Sign in") { Task { await signIn() } }
                    .buttonStyle(ActionButtonStyle(.primary))
                    .frame(maxWidth: 280)
                    .disabled(busy)
                if let error {
                    Text(error)
                        .font(AppFont.meta)
                        .foregroundStyle(Tokens.warn)
                        .multilineTextAlignment(.center)
                }
            }
            .padding(Metrics.side)
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
