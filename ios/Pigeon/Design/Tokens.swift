// Generated from src/core/src/design/tokens.css by pigeon_core::design::swift. Do not edit:
// change tokens.css, then regenerate (see ios/README.md). The smoke suite fails on drift.
import SwiftUI
import UIKit

/// A type style from the web design system.
struct TypeToken { let weight: Int; let size: CGFloat; let lineHeight: CGFloat; let mono: Bool }

/// pigeon's design tokens, light and dark, as the web design system defines them.
enum Tokens {
    static let background = dynamic(light: (0xF4, 0xEE, 0xE3, 1.0), dark: (0x14, 0x12, 0x1A, 1.0))
    static let dot = dynamic(light: (0xCF, 0xC4, 0xB2, 1.0), dark: (0x2F, 0x2B, 0x38, 1.0))
    static let card = dynamic(light: (0xFF, 0xFD, 0xF8, 1.0), dark: (0x21, 0x1E, 0x26, 1.0))
    static let ink = dynamic(light: (0x1B, 0x1A, 0x17, 1.0), dark: (0xF2, 0xEC, 0xDF, 1.0))
    static let ink2 = dynamic(light: (0x45, 0x40, 0x3A, 1.0), dark: (0xD3, 0xCC, 0xBE, 1.0))
    static let muted = dynamic(light: (0x6A, 0x62, 0x59, 1.0), dark: (0xA7, 0x9F, 0x93, 1.0))
    static let chip = dynamic(light: (0xEA, 0xE1, 0xD2, 1.0), dark: (0x2C, 0x28, 0x33, 1.0))
    static let line = dynamic(light: (0xCB, 0xBF, 0xAE, 1.0), dark: (0x4A, 0x45, 0x53, 1.0))
    static let shadow = dynamic(light: (0x1B, 0x1A, 0x17, 1.0), dark: (0x00, 0x00, 0x00, 1.0))
    static let accent = dynamic(light: (0x7A, 0x2E, 0x4F, 1.0), dark: (0xE0, 0x7A, 0xA6, 1.0))
    static let onAccent = dynamic(light: (0xFF, 0xFD, 0xF8, 1.0), dark: (0x14, 0x12, 0x1A, 1.0))
    static let accentSoft = dynamic(light: (0xF2, 0xD3, 0xDE, 1.0), dark: (0x3D, 0x24, 0x32, 1.0))
    static let good = dynamic(light: (0x2F, 0x6B, 0x4F, 1.0), dark: (0x5F, 0xBF, 0x8E, 1.0))
    static let warn = dynamic(light: (0xB0, 0x4F, 0x1A, 1.0), dark: (0xD0, 0x6A, 0x35, 1.0))
    static let region = dynamic(light: (0, 0, 0, 0.02), dark: (255, 255, 255, 0.025))
    static let cloudflare = dynamic(light: (0xA9, 0x52, 0x0F, 1.0), dark: (0xF0, 0x91, 0x3A, 1.0))
    static let cloudflareZone = dynamic(light: (169, 82, 15, 0.05), dark: (240, 145, 58, 0.05))
    static let github = dynamic(light: (0x6F, 0x42, 0xC1, 1.0), dark: (0xB3, 0x92, 0xF0, 1.0))
    static let kindWorker = dynamic(light: (0x7A, 0x2E, 0x4F, 1.0), dark: (0xE0, 0x7A, 0xA6, 1.0))
    static let kindAccess = dynamic(light: (0x2C, 0x5E, 0x9E, 1.0), dark: (0x78, 0xA9, 0xEE, 1.0))
    static let kindStore = dynamic(light: (0x2F, 0x6B, 0x4F, 1.0), dark: (0x5F, 0xBF, 0x8E, 1.0))
    static let kindApi = dynamic(light: (0xA9, 0x52, 0x0F, 1.0), dark: (0xF0, 0x91, 0x3A, 1.0))
    static let kindClient = dynamic(light: (0x6A, 0x62, 0x59, 1.0), dark: (0xA7, 0x9F, 0x93, 1.0))
    static let radiusCard: CGFloat = 14
    static let radiusButton: CGFloat = 12
    static let radiusPill: CGFloat = 999
    static let radiusRegion: CGFloat = 18
    static let shadowOffset: CGFloat = 3
    static let spacePage: CGFloat = 24
    static let spaceGap: CGFloat = 18
    static let gridStep: CGFloat = 16
    static let dotSize: CGFloat = 1.1
    static let typeDisplay = TypeToken(weight: 700, size: 32, lineHeight: 1.15, mono: false)
    static let typeTitle = TypeToken(weight: 700, size: 22, lineHeight: 1.25, mono: false)
    static let typeHeading = TypeToken(weight: 700, size: 17, lineHeight: 1.3, mono: false)
    static let typeBody = TypeToken(weight: 400, size: 16, lineHeight: 1.5, mono: false)
    static let typeSmall = TypeToken(weight: 400, size: 14, lineHeight: 1.45, mono: false)
    static let typeMono = TypeToken(weight: 400, size: 13, lineHeight: 1.45, mono: true)
    static let typeEyebrow = TypeToken(weight: 600, size: 12, lineHeight: 1.3, mono: true)
    static let typeButton = TypeToken(weight: 600, size: 16, lineHeight: 1.2, mono: false)

    /// A colour that follows the light or dark appearance.
    private static func dynamic(light: (Int, Int, Int, Double), dark: (Int, Int, Int, Double)) -> Color {
        Color(UIColor { traits in
            let c = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(
                red: CGFloat(c.0) / 255, green: CGFloat(c.1) / 255,
                blue: CGFloat(c.2) / 255, alpha: CGFloat(c.3))
        })
    }
}
