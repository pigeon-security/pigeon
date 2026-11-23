import SwiftUI
import UIKit

/// A tab of the app. The server names them in `instances_tabs`.
enum AppTab: String, CaseIterable {
    case approvals, status, cost, instances, home

    /// The label under the icon.
    var title: String {
        switch self {
        case .approvals: return "Approvals"
        case .status: return "Status"
        case .cost: return "Cost"
        case .instances: return "Instances"
        case .home: return "Home"
        }
    }

    /// The SF Symbol for the icon.
    var symbol: String {
        switch self {
        case .approvals: return "checkmark.seal"
        case .status: return "waveform.path.ecg"
        case .cost: return "chart.bar"
        case .instances: return "square.stack.3d.up"
        case .home: return "house"
        }
    }
}

/// One instance, as `instances_tabs` (or, failing that, `instances_list`) describes it.
struct InstanceInfo: Identifiable {
    let id: String
    let title: String
    /// "home" for the platform's own account, "linked" for another.
    let accountKind: String
    let zones: [String]
    let tabs: [AppTab]
    let website: String?
    let site: String?
    let repository: String?

    /// Whether this is the platform instance, which has the four data tabs and Settings.
    var isPlatform: Bool { accountKind == "home" }
}

/// The instances, and which one the app shows. The choice is kept in `UserDefaults`.
@MainActor
@Observable
final class InstanceStore {
    private(set) var instances: [InstanceInfo] = []
    private(set) var error: String?
    private(set) var selectedId: String? = UserDefaults.standard.string(forKey: "selectedInstance")

    /// The platform instance: the one in the home account, else the first.
    var platform: InstanceInfo? { instances.first { $0.isPlatform } ?? instances.first }

    /// The selected instance; the platform when nothing valid is selected.
    var selected: InstanceInfo? { instances.first { $0.id == selectedId } ?? platform }

    /// The tabs to show. Before the first load, the platform's four.
    var tabs: [AppTab] { selected?.tabs ?? [.approvals, .status, .cost, .instances] }

    /// Chooses an instance and keeps the choice on this device.
    func select(_ id: String) {
        selectedId = id
        UserDefaults.standard.set(id, forKey: "selectedInstance")
    }

    /// Loads the instances with `instances_tabs`; if that tool is missing, with `instances_list`.
    func load(_ client: MCPClient) async {
        do {
            let data = try await client.call("instances_tabs")
            let rows = (data as? [String: Any])?["instances"] as? [[String: Any]] ?? []
            instances = rows.compactMap { Self.parse($0, fallback: false) }
            error = nil
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            await loadFallback(client)
        }
    }

    /// Reads `instances_list` and gives each instance the tabs its account kind implies.
    private func loadFallback(_ client: MCPClient) async {
        do {
            let data = try await client.call("instances_list")
            let rows =
                data as? [[String: Any]]
                ?? (data as? [String: Any])?["instances"] as? [[String: Any]] ?? []
            instances = rows.compactMap { Self.parse($0, fallback: true) }
            error = nil
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            self.error = "instances_tabs failed: \(error.localizedDescription)"
        }
    }

    /// Reads one instance object; nil when it has no id.
    private static func parse(_ row: [String: Any], fallback: Bool) -> InstanceInfo? {
        guard let id = row["id"] as? String else { return nil }
        var kind = row["account_kind"] as? String ?? ""
        if kind.isEmpty { kind = (row["account"] as? [String: Any])?["kind"] as? String ?? "linked" }
        let zones = row["zones"] as? [String] ?? []
        var tabs = (row["tabs"] as? [String] ?? []).compactMap { AppTab(rawValue: $0) }
        if tabs.isEmpty {
            tabs = kind == "home" ? [.approvals, .status, .cost, .instances] : [.home, .instances]
        }
        // The Instances tab always stays, so the person can always switch back.
        if !tabs.contains(.instances) { tabs.append(.instances) }
        let facts = row["facts"] as? [String: Any] ?? [:]
        var website = facts["website"] as? String
        var site = facts["site"] as? String
        var repository = facts["repository"] as? String
        if fallback {
            if let zone = zones.first { website = "https://\(zone)" }
            site = (row["sites"] as? [[String: Any]])?.first?["name"] as? String
            if let repo = row["repo"] as? [String: Any],
                let owner = repo["owner"] as? String, let name = repo["name"] as? String
            {
                repository = "\(owner)/\(name)"
            } else if let repo = row["repo"] as? String {
                repository = repo
            }
        }
        return InstanceInfo(
            id: id, title: row["title"] as? String ?? id, accountKind: kind, zones: zones,
            tabs: tabs, website: website, site: site, repository: repository)
    }
}

/// The custom tab bar. A tap selects a tab; holding Instances for half a second opens the switcher.
struct TabBar: View {
    let tabs: [AppTab]
    @Binding var selected: AppTab
    let badge: Int
    let onHoldInstances: () -> Void

    var body: some View {
        HStack(spacing: 0) {
            ForEach(tabs, id: \.self) { tab in
                item(tab)
            }
        }
        .padding(.top, 8)
        .padding(.bottom, 4)
        .background(Tokens.card.ignoresSafeArea(edges: .bottom))
        .overlay(alignment: .top) { Rectangle().fill(Tokens.line).frame(height: 1) }
    }

    /// One tab: icon, label and, on Approvals, the count badge.
    private func item(_ tab: AppTab) -> some View {
        VStack(spacing: 3) {
            Image(systemName: tab.symbol)
                .font(.system(size: 22))
                .overlay(alignment: .topTrailing) {
                    if tab == .approvals && badge > 0 {
                        Text("\(badge)")
                            .font(AppFont.tab)
                            .foregroundStyle(Tokens.onAccent)
                            .padding(.horizontal, 5)
                            .padding(.vertical, 1)
                            .background(Capsule().fill(Tokens.accent))
                            .offset(x: 12, y: -6)
                    }
                }
            Text(tab.title).font(AppFont.tab)
        }
        .foregroundStyle(selected == tab ? Tokens.accent : Tokens.muted)
        .frame(maxWidth: .infinity)
        .contentShape(Rectangle())
        .onTapGesture { selected = tab }
        .onLongPressGesture(minimumDuration: 0.5) {
            guard tab == .instances else { return }
            UIImpactFeedbackGenerator(style: .medium).impactOccurred()
            onHoldInstances()
        }
        .accessibilityLabel(tab.title)
        .accessibilityAddTraits(.isButton)
    }
}

/// The instance switcher menu: the current instance ticked, the others, then "All instances".
struct InstanceSwitcher: View {
    let store: InstanceStore
    let onPick: (InstanceInfo) -> Void
    let onAll: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(store.instances) { instance in
                Button {
                    onPick(instance)
                } label: {
                    HStack {
                        Text(instance.title).font(AppFont.row).foregroundStyle(Tokens.ink)
                        Spacer(minLength: 24)
                        if instance.id == store.selected?.id {
                            Image(systemName: "checkmark").foregroundStyle(Tokens.accent)
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                Divider()
            }
            Button(action: onAll) {
                Text("All instances").font(AppFont.row).foregroundStyle(Tokens.accent)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        }
        .fixedSize(horizontal: true, vertical: false)
        .block()
    }
}
