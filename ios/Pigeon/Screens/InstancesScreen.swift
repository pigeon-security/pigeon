import SwiftUI

/// Instances (platform-wide, home only): every instance, home first, as the web console's
/// Instances › All instances. Reads `instances_list`; tapping one opens its Overview. A new
/// instance is made in the web console, and a record is edited in Overview and Capabilities.
struct InstancesScreen: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @Environment(ManifestStore.self) private var store
    @Environment(ToastCenter.self) private var toasts
    @Environment(\.openArea) private var openArea
    @State private var state: Loadable<[Row]> = .loading
    @State private var opening: String?

    struct Row: Identifiable, Hashable {
        let id: String
        let title: String
        let isHome: Bool
        let host: String?
        let worker: String?
        let version: Int?
        let capabilities: [String]

        /// The capability sections a record may enable, as the web lists them.
        static let sections = ["publishing", "site_passkeys", "storage", "source", "ci", "observability", "ingest"]

        init?(_ j: JSON) {
            guard let id = j["id"].string else { return nil }
            self.id = id
            title = j["title"].string ?? id
            isHome = j["account"]["kind"].string == "home"
            host = j["zones"][0].string
            worker = j["worker"].string
            version = j["version"].int
            capabilities = Row.sections.filter { key in
                let v = j[key]
                return !v.isNull && v != .bool(false)
            }
        }
    }

    private var webURL: URL? {
        var parts = URLComponents(url: OAuth.base, resolvingAgainstBaseURL: false)
        parts?.path = "/app/\(context.instance.id)/instances"
        return parts?.url
    }

    var body: some View {
        List {
            PlatformViewPicker(context: context)
            LoadableSection(
                state: state, emptyTitle: "No instances yet", isEmpty: { $0.isEmpty },
                retry: { await load() }
            ) { rows in
                let home = rows.filter { $0.isHome }
                let linked = rows.filter { !$0.isHome }
                if !home.isEmpty {
                    Section("Home account") {
                        ForEach(home) { instanceRow($0) }
                    }
                }
                if !linked.isEmpty {
                    Section("Linked accounts (\(linked.count))") {
                        ForEach(linked) { instanceRow($0) }
                    }
                }
            }
            Section {
                if let webURL {
                    Link(destination: webURL) {
                        ListRow(title: "New instance", subtitle: "Made in the web console") {
                            Image(systemName: "arrow.up.right.square")
                                .foregroundStyle(Theme.muted)
                        }
                    }
                }
            } footer: {
                Text("A record is edited in Overview and Capabilities, with your passkey.")
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task { await load() }
    }

    private func instanceRow(_ r: Row) -> some View {
        Button {
            Task { await open(r) }
        } label: {
            HStack(alignment: .top, spacing: 10) {
                InstanceInitial(text: String(r.title.prefix(1)).uppercased(), size: 30)
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        Text(r.title).font(Theme.Fonts.body).foregroundStyle(Theme.ink).lineLimit(1)
                        if r.id == store.selected?.id {
                            Image(systemName: "checkmark")
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(Theme.accent)
                                .accessibilityLabel("Current")
                        }
                    }
                    Text(detail(r)).idStyle().foregroundStyle(Theme.muted)
                    if !r.capabilities.isEmpty {
                        Text(r.capabilities.joined(separator: " · "))
                            .font(Theme.Fonts.meta).foregroundStyle(Theme.ink2).lineLimit(2)
                    }
                }
                Spacer(minLength: 8)
                if opening == r.id {
                    ProgressView()
                } else {
                    StatusPill(r.isHome ? "Home" : "Linked", tone: r.isHome ? .accent : .ok)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityHint("Opens its Overview")
    }

    private func detail(_ r: Row) -> String {
        var parts = [r.host ?? r.id]
        if let worker = r.worker { parts.append(worker) }
        if let version = r.version { parts.append("v\(version)") }
        return parts.joined(separator: " · ")
    }

    /// Selects the instance (as the switcher does) and opens its Overview.
    private func open(_ r: Row) async {
        if r.id == store.selected?.id {
            openArea("overview")
            return
        }
        opening = r.id
        defer { opening = nil }
        if await store.select(r.id, api) != nil {
            openArea("overview")
        } else {
            toasts.show("Couldn't open \(r.title). Try again.", tone: .bad)
        }
    }

    private func load() async {
        do {
            let data = try await api.op("instances_list")
            let rows = (data.array ?? data["instances"].items).compactMap { Row($0) }
            state = .loaded(rows)
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            if state.value == nil { state = .failed(error.localizedDescription) }
        }
    }
}
