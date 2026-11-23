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
            .padding(Tokens.spaceGap)
            .block()
    }
}

/// A screen: dot grid, errors at the top, pull to refresh.
struct Screen<Content: View>: View {
    let errors: [String]
    let refresh: @MainActor () async -> Void
    @ViewBuilder let content: Content

    var body: some View {
        ZStack {
            DotGrid()
            ScrollView {
                VStack(alignment: .leading, spacing: Tokens.spaceGap) {
                    ForEach(errors, id: \.self) { ErrorBanner(text: $0) }
                    content
                }
                .padding(Tokens.spacePage)
            }
            .refreshable { await refresh() }
        }
    }
}

/// A card of text lines in a column.
struct Card<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 4) { content }
            .font(.token(Tokens.typeSmall))
            .foregroundStyle(Tokens.muted)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Tokens.spaceGap)
            .block()
    }
}

/// A card's heading line.
struct CardTitle: View {
    let text: String

    var body: some View {
        Text(text).font(.token(Tokens.typeHeading)).foregroundStyle(Tokens.ink)
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

/// A time in ms since the epoch as text, or "never".
func whenText(_ ms: Double?) -> String {
    guard let ms else { return "never" }
    return Date(timeIntervalSince1970: ms / 1000).formatted(date: .abbreviated, time: .shortened)
}

/// Any JSON answer as titled cards: a card for each object, with its fields as lines. Used
/// where the console's section is itself a plain listing of what the tool answers.
struct JSONCards: View {
    let value: Any?

    var body: some View {
        if let dict = value as? [String: Any] {
            ForEach(dict.keys.sorted(), id: \.self) { key in
                Eyebrow(text: key)
                parts(dict[key])
            }
        } else if let value {
            parts(value)
        }
    }

    @ViewBuilder
    private func parts(_ value: Any?) -> some View {
        if let rows = value as? [Any] {
            if rows.isEmpty { Card { Text("none") } }
            ForEach(Array(rows.enumerated()), id: \.offset) { _, row in fields(row) }
        } else {
            fields(value)
        }
    }

    @ViewBuilder
    private func fields(_ row: Any?) -> some View {
        Card {
            if let obj = row as? [String: Any] {
                ForEach(obj.keys.sorted(), id: \.self) { k in
                    Text("\(k): \(jsonText(obj[k]))")
                }
            } else {
                Text(jsonText(row))
            }
        }
    }
}
