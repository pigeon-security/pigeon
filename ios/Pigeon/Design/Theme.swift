import SwiftUI

/// The console's design tokens (`src/capabilities/src/console/web/shell.css`), one asset catalog
/// Color set each under `Assets.xcassets/Theme`, with Any and Dark appearances. Use these, not
/// `Tokens` (the older web design system, kept only while its drift check exists).
enum Theme {
    static let bg = Color("bg")
    static let sunk = Color("sunk")
    static let surface = Color("surface")
    static let raised = Color("raised")
    static let hover = Color("hover")
    static let line = Color("line")
    static let lineStrong = Color("line-strong")
    static let ink = Color("ink")
    static let ink2 = Color("ink-2")
    static let muted = Color("muted")
    static let accent = Color("accent")
    static let accentInk = Color("accent-ink")
    static let accentSoft = Color("accent-soft")
    static let accentLine = Color("accent-line")
    static let ok = Color("ok")
    static let okSoft = Color("ok-soft")
    static let warn = Color("warn")
    static let warnSoft = Color("warn-soft")
    static let bad = Color("bad")
    static let badSoft = Color("bad-soft")
    static let info = Color("info")
    static let infoSoft = Color("info-soft")
    static let markGround = Color("mark-ground")
    static let markEdge = Color("mark-edge")

    /// The type scale: Dynamic Type styles only (body 15, footnote 13, caption 11 at the default
    /// size), with SF Mono for ids, hashes, hosts and amounts that line up.
    enum Fonts {
        /// Row titles and body text (15 pt).
        static let body = Font.subheadline
        /// Row subtitles, meta lines and section notes (13 pt).
        static let meta = Font.footnote
        /// Status pills and small labels (11 pt).
        static let caption = Font.caption2.weight(.medium)
        /// An id, hash, host or path inside a row (13 pt, monospaced).
        static let id = Font.system(.footnote, design: .monospaced)
        /// An id as a row's main text (15 pt, monospaced).
        static let idBody = Font.system(.subheadline, design: .monospaced)
        /// A large figure, such as a cost total.
        static let figure = Font.title2.weight(.bold).monospacedDigit()
    }
}

extension View {
    /// Sets an id, hash, host or path in SF Mono, kept on one line and truncated in the middle.
    func idStyle(_ font: Font = Theme.Fonts.id) -> some View {
        self.font(font).lineLimit(1).truncationMode(.middle)
    }
}

/// The person's appearance choice, kept in `UserDefaults` under `theme`.
enum AppearanceChoice: String, CaseIterable, Identifiable {
    case system, light, dark

    var id: String { rawValue }

    var title: String {
        switch self {
        case .system: return "System"
        case .light: return "Light"
        case .dark: return "Dark"
        }
    }

    /// The scheme for `.preferredColorScheme`: nil follows the system.
    var scheme: ColorScheme? {
        switch self {
        case .system: return nil
        case .light: return .light
        case .dark: return .dark
        }
    }
}
