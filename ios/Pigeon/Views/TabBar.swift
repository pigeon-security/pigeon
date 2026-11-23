import SwiftUI
import UIKit

/// A tab of the app. The server names them in `instances_tabs`.
enum AppTab: String, CaseIterable {
    case sessions, cost, settings, home

    /// The label under the icon.
    var title: String {
        switch self {
        case .sessions: return "Sessions"
        case .cost: return "Cost"
        case .settings: return "Settings"
        case .home: return "Home"
        }
    }

    /// The SF Symbol for the icon.
    var symbol: String {
        switch self {
        case .sessions: return "waveform.path.ecg"
        case .cost: return "chart.bar"
        case .settings: return "gearshape"
        case .home: return "house"
        }
    }

    /// The tab a server tab name stands for: `approvals` and `status` are Sessions, `instances`
    /// is Settings. Nil for a name this app does not know.
    static func fromServer(_ name: String) -> AppTab? {
        switch name {
        case "approvals", "status", "sessions": return .sessions
        case "instances", "settings": return .settings
        case "cost": return .cost
        case "home": return .home
        default: return nil
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

    /// The line under the title in the instance menu.
    var subtitle: String {
        "\(repository ?? id) · \(isPlatform ? "core account" : "linked account")"
    }
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
    var tabs: [AppTab] { selected?.tabs ?? [.sessions, .cost, .settings] }

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
        var tabs: [AppTab] = []
        for tab in (row["tabs"] as? [String] ?? []).compactMap({ AppTab.fromServer($0) })
        where !tabs.contains(tab) {
            tabs.append(tab)
        }
        if tabs.isEmpty {
            tabs = kind == "home" ? [.sessions, .cost, .settings] : [.home, .settings]
        }
        // Settings always stays, so the person can always switch back.
        if !tabs.contains(.settings) { tabs.append(.settings) }
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

/// The action for "All instances" in the instance menu. Root sets it to open the Settings tab.
extension EnvironmentValues {
    @Entry var showAllInstances: () -> Void = {}
}

/// The instance switcher: the instance's initial in a round avatar and a chevron, as a menu that
/// lists the instances and "All instances". Only Sessions and Home carry it.
struct InstanceSwitcher: View {
    @Environment(InstanceStore.self) private var store
    @Environment(\.showAllInstances) private var showAllInstances

    /// The first letter of the current instance's name, in capitals.
    private var initial: String {
        String((store.selected?.title ?? "?").prefix(1)).uppercased()
    }

    var body: some View {
        Menu {
            if !store.instances.isEmpty {
                Section("Switch instance") {
                    ForEach(store.instances) { i in
                        Button {
                            store.select(i.id)
                        } label: {
                            if i.id == store.selected?.id {
                                Label(i.title, systemImage: "checkmark")
                            } else {
                                Text(i.title)
                            }
                            Text(i.subtitle)
                        }
                    }
                }
                Divider()
            }
            Button("All instances", systemImage: "square.grid.2x2") { showAllInstances() }
        } label: {
            HStack(spacing: 6) {
                Text(initial)
                    .font(.system(size: 15, weight: .bold))
                    .foregroundStyle(Tokens.onAccent)
                    .frame(width: 32, height: 32)
                    .background(Circle().fill(Tokens.accent))
                Image(systemName: "chevron.down")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Tokens.muted)
            }
            .contentShape(Rectangle())
        }
        .accessibilityLabel("Instance: \(store.selected?.title ?? "none")")
    }
}
