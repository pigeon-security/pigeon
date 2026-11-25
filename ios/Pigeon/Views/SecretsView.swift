import SwiftUI

/// Metadata for a secret slot; never holds its value.
private struct SecretSlot: Identifiable {
    let name: String
    let owner: String
    let isSet: Bool
    let modified: String?
    var id: String { name }
    var service: String { owner.isEmpty ? "Other" : owner }

    init?(_ row: [String: Any]) {
        guard let name = row["name"] as? String, let isSet = row["set"] as? Bool else { return nil }
        self.name = name
        self.isSet = isSet
        owner = row["owner"] as? String ?? ""
        modified = row["modified"] as? String
    }
}

/// Lists secret metadata and opens secure entry for declared slots.
struct SecretsView: View {
    let session: Session
    @State private var secrets = ToolModel("secrets_list")
    @State private var selected: SecretSlot?
    @State private var saved = false
    @State private var refreshAfterWrite = false

    private var rows: [[String: Any]]? { secrets.data as? [[String: Any]] }

    private var slots: [SecretSlot] {
        (rows ?? []).compactMap { SecretSlot($0) }.sorted { $0.name < $1.name }
    }

    private var missing: [SecretSlot] { slots.filter { !$0.isSet } }

    private var groups: [String: [SecretSlot]] {
        Dictionary(grouping: slots.filter { $0.isSet }) { $0.service }
    }

    var body: some View {
        Screen(
            title: "Secrets", errors: [secrets.error].compactMap { $0 },
            refresh: { await secrets.load(session.client) }
        ) {
            if saved { Toast("Secret set.") }
            if let rows {
                if slots.count != rows.count {
                    ErrorBanner(text: "Some secret slots couldn't be read.")
                } else {
                    Footnote("\(slots.count - missing.count) of \(slots.count) set")
                }
                if !missing.isEmpty { section("Needs setting", slots: missing) }
                ForEach(groups.keys.sorted(), id: \.self) { service in
                    section(service, slots: groups[service] ?? [])
                }
            } else if secrets.data != nil {
                ErrorBanner(text: "Couldn't read the list of secrets.")
            } else if secrets.error == nil {
                ProgressView("Loading secrets…")
            }
        }
        .task(id: refreshAfterWrite) { await secrets.load(session.client) }
        .sheet(item: $selected) { slot in
            SecretEntrySheet(slot: slot, client: session.client) {
                saved = true
                selected = nil
                refreshAfterWrite.toggle()
            }
        }
    }

    private func section(_ title: String, slots: [SecretSlot]) -> some View {
        CardSection(title) {
            GroupedCard {
                ForEach(Array(slots.enumerated()), id: \.element.id) { index, slot in
                    if index > 0 { RowSeparator(inset: 16) }
                    HStack(spacing: 8) {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(slot.name)
                                .font(.system(size: 14, weight: .medium, design: .monospaced))
                                .foregroundStyle(Tokens.ink)
                                .fixedSize(horizontal: false, vertical: true)
                            Text(slot.isSet ? "Updated \(timestampText(slot.modified))" : slot.service)
                                .font(AppFont.meta).foregroundStyle(Tokens.muted)
                        }
                        Spacer(minLength: 0)
                        if !slot.owner.isEmpty {
                            Button("Set") {
                                saved = false
                                selected = slot
                            }
                            .buttonStyle(ActionButtonStyle(.primary, small: true, fill: false))
                            .accessibilityLabel("Set \(slot.name)")
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 8)
                    .frame(minHeight: 56)
                }
            }
        }
    }
}

/// A write-only field for a new value. Every submission requires a fresh passkey assertion.
private struct SecretEntrySheet: View {
    let slot: SecretSlot
    let client: MCPClient
    let onSaved: () -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var value = ""
    @State private var saving = false
    @State private var failed = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                ScreenHeader("Set secret")
                Text(slot.name).font(.system(.body, design: .monospaced))
                Text("Paste a new value, then confirm with your passkey.")
                    .font(AppFont.note).foregroundStyle(Tokens.muted)
                SecureField("Paste secret value", text: $value)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .padding(12)
                    .background(Tokens.chip, in: RoundedRectangle(cornerRadius: 10))
                    .disabled(saving)
                if failed {
                    Text("Couldn't confirm the write. Check the value and try again.")
                        .font(AppFont.note).foregroundStyle(Tokens.warn)
                }
                Button(saving ? "Waiting for confirmation…" : "Set with passkey", action: submit)
                    .buttonStyle(ActionButtonStyle())
                    .disabled(saving || value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                Button("Cancel") { dismiss() }
                    .buttonStyle(ActionButtonStyle(.secondary))
                    .disabled(saving)
            }
            .padding(Metrics.side)
        }
        .foregroundStyle(Tokens.ink)
        .background(Tokens.background.ignoresSafeArea())
        .interactiveDismissDisabled(saving)
        .onDisappear { value = "" }
    }

    private func submit() {
        guard !saving else { return }
        let submitted = value
        value = ""
        failed = false
        saving = true
        Task {
            defer { saving = false }
            do {
                try await Approver().putSecret(name: slot.name, value: submitted, client: client)
                onSaved()
            } catch {
                // Do not display or log a response that might echo the input.
                failed = true
            }
        }
    }
}
