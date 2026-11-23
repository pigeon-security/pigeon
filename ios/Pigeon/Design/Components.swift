import SwiftUI
import UIKit

/// The shared layout numbers, from the mockup.
enum Metrics {
    /// Page side padding.
    static let side: CGFloat = 16
    /// Wider side padding for Users and User detail.
    static let sideWide: CGFloat = 20
    /// Space between cards.
    static let gap: CGFloat = 10
    /// Corner radius of a card.
    static let cardRadius: CGFloat = 14
    /// Corner radius of a large card.
    static let cardRadiusLarge: CGFloat = 16
    /// Offset of a card's hard shadow.
    static let shadow: CGFloat = 3
    /// Padding inside a card.
    static let cardPadding = EdgeInsets(top: 11, leading: 14, bottom: 12, trailing: 14)
    /// Space between lines in a card.
    static let rowSpacing: CGFloat = 4
    /// Height of a button.
    static let buttonHeight: CGFloat = 40
    /// Height of a small button.
    static let buttonHeightSmall: CGFloat = 36
    /// Corner radius of a button.
    static let buttonRadius: CGFloat = 10
    /// Space between buttons in a row.
    static let buttonGap: CGFloat = 8
    /// Indent of a section label or footnote.
    static let sectionIndent: CGFloat = 4
}

/// The dot grid behind every screen: soft dots on a 16 pt grid, drawn in one Canvas.
struct DotGrid: View {
    var body: some View {
        Canvas { context, size in
            let step = Tokens.gridStep
            let solid: CGFloat = 1.1
            let edge: CGFloat = 1.4
            let shading = GraphicsContext.Shading.radialGradient(
                Gradient(stops: [
                    .init(color: Tokens.dot, location: 0),
                    .init(color: Tokens.dot, location: solid / edge),
                    .init(color: Tokens.dot.opacity(0), location: 1),
                ]),
                center: .zero, startRadius: 0, endRadius: edge)
            var y = step / 2
            while y < size.height {
                var x = step / 2
                while x < size.width {
                    var dot = context
                    dot.translateBy(x: x, y: y)
                    dot.fill(
                        Path(ellipseIn: CGRect(x: -edge, y: -edge, width: edge * 2, height: edge * 2)),
                        with: shading)
                    x += step
                }
                y += step
            }
        }
        .background(Tokens.background)
        .ignoresSafeArea()
    }
}

/// A card: a filled surface with a hard offset shadow.
struct Block: ViewModifier {
    /// The corner radius.
    var radius: CGFloat = Metrics.cardRadius

    func body(content: Content) -> some View {
        content
            .background(RoundedRectangle(cornerRadius: radius).fill(Tokens.card))
            .background(
                RoundedRectangle(cornerRadius: radius)
                    .fill(Tokens.shadow)
                    .offset(x: Metrics.shadow, y: Metrics.shadow))
    }
}

extension View {
    /// Draws the view as a card.
    func block(radius: CGFloat = Metrics.cardRadius) -> some View {
        modifier(Block(radius: radius))
    }
}

extension Font {
    /// The system font for a type token.
    static func token(_ t: TypeToken) -> Font {
        .system(size: t.size, weight: weight(t.weight), design: t.mono ? .monospaced : .default)
    }

    private static func weight(_ w: Int) -> Font.Weight {
        switch w {
        case ..<150: return .ultraLight
        case ..<250: return .thin
        case ..<350: return .light
        case ..<450: return .regular
        case ..<550: return .medium
        case ..<650: return .semibold
        case ..<750: return .bold
        case ..<850: return .heavy
        default: return .black
        }
    }
}

/// The app's type scale, from the confirmed mockup (docs/plans/ios-app-next.md §4). Sentence
/// case everywhere; nothing is set in capitals. Numbers take `.monospacedDigit()` where drawn.
enum AppFont {
    /// Screen titles and the cost Total: 30 bold.
    static let screenTitle = Font.system(size: 30, weight: .bold)
    /// A source's total on the Cost screen (Cloudflare, AI Gateway): 20 bold.
    static let sourceTotal = Font.system(size: 20, weight: .bold)
    /// Card titles: 17 semibold.
    static let cardTitle = Font.system(size: 17, weight: .semibold)
    /// List rows: 17 regular.
    static let row = Font.system(size: 17, weight: .regular)
    /// Descriptions and buttons: 15 regular.
    static let body = Font.system(size: 15, weight: .regular)
    /// Button labels: 15 semibold.
    static let button = Font.system(size: 15, weight: .semibold)
    /// Labels, drawn muted: 13 semibold.
    static let label = Font.system(size: 13, weight: .semibold)
    /// Times and other meta: 13 regular.
    static let meta = Font.system(size: 13, weight: .regular)
    /// Chart axes and values: 11 regular.
    static let chart = Font.system(size: 11, weight: .regular)
    /// A note under a title: 14 regular.
    static let note = Font.system(size: 14, weight: .regular)
    /// A name: 24 bold.
    static let name = Font.system(size: 24, weight: .bold)
    /// Initials in an avatar: 28 semibold.
    static let avatarInitials = Font.system(size: 28, weight: .semibold)
    /// The initial in a tile: 17 bold.
    static let initial = Font.system(size: 17, weight: .bold)
    /// The large title: 30 bold.
    static let largeTitle = Font.system(size: 30, weight: .bold)
}

/// The app's chart colours, light and dark, checked for colour-blind distance (the plan's §4).
/// A series keeps its colour by name, never by rank.
enum ChartColour {
    /// CI Containers, bottom of the Cloudflare stack.
    static let ciContainers = pair(light: 0x7A55B5, dark: 0x9070CC)
    /// Workers.
    static let workers = pair(light: 0x008C82, dark: 0x00A396)
    /// D1.
    static let d1 = pair(light: 0x9C7C00, dark: 0xB39020)
    /// R2; also OpenAI in the AI Gateway chart, which has no R2.
    static let r2 = pair(light: 0xA64D87, dark: 0xC86BA6)
    /// Opus.
    static let opus = pair(light: 0xD97757, dark: 0xCF7050)
    /// Sonnet.
    static let sonnet = pair(light: 0x2A78D6, dark: 0x3A86DE)
    /// Haiku.
    static let haiku = pair(light: 0x558A42, dark: 0x5C9748)

    /// The colour of a named series: a Cloudflare line or a model family. Anything else (such
    /// as "Other") is muted.
    static func of(_ name: String) -> Color {
        switch name {
        case "CI Containers": return ciContainers
        case "Workers": return workers
        case "D1": return d1
        case "R2", "OpenAI": return r2
        case "Opus": return opus
        case "Sonnet": return sonnet
        case "Haiku": return haiku
        default: return Tokens.muted
        }
    }

    /// A colour that follows the light or dark appearance, from two 0xRRGGBB values.
    private static func pair(light: Int, dark: Int) -> Color {
        Color(UIColor { traits in
            let c = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(
                red: CGFloat((c >> 16) & 0xFF) / 255, green: CGFloat((c >> 8) & 0xFF) / 255,
                blue: CGFloat(c & 0xFF) / 255, alpha: 1)
        })
    }
}

/// A card with the mockup's padding, left aligned, with no text style of its own.
struct PlainCard<Content: View>: View {
    let radius: CGFloat
    let content: Content

    /// A card of `radius` around `content`.
    init(radius: CGFloat = Metrics.cardRadius, @ViewBuilder content: () -> Content) {
        self.radius = radius
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Metrics.rowSpacing) { content }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Metrics.cardPadding)
            .block(radius: radius)
    }
}

/// A card with no padding, for rows that bring their own.
struct GroupedCard<Content: View>: View {
    let content: Content

    /// A card around `content`.
    init(@ViewBuilder content: () -> Content) { self.content = content() }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) { content }
            .frame(maxWidth: .infinity, alignment: .leading)
            .block()
    }
}

/// A 1 pt line between rows, inset from the leading edge.
struct RowSeparator: View {
    /// The leading inset.
    let inset: CGFloat

    /// A separator inset by `inset`.
    init(inset: CGFloat) { self.inset = inset }

    var body: some View {
        Rectangle().fill(Tokens.line).frame(height: 1).padding(.leading, inset)
    }
}

/// The disclosure chevron at the end of a row.
struct Chevron: View {
    var body: some View {
        Image(systemName: "chevron.right")
            .resizable()
            .aspectRatio(contentMode: .fit)
            .fontWeight(.semibold)
            .frame(width: 7, height: 12)
            .foregroundStyle(Tokens.line)
    }
}

/// A muted section label above content.
struct CardSection<Content: View>: View {
    let title: String
    let content: Content

    /// A section called `title` around `content`.
    init(_ title: String, @ViewBuilder content: () -> Content) {
        self.title = title
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title)
                .font(AppFont.label)
                .foregroundStyle(Tokens.muted)
                .padding(.leading, Metrics.sectionIndent)
            content
        }
    }
}

/// A muted note under a card.
struct Footnote: View {
    let text: String

    /// A footnote reading `text`.
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(AppFont.meta)
            .foregroundStyle(Tokens.muted)
            .padding(.leading, Metrics.sectionIndent)
            .frame(maxWidth: .infinity, alignment: .leading)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// A count in a warn capsule; nothing at zero.
struct CountBadge: View {
    let count: Int

    /// A badge for `count`.
    init(count: Int) { self.count = count }

    var body: some View {
        if count > 0 {
            Text("\(count)")
                .font(.system(size: 20, weight: .bold))
                .monospacedDigit()
                .foregroundStyle(Tokens.onAccent)
                .padding(.horizontal, 8)
                .frame(minWidth: 34, minHeight: 34)
                .background(Capsule().fill(Tokens.warn))
        }
    }
}

/// A short success message.
struct Toast: View {
    let text: String

    /// A toast reading `text`.
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(AppFont.label)
            .foregroundStyle(Tokens.good)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A rounded square with content in the middle.
struct Tile<Content: View>: View {
    let size: CGFloat
    let radius: CGFloat
    let fill: Color
    let content: Content

    /// A `size` square of `fill` with `radius` corners around `content`.
    init(size: CGFloat, radius: CGFloat, fill: Color, @ViewBuilder content: () -> Content) {
        self.size = size
        self.radius = radius
        self.fill = fill
        self.content = content()
    }

    var body: some View {
        content
            .frame(width: size, height: size)
            .background(RoundedRectangle(cornerRadius: radius).fill(fill))
    }
}

/// The role of an action button.
enum ActionKind {
    /// The main action: accent fill.
    case primary
    /// A second action: chip fill.
    case secondary
    /// A risky action: chip fill, warn text.
    case destructive
}

/// The mockup's button: filled, 40 pt high, optionally full width.
struct ActionButtonStyle: ButtonStyle {
    let kind: ActionKind
    let small: Bool
    let fill: Bool

    /// A style for `kind`; `small` is 36 pt high; `fill` takes the full width offered.
    init(_ kind: ActionKind = .primary, small: Bool = false, fill: Bool = true) {
        self.kind = kind
        self.small = small
        self.fill = fill
    }

    func makeBody(configuration: Configuration) -> some View {
        ActionButtonBody(
            kind: kind, small: small, fill: fill, label: configuration.label,
            pressed: configuration.isPressed)
    }
}

/// The body of `ActionButtonStyle`, a view so it can read the enabled state.
private struct ActionButtonBody<Label: View>: View {
    let kind: ActionKind
    let small: Bool
    let fill: Bool
    let label: Label
    let pressed: Bool
    @Environment(\.isEnabled) private var isEnabled

    private var background: Color { kind == .primary ? Tokens.accent : Tokens.chip }
    private var text: Color {
        switch kind {
        case .primary: return Tokens.onAccent
        case .secondary: return Tokens.ink
        case .destructive: return Tokens.warn
        }
    }

    var body: some View {
        label
            .font(AppFont.button)
            .foregroundStyle(text)
            .padding(.horizontal, 14)
            .frame(maxWidth: fill ? .infinity : nil)
            .frame(height: small ? Metrics.buttonHeightSmall : Metrics.buttonHeight)
            .background(RoundedRectangle(cornerRadius: Metrics.buttonRadius).fill(background))
            .opacity(isEnabled ? (pressed ? 0.7 : 1) : 0.5)
    }
}

/// An answer option: full width, accent outline, no fill.
struct OptionButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(AppFont.button)
            .foregroundStyle(Tokens.accent)
            .padding(12)
            .frame(maxWidth: .infinity, minHeight: 40, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: Metrics.buttonRadius)
                    .strokeBorder(Tokens.accent, lineWidth: 1)
            )
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

/// A text-only button in the muted colour.
struct QuietButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(AppFont.button)
            .foregroundStyle(Tokens.muted)
            .frame(maxWidth: .infinity, minHeight: 36, alignment: .leading)
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}
