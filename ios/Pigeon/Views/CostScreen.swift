import SwiftUI

// The Cost tab, from the confirmed mockup (docs/plans/ios-app-next.md). A title row, then a 2×2
// grid of equal squares: the total, a details box, a Cloudflare chart and an AI Gateway chart.
// Everything here is private except the screen itself.

/// The time ranges the person can choose; the raw value is what `cost_daily` takes.
private enum CostRange: String, CaseIterable, Identifiable {
    case today
    case week = "7d"
    case all

    var id: String { rawValue }

    /// The text on the segmented control.
    var title: String {
        switch self {
        case .today: return "Today"
        case .week: return "7 days"
        case .all: return "All"
        }
    }
}

/// Which chart a bar belongs to.
private enum CostSource {
    case cloudflare
    case aiGateway

    /// The source's name as shown.
    var title: String { self == .cloudflare ? "Cloudflare" : "AI Gateway" }
}

/// One line, model or role: a name, a total and its amount for each day.
private struct CostSeries {
    let name: String
    let total: Double
    let byDay: [String: Double]
    let perPeriod: Bool
}

/// The parts of a `cost_daily` answer the screen uses. Every field is read as optional.
private struct CostData {
    var range = ""
    var days: [String] = []
    var total = 0.0
    var cloudflareTotal = 0.0
    var lines: [CostSeries] = []
    var aiTotal = 0.0
    var models: [CostSeries] = []
    var roles: [CostSeries] = []
    var unavailable = false
    var storedFrom: String?

    /// Reads a decoded JSON answer. Missing or odd fields become empty or zero.
    init(json: Any) {
        let root = json as? [String: Any] ?? [:]
        range = root["range"] as? String ?? ""
        days = (root["days"] as? [Any] ?? []).compactMap { $0 as? String }
        total = Self.number(root["total"])
        let cloudflare = root["cloudflare"] as? [String: Any] ?? [:]
        cloudflareTotal = Self.number(cloudflare["total"])
        lines = Self.series(
            cloudflare["lines"], order: ["CI Containers", "Workers", "D1", "R2", "Other"])
        let gateway = root["ai_gateway"] as? [String: Any] ?? [:]
        aiTotal = Self.number(gateway["total"])
        models = Self.series(gateway["by_model"], order: ["Opus", "Sonnet", "Haiku", "Other"])
        roles = Self.series(gateway["by_role"], order: ["planner", "executor", "other"])
        unavailable = !(root["unavailable"] as? [Any] ?? []).isEmpty
        storedFrom = root["stored_from"] as? String
    }

    /// A number that may arrive as an Int or a Double; anything else is 0.
    static func number(_ value: Any?) -> Double {
        (value as? NSNumber)?.doubleValue ?? 0
    }

    /// Reads a list of series and puts them in `order`; unknown names go last, in given order.
    private static func series(_ value: Any?, order: [String]) -> [CostSeries] {
        let rows = (value as? [Any] ?? []).compactMap { $0 as? [String: Any] }
        let list = rows.map { row -> CostSeries in
            var byDay: [String: Double] = [:]
            for (day, amount) in row["by_day"] as? [String: Any] ?? [:] {
                byDay[day] = number(amount)
            }
            return CostSeries(
                name: row["name"] as? String ?? "Other", total: number(row["total"]),
                byDay: byDay, perPeriod: (row["granularity"] as? String) == "period")
        }
        func rank(_ s: CostSeries) -> Int { order.firstIndex(of: s.name) ?? order.count }
        return list.enumerated()
            .sorted { (rank($0.element), $0.offset) < (rank($1.element), $1.offset) }
            .map { $0.element }
    }
}

/// One part of a stacked bar.
private struct CostPart {
    let name: String
    let value: Double
}

/// One bar: a day, a week, or (for today) one item.
private struct CostBar: Identifiable {
    let id: Int
    /// The label under the bar.
    let axis: String
    /// The first day of the bar, as YYYY-MM-DD.
    let date: String
    let isWeek: Bool
    let parts: [CostPart]

    /// The height of the whole stack.
    var total: Double { parts.reduce(0) { $0 + max($1.value, 0) } }
}

/// The selected bar: which chart and which bar in it.
private struct CostSelection: Equatable {
    let source: CostSource
    let bar: Int
}

/// Date and money text helpers.
private enum CostText {
    /// A UTC date from "YYYY-MM-DD".
    static func date(_ day: String) -> Date? {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = TimeZone(identifier: "UTC")
        f.dateFormat = "yyyy-MM-dd"
        return f.date(from: day)
    }

    /// A "YYYY-MM-DD" day in another format, in UTC; the input itself if it does not parse.
    static func day(_ day: String, _ format: String) -> String {
        guard let d = date(day) else { return day }
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = TimeZone(identifier: "UTC")
        f.dateFormat = format
        return f.string(from: d)
    }

    /// Money as "$12.34"; a positive amount under a cent is "<$0.01".
    static func money(_ v: Double) -> String {
        if v > 0 && v < 0.01 { return "<$0.01" }
        return String(format: "$%.2f", max(v, 0))
    }
}

/// Builds the bars of one chart for a range.
private func costBars(_ series: [CostSeries], days: [String], range: CostRange) -> [CostBar] {
    if range == .today {
        let day = days.first ?? ""
        return series.enumerated().map { i, s in
            CostBar(
                id: i, axis: "", date: day, isWeek: false,
                parts: [CostPart(name: s.name, value: s.byDay[day] ?? s.total)])
        }
    }
    if range == .week || days.count <= 31 {
        return days.enumerated().map { i, day in
            let initial = range == .week ? CostText.day(day, "EEEEE") : String(day.suffix(2))
            return CostBar(
                id: i, axis: initial, date: day, isWeek: false,
                parts: series.map { CostPart(name: $0.name, value: $0.byDay[day] ?? 0) })
        }
    }
    var bars: [CostBar] = []
    var start = 0
    while start < days.count {
        let week = Array(days[start..<min(start + 7, days.count)])
        let first = week.first ?? ""
        bars.append(
            CostBar(
                id: bars.count, axis: String(Int(first.suffix(2)) ?? 0), date: first,
                isWeek: true,
                parts: series.map { s in
                    CostPart(name: s.name, value: week.reduce(0) { $0 + (s.byDay[$1] ?? 0) })
                }))
        start += 7
    }
    return bars
}

/// The Cost tab: title and range control, then the 2×2 grid.
struct CostScreen: View {
    let session: Session

    @State private var range = CostRange.week
    @State private var data: CostData?
    @State private var error: String?
    @State private var selection: CostSelection?
    @State private var byRole = false
    @State private var side: CGFloat = 150

    private let gap = Tokens.spaceGap

    var body: some View {
        Screen(errors: error.map { [$0] } ?? [], refresh: { await load() }) {
            HStack(spacing: gap) {
                Text("Cost").font(AppFont.screenTitle).foregroundStyle(Tokens.ink)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Picker("Range", selection: $range) {
                    ForEach(CostRange.allCases) { Text($0.title).tag($0) }
                }
                .pickerStyle(.segmented)
                .frame(width: side)
            }
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width in
                side = max((width - gap) / 2, 80)
            }
            HStack(spacing: gap) {
                totalCard.frame(width: side, height: side)
                detailsCard.frame(width: side, height: side)
            }
            HStack(spacing: gap) {
                chartCard(.cloudflare).frame(width: side, height: side)
                chartCard(.aiGateway).frame(width: side, height: side)
            }
        }
        .task(id: range) { await load() }
        .onChange(of: range) { selection = nil }
        .onChange(of: byRole) {
            if selection?.source == .aiGateway { selection = nil }
        }
    }

    /// Loads the chosen range. On failure the error shows and the grid is empty.
    private func load() async {
        let asked = range
        do {
            let json = try await session.client.call("cost_daily", ["range": asked.rawValue])
            guard asked == range else { return }
            data = CostData(json: json)
            error = nil
        } catch PlatformError.signedOut {
            // The session has already signed out.
        } catch is CancellationError {
            // A newer load replaced this one.
        } catch let e as URLError where e.code == .cancelled {
            // A newer load replaced this one.
        } catch {
            guard asked == range else { return }
            data = nil
            self.error = "cost_daily failed: \(error.localizedDescription)"
        }
    }

    // MARK: Bars

    private var cloudflareBars: [CostBar] {
        costBars(data?.lines ?? [], days: data?.days ?? [], range: range)
    }

    private var gatewayBars: [CostBar] {
        costBars(
            (byRole ? data?.roles : data?.models) ?? [], days: data?.days ?? [], range: range)
    }

    private func bars(_ source: CostSource) -> [CostBar] {
        source == .cloudflare ? cloudflareBars : gatewayBars
    }

    /// The selected bar and its source, if the selection still points at a bar.
    private var selectedBar: (CostSource, CostBar)? {
        guard let selection else { return nil }
        guard let bar = bars(selection.source).first(where: { $0.id == selection.bar }) else {
            return nil
        }
        return (selection.source, bar)
    }

    /// Selects a bar, or clears the selection when it is already selected.
    private func tap(_ source: CostSource, _ bar: Int) {
        let next = CostSelection(source: source, bar: bar)
        selection = selection == next ? nil : next
    }

    // MARK: Cards

    /// The text under the total saying which range it covers.
    private var rangeText: String {
        switch range {
        case .today: return "Today"
        case .week: return "Last 7 days"
        case .all:
            guard let from = data?.storedFrom else { return "All time" }
            return "Since \(CostText.day(from, "d MMM yyyy"))"
        }
    }

    private var totalCard: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Total").font(AppFont.label).foregroundStyle(Tokens.muted)
            Text(CostText.money(data?.total ?? 0))
                .font(AppFont.screenTitle).foregroundStyle(Tokens.ink)
                .lineLimit(1).minimumScaleFactor(0.5)
            Text(rangeText).font(AppFont.meta).foregroundStyle(Tokens.muted)
                .lineLimit(2)
        }
        .padding(12)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .block()
    }

    private var detailsCard: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let (source, bar) = selectedBar {
                selectedDetails(source, bar)
            } else {
                overview
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .contentShape(Rectangle())
        .onTapGesture { selection = nil }
        .block()
    }

    /// The details with nothing selected: the total by source, most expensive first.
    @ViewBuilder
    private var overview: some View {
        let rows = [(CostSource.cloudflare, data?.cloudflareTotal ?? 0),
                    (CostSource.aiGateway, data?.aiTotal ?? 0)]
            .sorted { $0.1 > $1.1 }
        Text("Where it went").font(AppFont.label).foregroundStyle(Tokens.muted)
        ForEach(rows, id: \.0.title) { source, amount in
            HStack {
                Text(source.title).font(AppFont.meta).foregroundStyle(Tokens.ink)
                    .lineLimit(1).minimumScaleFactor(0.7)
                Spacer(minLength: 4)
                Text(CostText.money(amount)).font(AppFont.meta).foregroundStyle(Tokens.ink)
            }
        }
        if data?.unavailable == true {
            Text("Billing unavailable").font(AppFont.chart).foregroundStyle(Tokens.muted)
        }
        if data?.lines.contains(where: { $0.perPeriod }) == true {
            Text("Some Cloudflare lines are billed per period")
                .font(AppFont.chart).foregroundStyle(Tokens.muted)
        }
    }

    /// The details of one bar: source, date, total and up to four items.
    @ViewBuilder
    private func selectedDetails(_ source: CostSource, _ bar: CostBar) -> some View {
        let items = bar.parts.filter { $0.value > 0 }.sorted { $0.value > $1.value }
        let shown = Array(items.prefix(4))
        let rest = items.dropFirst(4)
        Text(source.title).font(AppFont.label).foregroundStyle(Tokens.muted)
        Text(dateText(bar)).font(AppFont.meta).foregroundStyle(Tokens.muted)
        Text(CostText.money(bar.total)).font(AppFont.sourceTotal).foregroundStyle(Tokens.ink)
            .lineLimit(1).minimumScaleFactor(0.5)
        if items.isEmpty {
            Text("No cost").font(AppFont.chart).foregroundStyle(Tokens.muted)
        }
        ForEach(Array(shown.enumerated()), id: \.offset) { _, part in
            HStack(spacing: 4) {
                RoundedRectangle(cornerRadius: 2).fill(ChartColour.of(part.name))
                    .frame(width: 8, height: 8)
                Text(part.name).font(AppFont.chart).foregroundStyle(Tokens.ink)
                    .lineLimit(1).minimumScaleFactor(0.7)
                Spacer(minLength: 2)
                Text(CostText.money(part.value)).font(AppFont.chart).foregroundStyle(Tokens.ink)
            }
        }
        if !rest.isEmpty {
            HStack {
                Text("+\(rest.count) more").font(AppFont.chart).foregroundStyle(Tokens.muted)
                Spacer(minLength: 2)
                Text(CostText.money(rest.reduce(0) { $0 + $1.value }))
                    .font(AppFont.chart).foregroundStyle(Tokens.muted)
            }
        }
    }

    /// A bar's date as "Mon 24 Nov", with "Week of" for a week bar.
    private func dateText(_ bar: CostBar) -> String {
        let day = CostText.day(bar.date, "EEE d MMM")
        return bar.isWeek ? "Week of \(day)" : day
    }

    private func chartCard(_ source: CostSource) -> some View {
        let isCloudflare = source == .cloudflare
        return VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 4) {
                Text(source.title).font(AppFont.label).foregroundStyle(Tokens.muted)
                    .lineLimit(1).minimumScaleFactor(0.7)
                if !isCloudflare {
                    Spacer(minLength: 2)
                    toggleButton("Model", active: !byRole) { byRole = false }
                    toggleButton("Role", active: byRole) { byRole = true }
                }
            }
            Text(CostText.money(isCloudflare ? data?.cloudflareTotal ?? 0 : data?.aiTotal ?? 0))
                .font(AppFont.sourceTotal).foregroundStyle(Tokens.ink)
                .lineLimit(1).minimumScaleFactor(0.5)
            chart(source).padding(.top, 4)
        }
        .padding(12)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .block()
    }

    /// One side of the Model / Role toggle.
    private func toggleButton(_ title: String, active: Bool, action: @escaping () -> Void)
        -> some View
    {
        Button(action: action) {
            Text(title).font(AppFont.chart)
                .foregroundStyle(active ? Tokens.ink : Tokens.muted)
                .underline(active)
        }
        .buttonStyle(.plain)
    }

    // MARK: Chart

    /// The stacked bars of one chart, scaled to its tallest bar.
    private func chart(_ source: CostSource) -> some View {
        let list = bars(source)
        let peak = list.map(\.total).max() ?? 0
        let valueHeight: CGFloat = 14
        let axisHeight: CGFloat = 14
        let step = max(Int((Double(list.count) / 6).rounded(.up)), 1)
        return GeometryReader { geo in
            let barArea = max(geo.size.height - valueHeight - axisHeight, 1)
            HStack(alignment: .bottom, spacing: list.count > 12 ? 1 : 3) {
                ForEach(list) { bar in
                    let faded = selection != nil && selection != CostSelection(
                        source: source, bar: bar.id)
                    VStack(spacing: 0) {
                        Spacer(minLength: 0)
                        if range == .today {
                            Text(CostText.money(bar.total)).font(AppFont.chart)
                                .foregroundStyle(Tokens.ink)
                                .lineLimit(1).minimumScaleFactor(0.5)
                                .frame(height: valueHeight)
                        }
                        stack(bar, peak: peak, height: barArea)
                            .opacity(faded ? 0.4 : 1)
                        Text(list.count <= 8 || bar.id % step == 0 ? bar.axis : "")
                            .font(AppFont.chart).foregroundStyle(Tokens.muted)
                            .lineLimit(1).fixedSize()
                            .frame(height: axisHeight)
                    }
                    .frame(maxWidth: .infinity)
                    .contentShape(Rectangle())
                    .onTapGesture { tap(source, bar.id) }
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(accessibility(source, bar))
                    .accessibilityAddTraits(.isButton)
                }
            }
        }
    }

    /// The VoiceOver label of a bar: date, source and amount.
    private func accessibility(_ source: CostSource, _ bar: CostBar) -> String {
        var name = source.title
        if range == .today, let item = bar.parts.first?.name { name += ", \(item)" }
        return "\(dateText(bar)), \(name), \(CostText.money(bar.total))"
    }

    /// One bar's stack, the first part at the bottom. A zero bar is a 1 pt tick.
    private func stack(_ bar: CostBar, peak: Double, height: CGFloat) -> some View {
        VStack(spacing: 0) {
            Spacer(minLength: 0)
            if bar.total <= 0 || peak <= 0 {
                Rectangle().fill(Tokens.muted).frame(height: 1)
            } else {
                ForEach(Array(bar.parts.enumerated().reversed()), id: \.offset) { _, part in
                    if part.value > 0 {
                        Rectangle().fill(ChartColour.of(part.name))
                            .frame(height: CGFloat(part.value / peak) * height)
                    }
                }
            }
        }
        .frame(maxWidth: 28, minHeight: height, maxHeight: height)
    }
}
