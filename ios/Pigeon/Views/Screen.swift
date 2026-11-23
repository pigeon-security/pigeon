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

/// A screen: dot grid, errors at the top, pull to refresh.
struct Screen<Content: View>: View {
    let errors: [String]
    let side: CGFloat
    let refresh: @MainActor () async -> Void
    let content: Content

    /// A screen with `side` padding at the page edges.
    init(
        errors: [String], side: CGFloat = Metrics.side,
        refresh: @escaping @MainActor () async -> Void,
        @ViewBuilder content: () -> Content
    ) {
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
                    ForEach(errors, id: \.self) { ErrorBanner(text: $0) }
                    content
                }
                .padding(.horizontal, side)
                .padding(.top, 4)
                .padding(.bottom, 16)
            }
            .refreshable { await refresh() }
        }
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
