import Foundation
import SwiftUI

// Formatting and small drawings shared by the platform-wide screens (Sessions, Cost, Access,
// Instances, Platform), as the web console's views format them.

enum PlatformFormat {
    /// Dollars as $1,234.56, or to four places under a cent; "—" for no value.
    static func money(_ value: Double?) -> String {
        guard let value, value.isFinite else { return "—" }
        let fine = value != 0 && abs(value) < 0.01
        let f = NumberFormatter()
        f.locale = Locale(identifier: "en_US")
        f.numberStyle = .decimal
        f.minimumFractionDigits = 2
        f.maximumFractionDigits = fine ? 4 : 2
        let digits = f.string(from: NSNumber(value: abs(value))) ?? String(abs(value))
        return (value < 0 ? "-$" : "$") + digits
    }

    /// A length of time in ms as "Under 1m", "38m", "1h 12m" or "2d 3h".
    static func duration(_ ms: Double?) -> String {
        guard let ms, ms.isFinite else { return "—" }
        let m = Int(max(0, ms) / 60_000)
        if m >= 1440 { return "\(m / 1440)d \((m % 1440) / 60)h" }
        if m >= 60 { return "\(m / 60)h \(m % 60)m" }
        return m > 0 ? "\(m)m" : "Under 1m"
    }

    /// A time in epoch ms as its UTC day, 2026-11-26; "—" for none.
    static func day(_ ms: Double?) -> String {
        guard let ms, ms.isFinite, ms > 0 else { return "—" }
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = TimeZone(identifier: "UTC")
        f.dateFormat = "yyyy-MM-dd"
        return f.string(from: Date(timeIntervalSince1970: ms / 1000))
    }

    /// A snake_case word as a sentence-case one: "over_budget" → "Over budget".
    static func word(_ text: String?) -> String {
        guard let text, let first = text.first else { return "—" }
        return (String(first).uppercased() + text.dropFirst()).replacingOccurrences(of: "_", with: " ")
    }

    /// Who pays for a model route.
    static func pays(_ route: String?) -> String {
        switch route {
        case "byok": return "Own key"
        case "credits", "unified": return "Cloudflare credits"
        default: return "—"
        }
    }

    /// A UTC day such as 2026-11-25 as a local date at its midnight, for charts.
    static func date(ofDay day: String) -> Date? {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = .current
        f.dateFormat = "yyyy-MM-dd"
        return f.date(from: day)
    }

    /// A UTC day such as 2026-11-25 as "Wed, Nov 25".
    static func dayLabel(_ day: String) -> String {
        let parse = DateFormatter()
        parse.locale = Locale(identifier: "en_US_POSIX")
        parse.timeZone = TimeZone(identifier: "UTC")
        parse.dateFormat = "yyyy-MM-dd"
        guard let date = parse.date(from: day) else { return day }
        let show = DateFormatter()
        show.locale = Locale(identifier: "en_US")
        show.timeZone = TimeZone(identifier: "UTC")
        show.dateFormat = "EEE, MMM d"
        return show.string(from: date)
    }

    /// Whether `text` is a UTC day, yyyy-mm-dd.
    static func isDay(_ text: String) -> Bool {
        text.range(of: "^[0-9]{4}-[0-9]{2}-[0-9]{2}$", options: .regularExpression) != nil
    }

    /// "1 run", "3 runs".
    static func count(_ n: Int, _ one: String, _ many: String) -> String {
        "\(n) \(n == 1 ? one : many)"
    }
}

/// A budget meter: a short track filled to `fraction`, red from 80 %.
struct BudgetMeter: View {
    let fraction: Double
    var width: CGFloat = 52
    /// A fixed fill; by default accent, red from 80 %.
    var fill: Color?

    var body: some View {
        let f = fraction.isFinite ? min(max(fraction, 0), 1) : 0
        Capsule()
            .fill(Theme.line)
            .frame(width: width, height: 5)
            .overlay(alignment: .leading) {
                Capsule()
                    .fill(fill ?? (f >= 0.8 ? Theme.bad : Theme.accent))
                    .frame(width: width * CGFloat(f), height: 5)
            }
            .accessibilityHidden(true)
    }
}

/// A bar of coloured parts, each a fraction of the whole width: outcomes, a day's split.
struct ShareBar: View {
    struct Part {
        let fraction: Double
        let color: Color
    }

    let parts: [Part]
    var width: CGFloat = 90

    var body: some View {
        HStack(spacing: 0) {
            ForEach(Array(parts.enumerated()), id: \.offset) { _, part in
                Rectangle()
                    .fill(part.color)
                    .frame(width: width * CGFloat(part.fraction.isFinite ? min(max(part.fraction, 0), 1) : 0))
            }
            Spacer(minLength: 0)
        }
        .frame(width: width, height: 6)
        .background(Theme.line)
        .clipShape(Capsule())
        .accessibilityHidden(true)
    }
}

/// The first section of an area's list: its view picker and, for a view the instance is too
/// old for, the upgrade notice.
struct PlatformViewPicker: View {
    let context: AreaContext

    var body: some View {
        if context.area.views.count > 1 {
            Section {
                ViewPicker(views: context.area.views, selection: context.viewID)
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
            }
        }
        if let view = context.view, view.upgrade != nil {
            Section { UpgradeNotice(view: view) }
        }
    }
}

/// A form sheet's frame: a title, a line under it, the fields, a problem, and Review / Cancel.
/// Review hands over to the confirm sheet in the same sheet.
struct ReviewForm<Fields: View>: View {
    let title: String
    var subtitle: String?
    var problem: String?
    var reviewLabel = "Review"
    let review: () -> Void
    @ViewBuilder let fields: Fields
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.title3.weight(.bold)).foregroundStyle(Theme.ink)
                    if let subtitle {
                        Text(subtitle).font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                    }
                }
                fields
                if let problem {
                    Text(problem).font(Theme.Fonts.meta).foregroundStyle(Theme.bad)
                }
                VStack(spacing: 10) {
                    Button(reviewLabel, action: review)
                        .buttonStyle(SheetButtonStyle(.primary))
                    Button("Cancel") { dismiss() }
                        .buttonStyle(SheetButtonStyle(.quiet))
                }
                .padding(.top, 4)
            }
            .padding(.horizontal, 20)
            .padding(.top, 24)
            .padding(.bottom, 16)
        }
        .scrollBounceBehavior(.basedOnSize)
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }
}

/// A labelled field for a form sheet.
struct FormField<Control: View>: View {
    let label: String
    var hint: String?
    @ViewBuilder let control: Control

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(label)
                .font(Theme.Fonts.caption).textCase(.uppercase)
                .foregroundStyle(Theme.muted)
            control
                .font(Theme.Fonts.body)
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
                .background(Theme.raised, in: RoundedRectangle(cornerRadius: 10))
            if let hint {
                Text(hint).font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
            }
        }
    }
}
