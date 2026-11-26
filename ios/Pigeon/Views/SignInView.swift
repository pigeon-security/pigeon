import SwiftUI

/// The screen before sign-in: the pigeon mark, one line and one button (passkey only). It shows
/// the sign-in in progress, why the last attempt failed, and why the platform signed the person
/// out when it did.
struct SignInView: View {
    let session: Session
    @State private var error: String?
    @State private var busy = false

    var body: some View {
        VStack(spacing: 14) {
            Image("PigeonMark")
                .resizable()
                .scaledToFill()
                .frame(width: 84, height: 84)
                .background(Theme.markGround)
                .clipShape(RoundedRectangle(cornerRadius: 19))
                .overlay(RoundedRectangle(cornerRadius: 19).stroke(Theme.markEdge, lineWidth: 1))
                .accessibilityHidden(true)
            Text("pigeon").font(.largeTitle.weight(.bold)).foregroundStyle(Theme.ink)
            Text("Use your passkey to open the console.")
                .font(.body)
                .foregroundStyle(Theme.muted)
                .multilineTextAlignment(.center)
            if let notice = session.signedOutNotice, error == nil, !busy {
                Label(notice, systemImage: "info.circle")
                    .font(Theme.Fonts.meta)
                    .foregroundStyle(Theme.ink2)
                    .multilineTextAlignment(.center)
                    .padding(.vertical, 8)
                    .padding(.horizontal, 12)
                    .background(Theme.warnSoft, in: RoundedRectangle(cornerRadius: 10))
            }
            Button {
                Task { await signIn() }
            } label: {
                if busy {
                    HStack(spacing: 8) {
                        ProgressView().tint(Theme.accentInk)
                        Text("Signing in…")
                    }
                } else {
                    Label("Sign in with passkey", systemImage: "key")
                }
            }
            .buttonStyle(SheetButtonStyle(.primary))
            .disabled(busy)
            .padding(.top, 16)
            Text(OAuth.host).font(Theme.Fonts.id).foregroundStyle(Theme.muted)
            if let error {
                VStack(spacing: 4) {
                    Text("Couldn't sign in")
                        .font(Theme.Fonts.body.weight(.semibold))
                        .foregroundStyle(Theme.bad)
                    Text(error)
                        .font(Theme.Fonts.meta)
                        .foregroundStyle(Theme.bad)
                        .multilineTextAlignment(.center)
                        .textSelection(.enabled)
                }
                .accessibilityElement(children: .combine)
            }
        }
        .padding(.horizontal, 28)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.sunk)
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
