import SwiftUI

/// What a list shows while it loads, when it is empty and when it failed: one treatment for
/// every list (the mock-up's "States").
enum Loadable<Value> {
    case loading
    case loaded(Value)
    case failed(String)

    var value: Value? {
        if case .loaded(let v) = self { return v }
        return nil
    }

    var error: String? {
        if case .failed(let e) = self { return e }
        return nil
    }
}

/// One read op and what it answered, for a screen. Keeps the last good value while it reloads.
///
///     @State private var releases = OpResource<JSON>("site_releases")
///     .task(id: instance) { await releases.load(api, args: ["instance": .string(instance)]) }
@MainActor
@Observable
final class OpResource<Value: Decodable> {
    let name: String
    private(set) var state: Loadable<Value> = .loading
    private(set) var reloading = false

    init(_ name: String) { self.name = name }

    var value: Value? { state.value }

    /// Runs the op with `args`. A failure replaces the value only when there was none.
    func load(_ api: ConsoleAPI, args: JSON = [:]) async {
        reloading = state.value != nil
        defer { reloading = false }
        do {
            let v = try await api.op(name, args: args, as: Value.self)
            state = .loaded(v)
        } catch is CancellationError {
            // A newer load replaced this one.
        } catch ConsoleError.signedOut {
            // The app is back at sign-in.
        } catch {
            if state.value == nil { state = .failed(error.localizedDescription) }
        }
    }

    /// Forgets the value, so the next load shows the skeleton (after switching instance).
    func reset() { state = .loading }
}

/// Skeleton rows, redacted, while a list loads. Put it inside a `Section`.
struct LoadingRows: View {
    var count = 3

    var body: some View {
        ForEach(0..<count, id: \.self) { _ in
            ListRow(title: "Loading the rows", subtitle: "A line of detail for the row") {
                Text("0000").font(Theme.Fonts.meta)
            }
            .redacted(reason: .placeholder)
            .accessibilityHidden(true)
        }
        .accessibilityLabel("Loading")
    }
}

/// An empty list: what would be here, and where it comes from.
struct EmptyState: View {
    let title: String
    var systemImage = "checkmark.circle"
    var message: String?

    var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: systemImage)
        } description: {
            if let message { Text(message) }
        }
    }
}

/// A failed read: what happened and a retry.
struct ErrorState: View {
    var title = "Couldn't load this"
    let message: String
    let retry: () async -> Void

    var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: "exclamationmark.triangle")
        } description: {
            Text(message).textSelection(.enabled)
        } actions: {
            Button("Try again") { Task { await retry() } }
                .buttonStyle(RowButtonStyle(.primary))
        }
    }
}

/// The body of a list for a `Loadable`: skeleton rows, the error with a retry, the empty
/// state, or `content` for the value. Use inside a `List`.
struct LoadableSection<Value, Content: View>: View {
    let state: Loadable<Value>
    var emptyTitle = "Nothing here"
    var emptyMessage: String?
    var isEmpty: (Value) -> Bool = { _ in false }
    let retry: () async -> Void
    @ViewBuilder let content: (Value) -> Content

    var body: some View {
        switch state {
        case .loading:
            Section { LoadingRows() }
        case .failed(let message):
            Section { ErrorState(message: message, retry: retry) }
        case .loaded(let value):
            if isEmpty(value) {
                Section { EmptyState(title: emptyTitle, message: emptyMessage) }
            } else {
                content(value)
            }
        }
    }
}
