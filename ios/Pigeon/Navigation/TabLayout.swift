import Foundation
import Observation

/// Which of an instance's areas sit in the tab bar, in order, and the order of the rest in More.
/// Ids only: a layout is matched to the instance's manifest with `resolved(for:)` before use.
struct TabLayout: Codable, Equatable {
    /// How many areas fit in the bar beside the switcher and More.
    static let maxPinned = 3

    var pinned: [String]
    var overflow: [String]

    /// The layout an instance starts with: its first three areas in manifest order in the bar,
    /// the rest in More.
    static func defaults(for areas: [Area]) -> TabLayout {
        let ids = unique(areas.map(\.id))
        return TabLayout(pinned: Array(ids.prefix(maxPinned)), overflow: Array(ids.dropFirst(maxPinned)))
    }

    /// This layout matched to `areas`: ids the manifest does not list are dropped, areas the
    /// layout has never seen go to the end of More in manifest order (so a new area on the
    /// server still shows), and when no pinned area is left the defaults are used.
    func resolved(for areas: [Area]) -> TabLayout {
        let known = Set(areas.map(\.id))
        let pinned = Array(Self.unique(self.pinned.filter(known.contains)).prefix(Self.maxPinned))
        guard !pinned.isEmpty else { return Self.defaults(for: areas) }
        let placed = Set(pinned)
        var overflow = Self.unique(self.overflow.filter { known.contains($0) && !placed.contains($0) })
        let seen = placed.union(overflow)
        overflow += Self.unique(areas.map(\.id).filter { !seen.contains($0) })
        return TabLayout(pinned: pinned, overflow: overflow)
    }

    /// Whether the bar has room for another area.
    var canPin: Bool { pinned.count < Self.maxPinned }

    /// Whether an area can leave the bar (one always stays).
    var canUnpin: Bool { pinned.count > 1 }

    /// Moves area `id` from More to the end of the bar, when there is room.
    mutating func pin(_ id: String) {
        guard canPin, let i = overflow.firstIndex(of: id) else { return }
        overflow.remove(at: i)
        pinned.append(id)
    }

    /// Moves area `id` from the bar to the top of More, unless it is the last one in the bar.
    mutating func unpin(_ id: String) {
        guard canUnpin, let i = pinned.firstIndex(of: id) else { return }
        pinned.remove(at: i)
        overflow.insert(id, at: 0)
    }

    /// Reorders the bar, with `List.onMove`'s arguments.
    mutating func movePinned(from source: IndexSet, to destination: Int) {
        pinned = Self.moved(pinned, from: source, to: destination)
    }

    /// Reorders More, with `List.onMove`'s arguments.
    mutating func moveOverflow(from source: IndexSet, to destination: Int) {
        overflow = Self.moved(overflow, from: source, to: destination)
    }

    /// `items` with the elements at `source` moved to before the element that was at
    /// `destination` (or to the end), as `MutableCollection.move(fromOffsets:toOffset:)` does.
    static func moved(_ items: [String], from source: IndexSet, to destination: Int) -> [String] {
        let source = source.filter { items.indices.contains($0) }
        guard !source.isEmpty else { return items }
        let moving = source.map { items[$0] }
        var rest = items
        for i in source.reversed() { rest.remove(at: i) }
        let target = min(max(destination - source.filter { $0 < destination }.count, 0), rest.count)
        rest.insert(contentsOf: moving, at: target)
        return rest
    }

    private static func unique(_ ids: [String]) -> [String] {
        var seen = Set<String>()
        return ids.filter { seen.insert($0).inserted }
    }
}

/// Each instance's tab layout, kept in `UserDefaults` under `tabLayouts` (one JSON object by
/// instance id). An instance with no stored layout gets `TabLayout.defaults(for:)`.
@MainActor
@Observable
final class TabLayoutStore {
    private static let key = "tabLayouts"
    private let defaults: UserDefaults
    private var layouts: [String: TabLayout]

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        layouts = defaults.data(forKey: Self.key)
            .flatMap { try? JSONDecoder().decode([String: TabLayout].self, from: $0) } ?? [:]
    }

    /// Instance `instanceID`'s layout, matched to its `areas`.
    func layout(for instanceID: String, areas: [Area]) -> TabLayout {
        layouts[instanceID]?.resolved(for: areas) ?? TabLayout.defaults(for: areas)
    }

    /// Changes instance `instanceID`'s layout and stores it.
    func update(_ instanceID: String, areas: [Area], _ change: (inout TabLayout) -> Void) {
        var layout = layout(for: instanceID, areas: areas)
        change(&layout)
        layouts[instanceID] = layout
        save()
    }

    /// Forgets instance `instanceID`'s layout, so it has the defaults again.
    func reset(_ instanceID: String) {
        layouts[instanceID] = nil
        save()
    }

    private func save() {
        if let data = try? JSONEncoder().encode(layouts) { defaults.set(data, forKey: Self.key) }
    }
}
