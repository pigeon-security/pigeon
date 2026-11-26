import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// One fact in a confirm sheet: Instance, Tool, For, Expires.
struct Fact: Hashable, Identifiable {
    let label: String
    let value: String
    var mono = false
    var id: String { label }

    init(_ label: String, _ value: String, mono: Bool = false) {
        self.label = label
        self.value = value
        self.mono = mono
    }
}

/// A write for the confirm sheet: what it is, the target and the exact change, as the web's
/// `confirm({title, target, diff, action, id, change, danger})` takes it.
struct WriteRequest: Identifiable {
    let id = UUID()
    /// The sheet's title: "Approve tool call", "Roll back the site?".
    var title: String
    /// One line under the title.
    var subtitle: String?
    /// The facts about the target, shown first.
    var facts: [Fact] = []
    /// The change, as the app knows it before begin.
    var lines: [DiffLine] = []
    /// The console write, such as `approval-approve` or `site-rollback`.
    var action: String
    /// The write's target: an id, a name, or `{instance, site, host}`.
    var target: JSON
    /// The write's change (`.null` for none).
    var change: JSON = .null
    /// Makes the button the danger variant.
    var destructive = false
    /// The button's text; "Approve with passkey" when nil.
    var confirmLabel: String?
    /// Called with the finished write, before the sheet closes (or shows a reveal).
    var onDone: @MainActor (WriteResult) -> Void = { _ in }
}

/// A message for a form field that is not ready, thrown from a confirm sheet's `prepare`.
struct InputError: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

/// The one sheet for every write (decision: one confirm sheet). It names the target and the
/// exact change, then runs begin → the passkey → finish through `ConsoleAPI.write`, showing the
/// platform's own summary while the passkey sheet is up. A write that reveals something turns
/// into the show-once content. On success it closes and shows the platform's message as a toast.
///
/// `form` adds fields (a secret's value, an invite's name); `prepare` reads them when the
/// person confirms and answers the change to send, or throws an `InputError`.
struct ConfirmSheet<Form: View>: View {
    let request: WriteRequest
    let prepare: (@MainActor () throws -> JSON)?
    let form: Form

    @Environment(ConsoleAPI.self) private var api
    @Environment(ToastCenter.self) private var toasts
    @Environment(\.dismiss) private var dismiss
    @State private var working = false
    @State private var status: String?
    @State private var failure: String?
    @State private var summary: WriteSummary?
    @State private var reveal: Reveal?
    @State private var task: Task<Void, Never>?

    init(
        _ request: WriteRequest, prepare: (@MainActor () throws -> JSON)? = nil,
        @ViewBuilder form: () -> Form
    ) {
        self.request = request
        self.prepare = prepare
        self.form = form()
    }

    var body: some View {
        ScrollView {
            if let reveal {
                ShowOnceContent(title: request.title, reveal: reveal) { dismiss() }
            } else {
                confirm
            }
        }
        .scrollBounceBehavior(.basedOnSize)
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .interactiveDismissDisabled(working || reveal != nil)
        .onDisappear { task?.cancel() }
    }

    private var confirm: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 2) {
                Text(request.title).font(.title3.weight(.bold)).foregroundStyle(Theme.ink)
                if let subtitle = request.subtitle {
                    Text(subtitle).font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                }
            }
            if !request.facts.isEmpty {
                Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 14, verticalSpacing: 6) {
                    ForEach(request.facts) { fact in
                        GridRow {
                            Text(fact.label).foregroundStyle(Theme.muted)
                            Text(fact.value)
                                .font(fact.mono ? Theme.Fonts.id : Theme.Fonts.body)
                                .foregroundStyle(Theme.ink)
                                .textSelection(.enabled)
                        }
                    }
                }
                .font(Theme.Fonts.body)
            }
            if !request.lines.isEmpty { DiffBlock(lines: request.lines) }
            form
            if let summary, !summary.lines.isEmpty, summary.lines != request.lines {
                VStack(alignment: .leading, spacing: 6) {
                    Text("The platform will make this change")
                        .font(Theme.Fonts.caption).textCase(.uppercase)
                        .foregroundStyle(Theme.muted)
                    DiffBlock(lines: summary.lines)
                }
            }
            if let failure {
                Text(failure).font(Theme.Fonts.meta).foregroundStyle(Theme.bad)
                    .textSelection(.enabled)
            } else if let status {
                HStack(spacing: 8) {
                    ProgressView()
                    Text(status).font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                }
            }
            VStack(spacing: 10) {
                Button {
                    run()
                } label: {
                    Label(
                        request.confirmLabel ?? "Approve with passkey",
                        systemImage: "faceid")
                }
                .buttonStyle(SheetButtonStyle(destructive ? .danger : .primary))
                .disabled(working)
                Button("Cancel") { dismiss() }
                    .buttonStyle(SheetButtonStyle(.quiet))
                    .disabled(working)
            }
            .padding(.top, 4)
        }
        .padding(.horizontal, 20)
        .padding(.top, 24)
        .padding(.bottom, 16)
    }

    private var destructive: Bool { request.destructive || summary?.destructive == true }

    private func run() {
        var change = request.change
        if let prepare {
            do {
                change = try prepare()
            } catch {
                failure = error.localizedDescription
                return
            }
        }
        failure = nil
        working = true
        status = "Preparing…"
        task = Task {
            defer { working = false }
            do {
                let result = try await api.write(
                    action: request.action, target: request.target, change: change
                ) { s in
                    summary = s
                    status = "Waiting for your passkey…"
                }
                status = nil
                request.onDone(result)
                if let r = result.reveal {
                    reveal = r
                } else {
                    toasts.show(result.message, tone: .ok)
                    dismiss()
                }
            } catch is CancellationError {
                status = nil
            } catch {
                status = nil
                failure = error.localizedDescription
            }
        }
    }
}

extension ConfirmSheet where Form == EmptyView {
    /// A confirm sheet with no form fields.
    init(_ request: WriteRequest) {
        self.init(request, prepare: nil) { EmptyView() }
    }
}

extension View {
    /// Presents the confirm sheet for `request` while it is set.
    func confirmSheet(_ request: Binding<WriteRequest?>) -> some View {
        sheet(item: request) { ConfirmSheet($0) }
    }
}

/// The change as lines: additions in green, removals in red, notes in ink.
struct DiffBlock: View {
    let lines: [DiffLine]

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(lines) { line in
                Text(prefix(line) + line.text)
                    .foregroundStyle(colour(line))
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .font(.system(.caption, design: .monospaced))
        .textSelection(.enabled)
        .padding(.vertical, 8)
        .padding(.horizontal, 10)
        .background(Theme.raised, in: RoundedRectangle(cornerRadius: 10))
    }

    private func prefix(_ line: DiffLine) -> String {
        switch line.kind {
        case .add: return "+ "
        case .remove: return "− "
        case .note: return ""
        }
    }

    private func colour(_ line: DiffLine) -> Color {
        switch line.kind {
        case .add: return Theme.ok
        case .remove: return Theme.bad
        case .note: return Theme.ink2
        }
    }
}

/// The one sheet that shows a token or an invite link once (decision: one show-once sheet). It
/// cannot be swiped away until Done.
struct ShowOnceSheet: View {
    let title: String
    let reveal: Reveal
    @Environment(\.dismiss) private var dismiss

    init(title: String, reveal: Reveal) {
        self.title = title
        self.reveal = reveal
    }

    /// A sheet for `value`, with `note` above it; `kind` is `token` or `invite`.
    init(title: String, value: String, note: String? = nil, kind: String = "token") {
        self.init(title: title, reveal: Reveal(value: value, note: note, kind: kind))
    }

    var body: some View {
        ScrollView {
            ShowOnceContent(title: title, reveal: reveal) { dismiss() }
        }
        .scrollBounceBehavior(.basedOnSize)
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .interactiveDismissDisabled()
    }
}

/// The show-once body: the note, the value, Copy and Done.
struct ShowOnceContent: View {
    let title: String
    let reveal: Reveal
    let done: () -> Void
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.title3.weight(.bold)).foregroundStyle(Theme.ink)
                Text(reveal.note ?? reveal.rule)
                    .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            }
            Text(reveal.value)
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(Theme.ink)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(10)
                .background(Theme.raised, in: RoundedRectangle(cornerRadius: 10))
            if reveal.note != nil {
                Text(reveal.rule).font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            }
            VStack(spacing: 10) {
                Button(copied ? "Copied" : (reveal.kind == "invite" ? "Copy link" : "Copy")) {
                    copy()
                }
                .buttonStyle(SheetButtonStyle(.primary))
                Button("Done", action: done)
                    .buttonStyle(SheetButtonStyle(.quiet))
            }
        }
        .padding(.horizontal, 20)
        .padding(.top, 24)
        .padding(.bottom, 16)
    }

    /// Copies the value to this device only, for two minutes.
    private func copy() {
        UIPasteboard.general.setItems(
            [[UTType.plainText.identifier: reveal.value]],
            options: [.localOnly: true, .expirationDate: Date().addingTimeInterval(120)])
        copied = true
    }
}
