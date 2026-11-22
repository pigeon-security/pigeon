import SwiftUI

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
