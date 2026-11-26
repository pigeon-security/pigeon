import SwiftUI

// The shared components of the redesigned app (docs/plans/ios-app-redesign.md). Content is dense
// inset-grouped lists on the system background; glass is only for bars, the tab bar, the
// switcher, sheets and toasts.

// MARK: - Glass

extension View {
    /// Liquid Glass in `shape` on iOS 26 and later; the ultra-thin material before. For the
    /// switcher, the tab bar, toasts and other floating chrome only, never for content.
    @ViewBuilder
    func glass<S: Shape>(in shape: S, interactive: Bool = false) -> some View {
        if #available(iOS 26.0, *) {
            self.glassEffect(interactive ? .regular.interactive() : .regular, in: shape)
        } else {
            self.background(.ultraThinMaterial, in: shape)
                .overlay(shape.stroke(Theme.line.opacity(0.6), lineWidth: 0.5))
        }
    }
}

/// A capsule of glass around `content`: a toast.
struct GlassPill<Content: View>: View {
    var interactive = false
    @ViewBuilder let content: Content

    var body: some View {
        content
            .padding(.horizontal, 12)
            .frame(minHeight: 36)
            .glass(in: Capsule(), interactive: interactive)
    }
}

// MARK: - Status

/// The colour pair of a status: text and dot on a soft fill.
enum Tone: String, Hashable {
    case ok, warn, bad, info, accent, neutral

    var ink: Color {
        switch self {
        case .ok: return Theme.ok
        case .warn: return Theme.warn
        case .bad: return Theme.bad
        case .info: return Theme.info
        case .accent: return Theme.accent
        case .neutral: return Theme.ink2
        }
    }

    var soft: Color {
        switch self {
        case .ok: return Theme.okSoft
        case .warn: return Theme.warnSoft
        case .bad: return Theme.badSoft
        case .info: return Theme.infoSoft
        case .accent: return Theme.accentSoft
        case .neutral: return Theme.hover
        }
    }
}

/// A short status in a soft pill with a dot: "Live", "Missing", "4 min".
struct StatusPill: View {
    let text: String
    let tone: Tone

    init(_ text: String, tone: Tone) {
        self.text = text
        self.tone = tone
    }

    var body: some View {
        HStack(spacing: 4) {
            Circle().fill(tone.ink).frame(width: 5, height: 5)
            Text(text).lineLimit(1)
        }
        .font(Theme.Fonts.caption)
        .foregroundStyle(tone.ink)
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(tone.soft, in: RoundedRectangle(cornerRadius: 4))
        .accessibilityElement(children: .combine)
    }
}

/// A small outlined label, such as an inbox item's kind.
struct TagLabel: View {
    let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(Theme.Fonts.caption)
            .foregroundStyle(Theme.ink2)
            .padding(.horizontal, 5)
            .overlay(RoundedRectangle(cornerRadius: 4).stroke(Theme.line, lineWidth: 1))
    }
}

// MARK: - Rows

/// A list row: a title, an optional subtitle, and anything at the trailing edge. Put it in a
/// `List { Section { … } }` with `.listStyle(.insetGrouped)`.
struct ListRow<Trailing: View>: View {
    let title: String
    var subtitle: String?
    /// Sets the title in SF Mono (an id, a branch, a key).
    var monoTitle = false
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(monoTitle ? Theme.Fonts.idBody : Theme.Fonts.body)
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1).truncationMode(monoTitle ? .middle : .tail)
                if let subtitle, !subtitle.isEmpty {
                    Text(subtitle)
                        .font(Theme.Fonts.meta)
                        .foregroundStyle(Theme.muted)
                        .lineLimit(2)
                }
            }
            Spacer(minLength: 8)
            trailing
        }
        .frame(minHeight: 32)
    }
}

extension ListRow where Trailing == EmptyView {
    init(title: String, subtitle: String? = nil, monoTitle: Bool = false) {
        self.init(title: title, subtitle: subtitle, monoTitle: monoTitle) { EmptyView() }
    }
}

/// A fact: a label at the leading edge and its value at the trailing edge.
struct KeyValueRow: View {
    let label: String
    let value: String
    /// Sets the value in SF Mono.
    var mono = false

    init(_ label: String, _ value: String, mono: Bool = false) {
        self.label = label
        self.value = value
        self.mono = mono
    }

    var body: some View {
        LabeledContent {
            Text(value)
                .font(mono ? Theme.Fonts.id : Theme.Fonts.body)
                .foregroundStyle(Theme.ink2)
                .lineLimit(1).truncationMode(.middle)
                .textSelection(.enabled)
        } label: {
            Text(label).font(Theme.Fonts.body).foregroundStyle(Theme.ink)
        }
    }
}

/// An id, hash, host or path in SF Mono, selectable.
struct IDText: View {
    let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text).idStyle().foregroundStyle(Theme.ink2).textSelection(.enabled)
    }
}

// MARK: - Buttons

/// The large capsule button at the bottom of a sheet: primary (accent), danger (red) or quiet.
struct SheetButtonStyle: ButtonStyle {
    enum Kind { case primary, danger, quiet }
    let kind: Kind
    @Environment(\.isEnabled) private var isEnabled

    init(_ kind: Kind = .primary) { self.kind = kind }

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.body.weight(.semibold))
            .foregroundStyle(foreground)
            .frame(maxWidth: .infinity, minHeight: 50)
            .background(background, in: Capsule())
            .opacity(configuration.isPressed ? 0.75 : (isEnabled ? 1 : 0.5))
            .contentShape(Capsule())
    }

    private var foreground: Color {
        switch kind {
        case .primary: return Theme.accentInk
        case .danger: return Theme.bg
        case .quiet: return Theme.ink
        }
    }

    private var background: Color {
        switch kind {
        case .primary: return Theme.accent
        case .danger: return Theme.bad
        case .quiet: return Theme.hover
        }
    }
}

/// The small capsule button inside a row: Approve, Deny, Roll back.
struct RowButtonStyle: ButtonStyle {
    enum Kind { case primary, quiet, danger }
    let kind: Kind

    init(_ kind: Kind = .quiet) { self.kind = kind }

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(Theme.Fonts.meta.weight(.semibold))
            .foregroundStyle(kind == .primary ? Theme.accentInk : kind == .danger ? Theme.bad : Theme.ink)
            .padding(.horizontal, 12)
            .frame(minHeight: 30)
            .background(
                kind == .primary ? Theme.accent : kind == .danger ? Theme.badSoft : Theme.hover,
                in: Capsule())
            .opacity(configuration.isPressed ? 0.75 : 1)
    }
}

// MARK: - Area views

/// The views of an area as a segmented control (a menu for four or more), for the top of an
/// area's list. Draws nothing for an area with one view or none.
struct ViewPicker: View {
    let views: [AreaView]
    @Binding var selection: String

    var body: some View {
        if views.count > 1 {
            if views.count >= 4 {
                Picker("View", selection: $selection) {
                    ForEach(views) { Text($0.title).tag($0.id) }
                }
                .pickerStyle(.menu)
            } else {
                Picker("View", selection: $selection) {
                    ForEach(views) { Text($0.title).tag($0.id) }
                }
                .pickerStyle(.segmented)
            }
        }
    }
}

/// M4: a view the instance's capability version is too old for. Upgrade opens Capabilities.
struct UpgradeNotice: View {
    let view: AreaView
    @Environment(\.openArea) private var openArea

    var body: some View {
        if let up = view.upgrade {
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    StatusPill("Upgrade", tone: .warn)
                    Text("\(view.title) needs \(view.capability ?? "a newer capability") \(up.since ?? "")")
                        .font(Theme.Fonts.body).foregroundStyle(Theme.ink)
                }
                if let selected = up.selected {
                    Text("This instance runs \(selected).")
                        .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                }
                Button("Upgrade in Capabilities") { openArea("capabilities") }
                    .buttonStyle(RowButtonStyle(.primary))
            }
            .padding(.vertical, 4)
        }
    }
}

extension EnvironmentValues {
    /// Opens area `id` of the selected instance, when it has it: its tab when the area is in the
    /// bar, else pushed on More. Set by the root.
    @Entry var openArea: (String) -> Void = { _ in }
}

// MARK: - Toast

/// A short message over the content, at the top, gone after a few seconds.
@MainActor
@Observable
final class ToastCenter {
    struct Item: Equatable, Identifiable {
        let id = UUID()
        let text: String
        let tone: Tone
    }

    private(set) var current: Item?

    /// Shows `text` for about three seconds.
    func show(_ text: String, tone: Tone = .neutral) {
        let item = Item(text: text, tone: tone)
        current = item
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(3))
            if self?.current == item { self?.current = nil }
        }
    }

    func dismiss() { current = nil }
}

/// The toast capsule.
struct Toast: View {
    let item: ToastCenter.Item

    var body: some View {
        HStack(spacing: 8) {
            if item.tone != .neutral {
                Circle().fill(item.tone.ink).frame(width: 6, height: 6)
            }
            Text(item.text).font(Theme.Fonts.meta).foregroundStyle(Theme.ink).lineLimit(2)
        }
        .padding(.vertical, 9)
        .padding(.horizontal, 14)
        .glass(in: Capsule())
        .accessibilityAddTraits(.isStaticText)
    }
}

extension View {
    /// Shows `center`'s toast over this view, at the top.
    func toasts(_ center: ToastCenter) -> some View {
        overlay(alignment: .top) {
            if let item = center.current {
                Toast(item: item)
                    .padding(.top, 8)
                    .padding(.horizontal, 16)
                    .transition(.move(edge: .top).combined(with: .opacity))
                    .onTapGesture { center.dismiss() }
                    .id(item.id)
            }
        }
        .animation(.snappy, value: center.current)
    }
}
