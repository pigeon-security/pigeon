import SwiftUI
import UIKit

/// The dot grid behind every screen.
struct DotGrid: View {
    var body: some View {
        Canvas { context, size in
            let step = Tokens.gridStep
            let r = Tokens.dotSize
            var y = step / 2
            while y < size.height {
                var x = step / 2
                while x < size.width {
                    context.fill(
                        Path(ellipseIn: CGRect(x: x - r, y: y - r, width: r * 2, height: r * 2)),
                        with: .color(Tokens.dot))
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
    func body(content: Content) -> some View {
        content
            .background(RoundedRectangle(cornerRadius: Tokens.radiusCard).fill(Tokens.card))
            .background(
                RoundedRectangle(cornerRadius: Tokens.radiusCard)
                    .fill(Tokens.shadow)
                    .offset(x: Tokens.shadowOffset, y: Tokens.shadowOffset))
    }
}

extension View {
    /// Draws the view as a card.
    func block() -> some View { modifier(Block()) }
}

/// A label in mono capitals.
struct Eyebrow: View {
    let text: String

    var body: some View {
        Text(text.uppercased())
            .font(.token(Tokens.typeEyebrow))
            .tracking(1)
            .foregroundStyle(Tokens.muted)
    }
}

/// The accent-filled button.
struct PrimaryButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.token(Tokens.typeButton))
            .foregroundStyle(Tokens.onAccent)
            .padding(.vertical, 12)
            .padding(.horizontal, 24)
            .background(RoundedRectangle(cornerRadius: Tokens.radiusButton).fill(Tokens.accent))
            .opacity(configuration.isPressed ? 0.8 : 1)
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
/// case everywhere; nothing is set in capitals.
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
    /// The tab bar's labels: 10 medium.
    static let tab = Font.system(size: 10, weight: .medium)
}

/// The app's chart colours, light and dark, checked for colour-blind distance (the plan's §4).
/// A series keeps its colour by name, never by rank.
enum ChartColour {
    /// Violet: CI Containers, Opus, planner.
    static let violet = pair(light: 0x4E3FA6, dark: 0xA898F5)
    /// Teal: Workers, Sonnet, executor.
    static let teal = pair(light: 0x2E9E92, dark: 0x66D6C4)
    /// Ochre: D1, Haiku.
    static let ochre = pair(light: 0xB07A10, dark: 0xE0B04A)
    /// Magenta: R2.
    static let magenta = pair(light: 0x9C2462, dark: 0xC9508F)

    /// The colour of a named series: a Cloudflare line, a model family or a role. Anything else
    /// (such as "Other") is muted.
    static func of(_ name: String) -> Color {
        switch name {
        case "CI Containers", "Opus", "planner": return violet
        case "Workers", "Sonnet", "executor": return teal
        case "D1", "Haiku": return ochre
        case "R2": return magenta
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

/// A button with an accent outline and no fill, for the second choice beside a filled one.
struct OutlinedButtonStyle: ButtonStyle {
    /// The outline and text colour; the accent unless a caller asks for another.
    var colour: Color = Tokens.accent

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(AppFont.button)
            .foregroundStyle(colour)
            .padding(.vertical, 10)
            .padding(.horizontal, 18)
            .background(
                RoundedRectangle(cornerRadius: Tokens.radiusButton)
                    .strokeBorder(colour, lineWidth: 1.5)
            )
            .opacity(configuration.isPressed ? 0.6 : 1)
    }
}

/// A screen's title row: a 30 bold title, with an optional count badge at the right that is
/// hidden at zero.
struct TitleRow: View {
    let title: String
    var count: Int = 0

    var body: some View {
        HStack {
            Text(title).font(AppFont.screenTitle).foregroundStyle(Tokens.ink)
            Spacer()
            if count > 0 {
                Text("\(count)")
                    .font(AppFont.screenTitle)
                    .foregroundStyle(Tokens.onAccent)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 2)
                    .background(Capsule().fill(Tokens.accent))
            }
        }
    }
}

/// A card with 18 pt padding, left aligned, with no text style of its own.
struct PlainCard<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 8) { content }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Tokens.spaceGap)
            .block()
    }
}
