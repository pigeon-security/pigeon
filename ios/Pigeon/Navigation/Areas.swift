import SwiftUI

/// What an area's screen is given: its area from the manifest, the instance, and the selected
/// view (bound to the view picker).
struct AreaContext {
    let area: Area
    let instance: ConsoleManifest.Instance
    /// The selected view's id; empty for an area with no views.
    let viewID: Binding<String>

    /// The selected view, if the area has views.
    var view: AreaView? { area.views.first { $0.id == viewID.wrappedValue } }
}

/// The screen for each area id. A screen agent ports an area by adding its case here; every
/// other area shows `AreaPlaceholder`, so the app builds and runs between phases. Each screen
/// is the root of its tab's `NavigationStack` (or pushed on More), under the large title that
/// `AreaRoot` adds; it draws a `List` (`.insetGrouped`) whose first section holds
/// `ViewPicker` when the area has views.
enum AreaRegistry {
    @MainActor
    @ViewBuilder
    static func content(_ context: AreaContext) -> some View {
        switch context.area.id {
        case "inbox": InboxScreen(context: context)
        case "overview": OverviewScreen(context: context)
        case "capabilities": CapabilitiesScreen(context: context)
        case "site": SiteScreen(context: context)
        case "storage": StorageScreen(context: context)
        case "source": SourceScreen(context: context)
        case "ci": CIScreen(context: context)
        case "metrics": MetricsScreen(context: context)
        case "ingest": IngestScreen(context: context)
        case "sessions": SessionsScreen(context: context)
        case "cost": CostScreen(context: context)
        case "access": AccessScreen(context: context)
        case "instances": InstancesScreen(context: context)
        case "platform": PlatformScreen(context: context)
        default:
            AreaPlaceholder(context: context)
        }
    }
}

/// The SF Symbol for an area, in the tab bar, More and the tab editor. Only this map is in the app; an area the app does not know
/// gets the generic symbol, so a new area on the server still gets a tab.
enum AreaSymbol {
    static func name(for id: String) -> String {
        switch id {
        case "inbox": return "tray"
        case "overview": return "square.grid.2x2"
        case "site": return "globe"
        case "storage": return "externaldrive"
        case "source": return "arrow.triangle.branch"
        case "ci": return "checkmark.circle"
        case "metrics": return "chart.xyaxis.line"
        case "ingest": return "square.and.arrow.down"
        case "capabilities": return "puzzlepiece.extension"
        case "sessions": return "waveform.path.ecg"
        case "cost": return "chart.bar"
        case "access": return "key"
        case "instances": return "square.stack"
        case "platform": return "square.3.layers.3d"
        default: return "square.dashed"
        }
    }
}

/// The root of one area, in its own tab or pushed on More: the area's screen under its large
/// title. The switcher is not here; it is pinned beside the tab bar (`ShellBar`). Keeps which of
/// the area's views is selected.
struct AreaRoot: View {
    let area: Area
    let instance: ConsoleManifest.Instance
    @State private var viewID: String

    init(area: Area, instance: ConsoleManifest.Instance) {
        self.area = area
        self.instance = instance
        _viewID = State(initialValue: area.views.first?.id ?? "")
    }

    var body: some View {
        AreaRegistry.content(AreaContext(area: area, instance: instance, viewID: $viewID))
            .navigationTitle(area.title)
            .navigationBarTitleDisplayMode(.large)
            .onChange(of: area.views) { _, views in
                if !views.contains(where: { $0.id == viewID }) { viewID = views.first?.id ?? "" }
            }
    }
}

/// An area that is not ported yet: its views, any upgrade notice, and a link to the same page
/// on the web console.
struct AreaPlaceholder: View {
    let context: AreaContext

    private var webURL: URL? {
        var segment = CharacterSet.urlPathAllowed
        segment.remove("/")
        let ids = [context.instance.id, context.area.id] + (context.view.map { [$0.id] } ?? [])
        let path = ids.map { $0.addingPercentEncoding(withAllowedCharacters: segment) ?? $0 }
        var parts = URLComponents(url: OAuth.base, resolvingAgainstBaseURL: false)
        parts?.percentEncodedPath = "/app/" + path.joined(separator: "/")
        return parts?.url
    }

    var body: some View {
        List {
            if context.area.views.count > 1 {
                Section {
                    ViewPicker(views: context.area.views, selection: context.viewID)
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                }
            }
            if let view = context.view, view.upgrade != nil {
                Section { UpgradeNotice(view: view) }
            }
            Section {
                ContentUnavailableView {
                    Label(context.area.title, systemImage: AreaSymbol.name(for: context.area.id))
                } description: {
                    Text(
                        "\(context.view?.title ?? context.area.title) on \(context.instance.title) isn't in the app yet. The web console has it."
                    )
                } actions: {
                    if let webURL {
                        Link("Open in the web console", destination: webURL)
                            .buttonStyle(RowButtonStyle(.primary))
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
    }
}
