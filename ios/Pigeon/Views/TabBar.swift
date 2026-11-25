import SwiftUI
import UIKit

/// A tab of the app, from the areas `console_manifest` lists for the selected instance.
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

    /// The tab a console area stands for: Sessions and Cost as they are, and a linked
    /// instance's Overview as Home. Nil for an area this app has no tab for; Settings is
    /// always added.
    static func fromArea(_ id: String, home: Bool) -> AppTab? {
        switch id {
        case "sessions": return .sessions
        case "cost": return .cost
        case "overview": return home ? nil : .home
        default: return nil
        }
    }
}

/// One instance, as `instances_list` and its `console_manifest` describe it.
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

    /// Loads the instances with `instances_list`, then each one's tabs from its console
    /// navigation (`console_manifest`), the same data the web console builds its own from.
    func load(_ client: MCPClient) async {
        do {
            let data = try await client.call("instances_list")
            let rows =
                data as? [[String: Any]]
                ?? (data as? [String: Any])?["instances"] as? [[String: Any]] ?? []
            var loaded: [InstanceInfo] = []
            for row in rows {
                guard let id = row["id"] as? String else { continue }
                let areas = await Self.areas(client, instance: id)
                if let info = Self.parse(row, areas: areas) { loaded.append(info) }
            }
            instances = loaded
            error = nil
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            self.error = "instances_list failed: \(error.localizedDescription)"
        }
    }

    /// The ids of the areas `console_manifest` lists for `instance`, in order; nil when it
    /// cannot be read, so the tabs fall back to the account kind's.
    private static func areas(_ client: MCPClient, instance: String) async -> [String]? {
        guard let data = try? await client.call("console_manifest", ["instance": instance]),
            let areas = (data as? [String: Any])?["areas"] as? [[String: Any]]
        else { return nil }
        return areas.compactMap { $0["id"] as? String }
    }

    /// Reads one `instances_list` row and its manifest's `areas`; nil when it has no id.
    private static func parse(_ row: [String: Any], areas: [String]?) -> InstanceInfo? {
        guard let id = row["id"] as? String else { return nil }
        let kind = (row["account"] as? [String: Any])?["kind"] as? String ?? "linked"
        let home = kind == "home"
        let zones = row["zones"] as? [String] ?? []
        var tabs: [AppTab] = []
        for tab in (areas ?? []).compactMap({ AppTab.fromArea($0, home: home) })
        where !tabs.contains(tab) {
            tabs.append(tab)
        }
        if tabs.isEmpty {
            tabs = home ? [.sessions, .cost] : [.home]
        }
        // Settings always stays, so the person can always switch back.
        tabs.append(.settings)
        let website = zones.first.map { "https://\($0)" }
        let site = (row["sites"] as? [[String: Any]])?.first?["name"] as? String
        var repository: String?
        if let repo = row["repo"] as? [String: Any],
            let owner = repo["owner"] as? String, let name = repo["name"] as? String
        {
            repository = "\(owner)/\(name)"
        } else if let repo = row["repo"] as? String {
            repository = repo
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
