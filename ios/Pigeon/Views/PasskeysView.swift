import SwiftUI

/// One passkey as the platform lists it.
struct Passkey: Identifiable {
    let id: String
    let name: String?
    let createdAt: Double?
    let lastUsedAt: Double?

    /// Reads one element of the `passkeys_list` answer; nil when it has no id.
    init?(_ json: [String: Any]) {
        guard let id = json["id"] as? String else { return nil }
        self.id = id
        name = json["name"] as? String
        createdAt = json["created_at"] as? Double
        lastUsedAt = json["last_used_at"] as? Double
    }
}

/// Loads the passkey list.
@MainActor
@Observable
final class PasskeysModel {
    private(set) var passkeys: [Passkey] = []
    private(set) var error: String?

    /// Fetches the list with the MCP tool `passkeys_list`.
    func load(_ client: MCPClient) async {
        do {
            let data = try await client.call("passkeys_list")
            guard let rows = data as? [[String: Any]] else {
                throw PlatformError.badResponse("passkey list")
            }
            passkeys = rows.compactMap(Passkey.init)
            error = nil
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            self.error = "passkeys_list failed: \(error.localizedDescription)"
        }
    }
}

/// A read-only list of the person's passkeys.
struct PasskeysView: View {
    let session: Session
    @State private var model = PasskeysModel()

    var body: some View {
        ZStack {
            DotGrid()
            ScrollView {
                VStack(alignment: .leading, spacing: Tokens.spaceGap) {
                    Eyebrow(text: "Passkeys")
                    if let error = model.error {
                        Text(error).font(.token(Tokens.typeSmall)).foregroundStyle(Tokens.warn)
                            .textSelection(.enabled)
                    }
                    ForEach(model.passkeys) { key in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(key.name ?? "Passkey")
                                .font(.token(Tokens.typeHeading))
                                .foregroundStyle(Tokens.ink)
                            Text("Added \(when(key.createdAt))")
                            Text("Last used \(when(key.lastUsedAt))")
                        }
                        .font(.token(Tokens.typeSmall))
                        .foregroundStyle(Tokens.muted)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(Tokens.spaceGap)
                        .block()
                    }
                }
                .padding(Tokens.spacePage)
            }
            .refreshable { await model.load(session.client) }
        }
        .navigationTitle("Pigeon")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Sign out") { session.signOut() }
            }
        }
        .task { await model.load(session.client) }
    }

    /// A time in ms since the epoch as text, or "never".
    private func when(_ ms: Double?) -> String {
        guard let ms else { return "never" }
        return Date(timeIntervalSince1970: ms / 1000).formatted(
            date: .abbreviated, time: .shortened)
    }
}
