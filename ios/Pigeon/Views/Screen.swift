import SwiftUI

/// Loads one MCP tool and keeps its data and its last error.
@MainActor
@Observable
final class ToolModel {
    private(set) var data: Any?
    private(set) var error: String?
    let tool: String
    let args: [String: Any]

    /// A model for `tool` called with `args`.
    init(_ tool: String, _ args: [String: Any] = [:]) {
        self.tool = tool
        self.args = args
    }

    /// Calls the tool. The error stays until the next call that works.
    func load(_ client: MCPClient) async {
        do {
            data = try await client.call(tool, args)
            error = nil
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch {
            self.error = "\(tool) failed: \(error.localizedDescription)"
        }
    }
}

/// The last error of a screen, shown at the top until a later success clears it.
struct ErrorBanner: View {
    let text: String

    var body: some View {
        Text(text)
            .textSelection(.enabled)
            .font(AppFont.meta)
            .foregroundStyle(Tokens.warn)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Metrics.cardPadding)
            .block()
    }
}

/// The page title row: a left-aligned large title with an optional control at the right.
struct ScreenHeader: View {
    let title: String
    let trailing: AnyView?
    let showsBack: Bool
    @Environment(\.dismiss) private var dismiss

    /// A title with Back on pushed screens and an optional control on the right.
    init(_ title: String, trailing: AnyView? = nil, showsBack: Bool = false) {
        self.title = title
        self.trailing = trailing
        self.showsBack = showsBack
    }

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            if showsBack {
                Button { dismiss() } label: {
                    Image(systemName: "chevron.backward")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(Tokens.ink)
                        .frame(width: 44, height: 44)
                        .background(Circle().fill(Tokens.chip))
                        .contentShape(Circle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Back")
            }
            Text(title)
                .font(AppFont.largeTitle).tracking(-0.4)
                .foregroundStyle(Tokens.ink)
                .lineLimit(1).minimumScaleFactor(0.7)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityAddTraits(.isHeader)
            if let trailing { trailing }
        }
        .frame(minHeight: 44)
    }
}

/// A small rounded label: text on a soft fill.
struct Chip: View {
    let text: String
    let colour: Color
    let fill: Color

    /// A chip reading `text` in `colour` on `fill`.
    init(_ text: String, colour: Color = Tokens.muted, fill: Color = Tokens.chip) {
        self.text = text
        self.colour = colour
        self.fill = fill
    }

    var body: some View {
        Text(text)
            .font(AppFont.label)
            .foregroundStyle(colour)
            .padding(.horizontal, 10)
            .padding(.vertical, 3)
            .background(Capsule().fill(fill))
    }
}

/// A screen: dot grid, title row, errors at the top, pull to refresh.
struct Screen<Content: View>: View {
    let title: String?
    let trailing: AnyView?
    let root: Bool
    let errors: [String]
    let side: CGFloat
    let refresh: @MainActor () async -> Void
    let content: Content

    /// A screen with `side` padding at the page edges. With a `title`, the shared header is drawn
    /// in place of the navigation bar. A `root` screen leaves out the Back control.
    init(
        title: String? = nil, trailing: AnyView? = nil, root: Bool = false,
        errors: [String], side: CGFloat = Metrics.side,
        refresh: @escaping @MainActor () async -> Void,
        @ViewBuilder content: () -> Content
    ) {
        self.title = title
        self.trailing = trailing
        self.root = root
        self.errors = errors
        self.side = side
        self.refresh = refresh
        self.content = content()
    }

    var body: some View {
        ZStack {
            DotGrid()
            ScrollView {
                VStack(alignment: .leading, spacing: Metrics.gap) {
                    if let title { ScreenHeader(title, trailing: trailing, showsBack: !root) }
                    ForEach(errors, id: \.self) { ErrorBanner(text: $0) }
                    content
                }
                .padding(.horizontal, side)
                .padding(.top, 4)
                // Room for the floating tab bar, so the last card's buttons stay reachable.
                .padding(.bottom, 80)
            }
            .refreshable { await refresh() }
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(title != nil ? .hidden : .automatic, for: .navigationBar)
    }
}

/// Text for any JSON value: scalars as they are, containers as sorted, indented JSON.
func jsonText(_ value: Any?) -> String {
    guard let value, !(value is NSNull) else { return "none" }
    if let s = value as? String { return s }
    if let n = value as? NSNumber { return n.stringValue }
    guard JSONSerialization.isValidJSONObject(value),
        let data = try? JSONSerialization.data(
            withJSONObject: value, options: [.prettyPrinted, .sortedKeys]),
        let text = String(data: data, encoding: .utf8)
    else { return String(describing: value) }
    return text
}
