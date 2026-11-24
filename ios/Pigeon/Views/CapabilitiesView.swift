import SwiftUI

/// Hosted capabilities and the pin changes planned for the selected instance.
struct CapabilitiesView: View {
    let session: Session
    let instance: InstanceInfo?
    let model: CapabilityUpgrades

    private var instanceID: String? { instance?.id }
    private var snapshot: CapabilitySnapshot? { model.current(instanceID) }
    private var canUpgrade: Bool {
        snapshot?.version != nil && !model.loading && model.working.isEmpty
            && !model.waiting(instanceID) && model.error == nil
    }

    var body: some View {
        Screen(title: "Capabilities", errors: [model.error].compactMap { $0 }, refresh: reload) {
            Text(instance?.title ?? "Pigeon Platform")
                .font(AppFont.meta).foregroundStyle(Tokens.muted)
            if let notice = model.notice { Footnote(notice) }
            if model.waiting(instanceID) {
                Footnote("An upgrade proposal is waiting. Continue with your passkey, or review it in Needs you.")
                Button("Check or continue approval") {
                    Task { await model.resume(instanceID, client: session.client) }
                }
                .buttonStyle(ChipButtonStyle())
                .disabled(model.loading || !model.working.isEmpty)
            }
            if let snapshot {
                capabilitySections(snapshot)
                if snapshot.version != nil {
                    Footnote("Upgrading changes this instance’s version pins. You’ll confirm with your passkey. Any listed migration steps must be done manually.")
                }
            } else if model.error == nil {
                ProgressView().accessibilityLabel("Loading capabilities")
            }
        }
        .task { await reload() }
    }

    private func reload() async { await model.load(instanceID, client: session.client) }

    @ViewBuilder
    private func capabilitySections(_ snapshot: CapabilitySnapshot) -> some View {
        let breaking = snapshot.rows.filter { $0.step?.breaking == true }
        let patch = snapshot.rows.filter { $0.step?.breaking == false }
        let current = snapshot.rows.filter { $0.step == nil && $0.issue == nil }
        let unavailable = snapshot.rows.filter { $0.step == nil && $0.issue != nil }
        if snapshot.version == nil {
            Footnote("This instance has no stored record to propose changes to. These are the platform’s declared versions.")
            if !current.isEmpty { section("Versions", rows: current) }
        } else {
            if !breaking.isEmpty { section("Breaking", rows: breaking, breaking: true) }
            if !patch.isEmpty { section("Patch", rows: patch, batch: true) }
            if !current.isEmpty { section("Up to date", rows: current) }
            if breaking.isEmpty && patch.isEmpty && !snapshot.rows.isEmpty {
                Footnote("No updates in the current upgrade plan.")
            }
        }
        if !unavailable.isEmpty { section("Unavailable", rows: unavailable) }
        if snapshot.rows.isEmpty { Footnote("No hosted capabilities reported.") }
    }

    private func section(
        _ name: String, rows: [CapabilitySnapshot.Row], breaking: Bool = false, batch: Bool = false
    ) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("\(name) · \(rows.count)")
                    .font(AppFont.label).foregroundStyle(breaking ? Tokens.warn : Tokens.muted)
                    .accessibilityAddTraits(.isHeader)
                Spacer()
                if batch && rows.count >= 2 {
                    Button("Upgrade all") { propose(rows.compactMap(\.step)) }
                        .font(AppFont.button)
                        .foregroundStyle(Tokens.onAccent)
                        .padding(.horizontal, 12)
                        .frame(height: 32)
                        .background(Tokens.accent, in: Capsule())
                        .buttonStyle(.plain)
                        .disabled(!canUpgrade)
                        .opacity(canUpgrade ? 1 : 0.55)
                }
            }
            .padding(.leading, Metrics.sectionIndent)
            if breaking {
                Text("Check the changes and any migration steps before upgrading.")
                    .font(AppFont.meta).foregroundStyle(Tokens.warn)
                    .padding(.leading, Metrics.sectionIndent)
            }
            GroupedCard {
                ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                    if index > 0 { RowSeparator(inset: 16) }
                    capabilityRow(row)
                }
            }
        }
    }

    private func capabilityRow(_ row: CapabilitySnapshot.Row) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(row.id).font(AppFont.row).foregroundStyle(Tokens.ink)
                    runningVersion(row)
                }
                Spacer(minLength: 0)
                if let step = row.step { upgradeButton(step) }
            }
            if let issue = row.issue {
                Text(issue).font(AppFont.meta).foregroundStyle(Tokens.warn)
            }
            if let step = row.step {
                ForEach(step.work, id: \.version) { work in
                    VStack(alignment: .leading, spacing: 2) {
                        Text("\(work.version) · \(work.summary)")
                            .font(AppFont.meta).foregroundStyle(Tokens.ink)
                        if let migration = work.migration {
                            Text("Manual migration: \(migration)")
                                .font(AppFont.meta).foregroundStyle(Tokens.muted)
                        }
                    }
                    .textSelection(.enabled)
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .frame(minHeight: 56)
    }

    @ViewBuilder
    private func runningVersion(_ row: CapabilitySnapshot.Row) -> some View {
        if let version = row.version {
            if row.step == nil, let confirmation = model.confirmation(instanceID),
                let pin = confirmation.pins[row.id], snapshot?.pins[row.id] == pin {
                TimelineView(.periodic(from: confirmation.at, by: 60)) { context in
                    if context.date.timeIntervalSince(confirmation.at) < 60 {
                        Text("\(version) · upgraded just now").font(AppFont.meta).foregroundStyle(Tokens.good)
                    } else {
                        Text(version).font(AppFont.meta).foregroundStyle(Tokens.muted)
                    }
                }
            } else {
                Text(version).font(AppFont.meta).foregroundStyle(Tokens.muted)
            }
        } else {
            Text("No selected version").font(AppFont.meta).foregroundStyle(Tokens.muted)
        }
    }

    private func upgradeButton(_ step: CapabilitySnapshot.Step) -> some View {
        let pending = model.working.contains(step.component)
        return Button { propose([step]) } label: {
            Text(pending ? "Upgrading…" : "Upgrade to \(step.to)")
                .font(AppFont.button).lineLimit(1).minimumScaleFactor(0.65)
                .foregroundStyle(step.breaking ? Tokens.warn : Tokens.onAccent)
                .frame(width: 152, height: 32)
                .background(step.breaking ? Tokens.warn.opacity(0.14) : Tokens.accent, in: Capsule())
        }
        .buttonStyle(.plain)
        .disabled(!canUpgrade)
        .opacity(canUpgrade || pending ? 1 : 0.55)
    }

    private func propose(_ steps: [CapabilitySnapshot.Step]) {
        Task { await model.propose(steps, id: instanceID, client: session.client) }
    }
}
