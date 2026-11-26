import Foundation
import Observation

/// One view of an area, as `console_manifest` lists it (Site › Deployments).
struct AreaView: Decodable, Hashable, Identifiable {
    let id: String
    let title: String
    /// The capability the view needs, if any.
    let capability: String?
    /// Set when the instance runs a version of `capability` older than the view needs.
    let upgrade: Upgrade?

    struct Upgrade: Decodable, Hashable {
        /// The version the instance runs.
        let selected: String?
        /// The version the view needs.
        let since: String?
    }
}

/// One area of the console for the selected instance: a tab.
struct Area: Decodable, Hashable, Identifiable {
    let id: String
    let title: String
    /// `top`, `instance` or `platform`.
    let group: String
    let views: [AreaView]

    private enum CodingKeys: String, CodingKey { case id, title, group, views }

    init(id: String, title: String, group: String = "instance", views: [AreaView] = []) {
        self.id = id
        self.title = title
        self.group = group
        self.views = views
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? id.capitalized
        group = try c.decodeIfPresent(String.self, forKey: .group) ?? "instance"
        views = try c.decodeIfPresent([AreaView].self, forKey: .views) ?? []
    }

    /// Whether the area is platform-wide (shown on the home instance only).
    var isPlatform: Bool { group == "platform" }
}

/// What `console_manifest` answers for one instance: its areas in order, and the inbox count.
struct ConsoleManifest: Decodable, Equatable {
    struct Instance: Decodable, Equatable {
        let id: String
        let title: String
        let home: Bool
        let host: String?

        private enum CodingKeys: String, CodingKey { case id, title, home, host }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            id = try c.decode(String.self, forKey: .id)
            title = try c.decodeIfPresent(String.self, forKey: .title) ?? id
            home = try c.decodeIfPresent(Bool.self, forKey: .home) ?? false
            host = try c.decodeIfPresent(String.self, forKey: .host)
        }
    }

    struct Inbox: Decodable, Equatable {
        let count: Int
    }

    let instance: Instance
    let areas: [Area]
    let inbox: Inbox?

    /// The area with `id`, if the manifest lists it.
    func area(_ id: String) -> Area? { areas.first { $0.id == id } }
}

/// One row of `instances_list`, as the switcher shows it.
struct InstanceSummary: Identifiable, Hashable {
    let id: String
    let title: String
    /// `home` for the platform's own account, `linked` for another.
    let accountKind: String
    let zones: [String]

    var isHome: Bool { accountKind == "home" }
    /// The first zone, shown under the title.
    var host: String? { zones.first }
    /// The letter on the instance's tile.
    var initial: String { String(title.prefix(1)).uppercased() }

    init(id: String, title: String, accountKind: String, zones: [String]) {
        self.id = id
        self.title = title
        self.accountKind = accountKind
        self.zones = zones
    }

    /// Reads one row; nil when it has no id.
    init?(_ row: JSON) {
        guard let id = row["id"].string else { return nil }
        self.id = id
        title = row["title"].string ?? id
        accountKind = row["account"]["kind"].string ?? "linked"
        zones = row["zones"].items.compactMap(\.string)
    }
}

/// The instances, which one is selected, and its `console_manifest`: the only input to the
/// app's navigation. The selection is kept in `UserDefaults` under `selectedInstance`.
@MainActor
@Observable
final class ManifestStore {
    enum Phase: Equatable {
        case loading
        case ready
        case failed(String)
    }

    private(set) var instances: [InstanceSummary] = []
    /// Manifests read so far, by instance id (the selected one is always fresh).
    private(set) var manifests: [String: ConsoleManifest] = [:]
    private(set) var phase: Phase = .loading
    private(set) var selectedID: String? = UserDefaults.standard.string(forKey: "selectedInstance")

    /// The selected instance: the stored choice, else the home instance, else the first.
    var selected: InstanceSummary? {
        instances.first { $0.id == selectedID } ?? instances.first { $0.isHome } ?? instances.first
    }

    /// The selected instance's manifest.
    var manifest: ConsoleManifest? { selected.flatMap { manifests[$0.id] } }

    /// The tabs, in manifest order.
    var areas: [Area] { manifest?.areas ?? [] }

    /// The Inbox badge: `inbox.count` of the selected instance's manifest.
    var inboxCount: Int { manifest?.inbox?.count ?? 0 }

    /// Loads `instances_list`, then the selected instance's `console_manifest`.
    func load(_ api: ConsoleAPI) async {
        if manifest == nil { phase = .loading }
        do {
            let data = try await api.op("instances_list")
            let rows = data.array ?? data["instances"].items
            instances = rows.compactMap { InstanceSummary($0) }
            guard let id = selected?.id else {
                phase = .failed("No instances are listed for you.")
                return
            }
            try await loadManifest(id, api)
            phase = .ready
        } catch is CancellationError {
            // A newer load replaced this one.
        } catch {
            if manifest == nil { phase = .failed(error.localizedDescription) }
        }
    }

    /// Re-reads the selected instance's manifest (after a write that changes it, or to refresh
    /// the Inbox badge).
    func refresh(_ api: ConsoleAPI) async {
        guard let id = selected?.id else { return }
        try? await loadManifest(id, api)
    }

    /// Selects instance `id` and loads its manifest. Answers the manifest, or nil when it
    /// could not be read (the previous instance stays selected).
    @discardableResult
    func select(_ id: String, _ api: ConsoleAPI) async -> ConsoleManifest? {
        do {
            let m = try await loadManifest(id, api)
            selectedID = id
            UserDefaults.standard.set(id, forKey: "selectedInstance")
            return m
        } catch {
            return nil
        }
    }

    /// Reads the manifests not read yet, for the switcher's waiting counts. Failures are skipped.
    func loadOthers(_ api: ConsoleAPI) async {
        for i in instances where manifests[i.id] == nil {
            _ = try? await loadManifest(i.id, api)
        }
    }

    @discardableResult
    private func loadManifest(_ id: String, _ api: ConsoleAPI) async throws -> ConsoleManifest {
        let m = try await api.op("console_manifest", args: ["instance": .string(id)], as: ConsoleManifest.self)
        manifests[id] = m
        return m
    }

    /// Forgets everything, on sign-out.
    func reset() {
        instances = []
        manifests = [:]
        phase = .loading
    }
}
