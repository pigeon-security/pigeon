import SwiftUI

private enum CostRange: String, CaseIterable, Identifiable {
    case today
    case week = "7d"
    case all

    var id: String { rawValue }
    var title: String {
        switch self {
        case .today: return "Today"
        case .week: return "7 days"
        case .all: return "All"
        }
    }
}

// Cost's palette is local to this screen, not part of the generated app tokens.
private enum CostPalette {
    static let background = color(0x14121A)
    static let card = color(0x201E26)
    static let border = color(0x2A2830)
    static let text = color(0xF4EEE3)
    static let muted = color(0xA9A39A)
    static let accent = color(0xE07AA6)
    static let warning = color(0xF0A17E)
    static let success = color(0x7FCF9A)
    static let urgent = color(0xFF8A8A)

    static func color(_ hex: UInt32) -> Color {
        Color(red: Double(hex >> 16) / 255, green: Double((hex >> 8) & 255) / 255,
              blue: Double(hex & 255) / 255)
    }
}

private enum CostFamily {
    case byok, unified, cloudflare

    func color(at index: Int) -> Color {
        // Space neighbouring entries apart in hue and alternate their lightness.
        let step = (Double(index) * 0.618_033_988_75).truncatingRemainder(dividingBy: 1)
        let hue: Double
        switch self {
        case .byok: hue = 0.05 + 0.05 * step
        case .unified: hue = 0.15 + 0.23 * step
        case .cloudflare: hue = 0.55 + 0.10 * step
        }
        return Color(hue: hue, saturation: index.isMultiple(of: 2) ? 0.48 : 0.70,
                     brightness: index.isMultiple(of: 2) ? 0.96 : 0.80)
    }
}

private struct CostSeries {
    let name: String
    let total: Double?
    let byDay: [String: Double]?
    let color: Color

    init?(row: CostObject, color: Color) {
        guard let name = row["name"] as? String, !name.isEmpty else { return nil }
        self.name = name
        self.color = color
        total = costNumber(row["total"])
        byDay = (row["by_day"] as? CostObject)?.compactMapValues(costNumber)
    }
}

private func dayAmount(_ amounts: [String: Double]?, day: String) -> Double? {
    amounts.map { $0[day, default: 0] }
}

private struct CostGateway: Identifiable {
    let id: String
    let label: String?
    let total: Double?
    let byDay: [String: Double]?
    let models: [CostSeries]?
    let isAnthropicBYOK: Bool
}

private struct CostGrant {
    let name: String
    let amount: Double?
    let used: Double?
    let remaining: Double?
    let expiry: Date?

    init?(row: CostObject) {
        guard let name = row["name"] as? String, !name.isEmpty else { return nil }
        self.name = name
        amount = costNumber(row["amount"])
        used = costNumber(row["used"])
        remaining = costNumber(row["remaining"])
        expiry = (row["expiry"] as? String).flatMap(CostText.date)
    }
}

private typealias CostObject = [String: Any]

private func costNumber(_ value: Any?) -> Double? {
    guard let value = (value as? NSNumber)?.doubleValue, value.isFinite else { return nil }
    return value
}

private enum CostAvailability: Equatable {
    case available, empty, unavailable

    init(_ source: CostObject?, unavailable: [CostObject], name: String) {
        if source == nil || source?["status"] as? String == "unavailable"
            || unavailable.contains(where: { ($0["source"] as? String)?.lowercased() == name }) {
            self = .unavailable
        } else if source?["status"] as? String == "empty" {
            self = .empty
        } else {
            self = .available
        }
    }
}

private struct CostData {
    let days: [String]
    let gross: Double?
    let net: Double?
    let cloudflareTotal: Double?
    let cloudflareState: CostAvailability
    let gatewayState: CostAvailability
    let products: [CostSeries]?
    let gateways: [CostGateway]?
    let grants: [CostGrant]?

    init(json: Any) {
        let root = json as? CostObject ?? [:]
        days = Set(root["days"] as? [String] ?? []).sorted()
        gross = costNumber(root["total"])
        net = costNumber(root["net_total"])
        let unavailable = root["unavailable"] as? [CostObject] ?? []
        let cloudflare = root["cloudflare"] as? CostObject
        let gateway = root["ai_gateway"] as? CostObject
        cloudflareState = CostAvailability(cloudflare, unavailable: unavailable, name: "billable usage")
        gatewayState = CostAvailability(gateway, unavailable: unavailable, name: "ai gateway")
        cloudflareTotal = costNumber(cloudflare?["total"])
        products = (cloudflare?["lines"] as? [CostObject])?.enumerated().compactMap { index, row in
            CostSeries(row: row, color: CostFamily.cloudflare.color(at: index))
        }
        var byokIndex = 0
        var unifiedIndex = 0
        gateways = (gateway?["by_gateway"] as? [CostObject])?.compactMap { row in
            guard let id = row["id"] as? String, let billing = row["billing"] as? String,
                  billing == "byok" || billing == "unified" else { return nil }
            let byok = billing == "byok"
            let models = (row["by_model"] as? [CostObject])?.compactMap { model -> CostSeries? in
                let color: Color
                if byok {
                    color = CostFamily.byok.color(at: byokIndex)
                    byokIndex += 1
                } else {
                    color = CostFamily.unified.color(at: unifiedIndex)
                    unifiedIndex += 1
                }
                return CostSeries(row: model, color: color)
            }
            return CostGateway(id: id, label: row["label"] as? String,
                               total: costNumber(row["total"]),
                               byDay: (row["by_day"] as? CostObject)?.compactMapValues(costNumber),
                               models: models,
                               isAnthropicBYOK: byok && row["provider"] as? String == "anthropic")
        }
        if let rows = (root["credits"] as? CostObject)?["grants"] as? [CostObject] {
            let decoded = rows.compactMap(CostGrant.init(row:))
            grants = decoded.count == rows.count ? decoded.sorted {
                ($0.expiry ?? .distantFuture, $0.name) < ($1.expiry ?? .distantFuture, $1.name)
            } : nil
        } else {
            grants = nil
        }
    }

    func cloudflareAmount(on day: String?) -> Double? {
        guard let day else { return cloudflareTotal }
        guard let products, !products.isEmpty else { return nil }
        let amounts = products.compactMap { dayAmount($0.byDay, day: day) }
        return amounts.count == products.count ? amounts.reduce(0, +) : nil
    }

    var chartSeries: [CostSeries] {
        let cloudflare = cloudflareState == .unavailable ? [] : products ?? []
        let ai = gatewayState == .unavailable ? [] : (gateways ?? []).flatMap { $0.models ?? [] }
        return ai + cloudflare
    }

    var hasMissingDailyDetail: Bool {
        cloudflareState == .unavailable || gatewayState == .unavailable
            || (cloudflareState == .available && products == nil)
            || (gatewayState == .available && ((gateways ?? []).isEmpty
                || (gateways ?? []).contains { $0.models == nil }))
            || chartSeries.contains { $0.byDay == nil }
    }
}

private enum CostText {
    static let calendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0) ?? .gmt
        return calendar
    }()

    private static func formatter(_ pattern: String) -> DateFormatter {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.calendar = calendar
        formatter.timeZone = calendar.timeZone
        formatter.dateFormat = pattern
        return formatter
    }

    private static let iso = formatter("yyyy-MM-dd")
    private static let shortDate = formatter("d MMM")
    private static let datedYear = formatter("d MMM yyyy")
    private static let dayNumber = formatter("d")

    static func date(_ text: String) -> Date? {
        guard let date = iso.date(from: text), iso.string(from: date) == text else { return nil }
        return date
    }

    static func day(_ date: Date) -> String { shortDate.string(from: date) }

    static func period(_ days: [String]) -> String? {
        guard let first = days.first.flatMap(date), let last = days.last.flatMap(date) else { return nil }
        if first == last { return day(first) }
        if calendar.isDate(first, equalTo: last, toGranularity: .month) {
            return "\(dayNumber.string(from: first))–\(day(last))"
        }
        let format = calendar.isDate(first, equalTo: last, toGranularity: .year) ? shortDate : datedYear
        return "\(format.string(from: first))–\(format.string(from: last))"
    }

    static func money(_ amount: Double) -> String {
        let rounded = (amount * 100).rounded() / 100
        return String(format: "%@$%.2f", rounded < 0 ? "−" : "", abs(rounded))
    }
}

/// Cost by day and billing source, using the selected range of `cost_daily`.
struct CostScreen: View {
    let session: Session

    @Environment(\.displayScale) private var displayScale
    @AppStorage("costPeriod") private var range = CostRange.week
    @State private var selectedDay: String?
    @State private var loadedRange: CostRange?
    @State private var data: CostData?
    @State private var failed = false
    @State private var request = UUID()

    private var pixel: CGFloat { 1 / displayScale }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                header
                if loadedRange == range, let data {
                    hero(data)
                    daily(data)
                    HStack {
                        Text("By billing").font(.headline).accessibilityAddTraits(.isHeader)
                        if let day = selectedDay {
                            Text(CostText.date(day).map(CostText.day) ?? day).font(.subheadline)
                            Spacer()
                            Button("Show period") { selectedDay = nil }.tint(CostPalette.accent)
                        }
                    }
                    billing(data)
                } else if loadedRange == range && failed {
                    card { Text("Cost unavailable. Pull to refresh.").foregroundStyle(CostPalette.warning) }
                } else {
                    ProgressView("Loading cost…").tint(CostPalette.accent)
                }
            }
            .padding(.horizontal, Metrics.side)
            .padding(.top, 4)
            .padding(.bottom, 80)
        }
        .foregroundStyle(CostPalette.text)
        .background(CostPalette.background.ignoresSafeArea())
        .toolbar(.hidden, for: .navigationBar)
        .refreshable { await load() }
        .task(id: range) { await load() }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Text("Cost").font(AppFont.largeTitle).accessibilityAddTraits(.isHeader)
                .lineLimit(1).minimumScaleFactor(0.7)
            Spacer(minLength: 0)
            HStack(spacing: 2) {
                ForEach(CostRange.allCases) { choice in
                    Button {
                        if range != choice { selectedDay = nil }
                        range = choice
                    } label: {
                        Text(choice.title).font(.subheadline.weight(.semibold))
                            .lineLimit(1).minimumScaleFactor(0.7)
                            .padding(.horizontal, 8).frame(minHeight: 44)
                            .foregroundStyle(range == choice ? CostPalette.accent : CostPalette.muted)
                            .background(range == choice ? CostPalette.background : .clear,
                                        in: RoundedRectangle(cornerRadius: 7))
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(range == choice ? [.isSelected] : [])
                }
            }
            .padding(2)
            .background(CostPalette.card, in: RoundedRectangle(cornerRadius: 9))
            .overlay(RoundedRectangle(cornerRadius: 9).strokeBorder(CostPalette.border, lineWidth: pixel))
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Cost period")
        }
    }

    private func load() async {
        let asked = range
        let token = UUID()
        request = token
        do {
            let json = try await session.client.call("cost_daily", ["range": asked.rawValue])
            guard !Task.isCancelled, asked == range, request == token else { return }
            let result = CostData(json: json)
            if let day = selectedDay, !result.days.contains(day) { selectedDay = nil }
            data = result
            failed = false
            loadedRange = asked
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch is CancellationError {
        } catch let error as URLError where error.code == .cancelled {
        } catch {
            guard !Task.isCancelled, asked == range, request == token else { return }
            data = nil
            failed = true
            loadedRange = asked
        }
    }

    private func hero(_ data: CostData) -> some View {
        card {
            HStack(alignment: .firstTextBaseline) {
                Text("Net cost").font(.subheadline.weight(.semibold))
                Spacer()
                if let period = CostText.period(data.days) {
                    Text(period).font(.subheadline).foregroundStyle(CostPalette.muted)
                }
            }
            if let net = data.net {
                Text(CostText.money(net)).font(.largeTitle.bold()).monospacedDigit()
            } else {
                Text("Net cost unavailable").font(.headline).foregroundStyle(CostPalette.muted)
            }
            if let gross = data.gross, let net = data.net {
                Text("\(CostText.money(gross)) gross · \(CostText.money(gross - net)) promotional credits")
                    .font(.subheadline).foregroundStyle(CostPalette.muted)
            } else if let gross = data.gross {
                Text("\(CostText.money(gross)) gross").font(.subheadline).foregroundStyle(CostPalette.muted)
            }
            if data.cloudflareState == .unavailable || data.gatewayState == .unavailable {
                Text("Totals exclude unavailable billing data.").font(.caption)
                    .foregroundStyle(CostPalette.warning)
            }
        }
    }

    @ViewBuilder private func billing(_ data: CostData) -> some View {
        let promoGateway = data.gateways?.first(where: \.isAnthropicBYOK)?.id
        if let gateways = data.gateways, !gateways.isEmpty {
            ForEach(gateways) { gateway in
                card {
                    billingHeader("Cloudflare AI Gateway", subtitle: gateway.label,
                                  total: selectedDay == nil ? gateway.total : selectedDay.flatMap { dayAmount(gateway.byDay, day: $0) },
                                  state: data.gatewayState)
                    if data.gatewayState != .unavailable {
                        seriesRows("By model", gateway.models,
                                   empty: gateway.total == 0 ? "No spend in this period" : "Model detail unavailable")
                    }
                    if gateway.id == promoGateway { promotionalCredits(data.grants) }
                }
            }
        } else {
            card {
                billingHeader("Cloudflare AI Gateway", subtitle: nil, total: nil,
                              state: data.gatewayState == .empty ? .empty : .unavailable)
            }
        }
        card {
            billingHeader("Cloudflare", subtitle: "Workers, Containers, storage",
                          total: data.cloudflareAmount(on: selectedDay), state: data.cloudflareState)
            if data.cloudflareState != .unavailable {
                seriesRows("By product", data.products,
                           empty: data.cloudflareState == .empty ? "No spend in this period" : "Product detail unavailable")
            }
        }
    }

    private func billingHeader(_ title: String, subtitle: String?, total: Double?,
                               state: CostAvailability) -> some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(.headline).accessibilityAddTraits(.isHeader)
                if let subtitle { Text(subtitle).font(.subheadline).foregroundStyle(CostPalette.muted) }
            }
            Spacer(minLength: 0)
            if state == .unavailable {
                Text("Unavailable").foregroundStyle(CostPalette.warning)
            } else if let total {
                money(total)
            } else {
                Text(state == .empty ? "No spend" : "Unavailable").foregroundStyle(CostPalette.muted)
            }
        }
    }

    private func seriesRows(_ title: String, _ series: [CostSeries]?, empty: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Rectangle().fill(CostPalette.border).frame(height: pixel)
            Text(title).font(.caption.weight(.semibold)).foregroundStyle(CostPalette.muted)
            if let series, !series.isEmpty {
                ForEach(Array(series.enumerated()), id: \.offset) { _, item in
                    HStack(spacing: 8) {
                        RoundedRectangle(cornerRadius: 2).fill(item.color).frame(width: 10, height: 10)
                            .accessibilityHidden(true)
                        Text(item.name)
                        Spacer(minLength: 4)
                        money(selectedDay == nil ? item.total : selectedDay.flatMap { dayAmount(item.byDay, day: $0) })
                    }
                    .font(.subheadline)
                    .accessibilityElement(children: .combine)
                }
            } else {
                Text(series == nil ? "Unavailable" : empty).font(.subheadline)
                    .foregroundStyle(CostPalette.muted)
            }
        }
    }

    private func promotionalCredits(_ grants: [CostGrant]?) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Promotional credits").font(.subheadline.bold()).accessibilityAddTraits(.isHeader)
            if let grants, !grants.isEmpty {
                let amounts = grants.compactMap(\.amount)
                let balances = grants.compactMap(\.remaining)
                if amounts.count == grants.count, balances.count == grants.count {
                    Text("\(CostText.money(balances.reduce(0, +))) left of \(CostText.money(amounts.reduce(0, +)))")
                        .font(.subheadline.weight(.semibold)).monospacedDigit().foregroundStyle(CostPalette.success)
                } else {
                    Text("Balance unavailable").font(.subheadline).foregroundStyle(CostPalette.muted)
                }
                ForEach(Array(grants.enumerated()), id: \.offset) { _, grant in
                    Rectangle().fill(CostPalette.border).frame(height: pixel)
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            Text(grant.name)
                            Spacer(minLength: 4)
                            money(grant.amount)
                        }
                        if let used = grant.used {
                            Text("\(CostText.money(used)) used").monospacedDigit()
                                .foregroundStyle(CostPalette.muted)
                        }
                        if let remaining = grant.remaining {
                            Text("\(CostText.money(remaining)) left").monospacedDigit()
                                .foregroundStyle(CostPalette.success)
                        } else {
                            Text("Balance unavailable").foregroundStyle(CostPalette.muted)
                        }
                        expiry(grant.expiry)
                    }
                    .font(.subheadline)
                    .accessibilityElement(children: .combine)
                }
            } else {
                Text(grants == nil ? "Unavailable" : "Unconfigured")
                    .font(.subheadline).foregroundStyle(CostPalette.muted)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(CostPalette.border, lineWidth: pixel))
    }

    private func expiry(_ date: Date?) -> Text {
        guard let date else { return Text("Expiry unavailable").foregroundStyle(CostPalette.muted) }
        let calendar = CostText.calendar
        let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: .now), to: date).day ?? 0
        let day = CostText.day(date)
        if days < 0 { return Text("Expired \(day)").bold().foregroundStyle(CostPalette.urgent) }
        if days <= 7 {
            let when = days == 0 ? "today" : (days == 1 ? "in 1 day" : "in \(days) days")
            return Text("Expires \(day) · \(when)").bold()
                .foregroundStyle(days <= 3 ? CostPalette.urgent : CostPalette.warning)
        }
        return Text("Expires \(day)").foregroundStyle(CostPalette.muted)
    }

    private func money(_ amount: Double?) -> some View {
        Text(amount.map(CostText.money) ?? "Unavailable").monospacedDigit()
            .foregroundStyle(amount == nil ? CostPalette.muted : CostPalette.text)
            .fixedSize(horizontal: true, vertical: false)
    }

    private func card<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 10, content: content)
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(CostPalette.card, in: RoundedRectangle(cornerRadius: 12))
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(CostPalette.border, lineWidth: pixel))
    }

    private func daily(_ data: CostData) -> some View {
        let series = data.chartSeries
        let bars = data.days.map { CostBar(day: $0, series: series) }
        let positive = bars.map(\.positive).max() ?? 0
        let negative = bars.map(\.negative).max() ?? 0
        return card {
            Text("Daily spend").font(.headline).accessibilityAddTraits(.isHeader)
            if positive + negative > 0 {
                Text("Tap a day to filter By billing.").font(.caption).foregroundStyle(CostPalette.muted)
                chart(bars, positive: positive, negative: negative)
                if data.hasMissingDailyDetail {
                    Text("Some daily detail is unavailable.").font(.caption).foregroundStyle(CostPalette.muted)
                }
            } else {
                Text(data.hasMissingDailyDetail || bars.isEmpty ? "Daily detail unavailable" : "No daily spend in this period")
                    .font(.subheadline).foregroundStyle(CostPalette.muted)
            }
        }
    }

    private func chart(_ bars: [CostBar], positive: Double, negative: Double) -> some View {
        let plotHeight: CGFloat = 104
        let barWidth: CGFloat = 16
        let above = CGFloat(positive / (positive + negative)) * plotHeight
        let below = plotHeight - above
        return GeometryReader { area in
            ScrollView(.horizontal) {
                HStack(spacing: pixel) {
                    ForEach(Array(bars.enumerated()), id: \.offset) { index, bar in
                        Button {
                            selectedDay = selectedDay == bar.day ? nil : bar.day
                        } label: {
                            VStack(spacing: 0) {
                                stack(bar.charges.reversed(), sum: bar.positive, maximum: positive, height: above)
                                    .frame(height: above, alignment: .bottom)
                                Rectangle().fill(CostPalette.border).frame(height: pixel)
                                stack(bar.adjustments, sum: bar.negative, maximum: negative, height: below)
                                    .frame(height: below, alignment: .top)
                                if range != .all || bars.count <= 7 || index.isMultiple(of: 7) {
                                    Text(bar.axis).font(.caption2).foregroundStyle(CostPalette.muted)
                                        .lineLimit(1).minimumScaleFactor(0.7).padding(.top, 4)
                                } else {
                                    Color.clear.frame(height: 0)
                                }
                            }
                            .frame(minWidth: barWidth, maxWidth: range == .today ? 40 : .infinity,
                                   alignment: .top)
                            .frame(height: plotHeight + 22, alignment: .top)
                            .background(selectedDay == bar.day ? CostPalette.accent.opacity(0.19) : Color.clear)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(selectedDay == bar.day ? .isSelected : [])
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel(bar.label)
                    }
                }
                .frame(width: max(area.size.width,
                                  CGFloat(bars.count) * (barWidth + pixel) - pixel))
            }
            .scrollBounceBehavior(.basedOnSize)
        }
        .frame(height: plotHeight + 30)
    }

    private func stack<S: Collection>(_ parts: S, sum: Double, maximum: Double,
                                      height: CGFloat) -> some View where S.Element == CostBar.Part {
        let totalHeight = maximum > 0 ? CGFloat(sum / maximum) * height : 0
        let gaps = CGFloat(max(parts.count - 1, 0))
        let gap = min(pixel, totalHeight / max(gaps * 2, 1))
        let fill = max(0, totalHeight - gap * gaps)
        return VStack(spacing: gap) {
            ForEach(Array(parts)) { part in
                Rectangle().fill(part.color).frame(height: fill * CGFloat(abs(part.amount) / sum))
            }
        }
        .frame(maxWidth: .infinity)
    }
}

private struct CostBar {
    struct Part: Identifiable {
        let id: Int
        let amount: Double
        let color: Color
    }

    let day: String
    let charges: [Part]
    let adjustments: [Part]
    let hasDailyDetail: Bool
    let positive: Double
    let negative: Double

    init(day: String, series: [CostSeries]) {
        self.day = day
        let parts = series.enumerated().compactMap { index, item -> Part? in
            guard let amount = dayAmount(item.byDay, day: day) else { return nil }
            return Part(id: index, amount: amount, color: item.color)
        }
        hasDailyDetail = !parts.isEmpty
        charges = parts.filter { $0.amount > 0 }
        adjustments = parts.filter { $0.amount < 0 }
        positive = charges.reduce(0) { $0 + $1.amount }
        negative = adjustments.reduce(0) { $0 - $1.amount }
    }

    var axis: String { String(Int(day.suffix(2)) ?? 0) }
    var label: String {
        let date = CostText.date(day).map(CostText.day) ?? day
        return "\(date), \(hasDailyDetail ? CostText.money(positive - negative) : "daily detail unavailable")"
    }
}
