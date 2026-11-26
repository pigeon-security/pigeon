import Charts
import SwiftUI

/// Cost (platform-wide, home only): spend over 7 days, today or 30 days (the area's views `7d`,
/// `today`, `all`), as the web console's `sessions.js` cost view: headline figures, a daily
/// chart, by day, AI Gateway by billing source and by gateway, Cloudflare by product, and the
/// promotional credits. Reads `cost_daily` only; no writes.
struct CostScreen: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var localRange = "7d"
    @State private var state: Loadable<CostReport> = .loading

    private static let ranges = ["7d", "today", "all"]

    /// The range: the selected view, or the screen's own picker when the area has no views.
    private var range: String {
        if context.area.views.isEmpty { return localRange }
        let id = context.viewID.wrappedValue
        return Self.ranges.contains(id) ? id : "7d"
    }

    var body: some View {
        List {
            if context.area.views.isEmpty {
                Section {
                    Picker("Range", selection: $localRange) {
                        Text("7 days").tag("7d")
                        Text("Today").tag("today")
                        Text("30 days").tag("all")
                    }
                    .pickerStyle(.segmented)
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
                }
            } else {
                PlatformViewPicker(context: context)
            }
            LoadableSection(
                state: state, emptyTitle: "No spend recorded",
                emptyMessage: "Nothing was spent in this range.",
                isEmpty: { $0.days.isEmpty }, retry: { await load() }
            ) { report in
                CostReportSections(report: report)
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task(id: range) {
            state = .loading
            await load()
        }
    }

    private func load() async {
        do {
            let data = try await api.op("cost_daily", args: ["range": .string(range)])
            state = .loaded(CostReport(data))
        } catch is CancellationError {
        } catch ConsoleError.signedOut {
        } catch {
            if state.value == nil { state = .failed(error.localizedDescription) }
        }
    }
}

// MARK: - Model

/// What `cost_daily` answers, read for the screen.
struct CostReport {
    struct Item: Identifiable {
        let id: String
        let title: String
        let detail: String?
        let total: Double
    }

    struct Day: Identifiable {
        let id: String
        let ai: Double
        let cloudflare: Double
        var total: Double { ai + cloudflare }
    }

    struct Grant: Identifiable {
        let id: String
        let name: String
        let amount: Double?
        let used: Double?
        let remaining: Double?
        let expiry: String?
    }

    struct Point: Identifiable {
        let id: String
        let date: Date
        let source: String
        let amount: Double
    }

    let days: [String]
    let total: Double?
    let netTotal: Double?
    let aiTotal: Double?
    let cloudflareTotal: Double?
    let byDay: [Day]
    let byBilling: [Item]
    let byGateway: [Item]
    let byProduct: [Item]
    let grants: [Grant]
    let gaps: [String]
    let feeUSD: Double?

    init(_ data: JSON) {
        let days = data["days"].items.compactMap { $0.string }.filter { PlatformFormat.isDay($0) }.sorted()
        self.days = days
        total = data["total"].double
        netTotal = data["net_total"].double
        let ai = data["ai_gateway"]
        let cf = data["cloudflare"]
        aiTotal = ai["total"].double
        cloudflareTotal = cf["total"].double

        let routes = ai["by_route"].items
        let lines = cf["lines"].items
        func sum(_ entries: [JSON], _ day: String) -> Double {
            entries.reduce(0) { $0 + ($1["by_day"][day].double ?? 0) }
        }
        byDay = days.reversed().map { d in
            Day(id: d, ai: sum(routes, d), cloudflare: sum(lines, d))
        }

        byBilling = routes.compactMap { (r: JSON) -> Item? in
            guard let name = r["name"].string, let t = r["total"].double, t != 0 else { return nil }
            return Item(id: "route:\(name)", title: name, detail: nil, total: t)
        }
        .sorted { $0.total > $1.total }

        byGateway = ai["by_gateway"].items.compactMap { (g: JSON) -> Item? in
            guard let t = g["total"].double, t != 0 else { return nil }
            let id = g["id"].string ?? g["label"].string ?? "gateway"
            var detail: [String] = []
            let pays = PlatformFormat.pays(g["billing"].string)
            if pays != "—" { detail.append(pays) }
            let models = g["by_model"].items
                .filter { ($0["total"].double ?? 0) != 0 }
                .prefix(2)
                .compactMap { $0["name"].string }
            detail.append(contentsOf: models)
            return Item(
                id: "gateway:\(id)", title: g["label"].string ?? id,
                detail: detail.isEmpty ? nil : detail.joined(separator: " · "), total: t)
        }
        .sorted { $0.total > $1.total }

        byProduct = lines.enumerated().compactMap { (pair: (offset: Int, element: JSON)) -> Item? in
            let l = pair.element
            guard let t = l["total"].double, t != 0 else { return nil }
            return Item(id: "line:\(pair.offset)", title: l["name"].string ?? "Usage", detail: nil, total: t)
        }
        .sorted { $0.total > $1.total }

        grants = data["credits"]["grants"].items.enumerated().map { (pair: (offset: Int, element: JSON)) -> Grant in
            let g = pair.element
            return Grant(
                id: "grant:\(pair.offset)", name: g["name"].string ?? "Credits",
                amount: g["amount"].double, used: g["used"].double,
                remaining: g["remaining"].double, expiry: g["expiry"].string)
        }

        var gaps: [String] = data["unavailable"].items.map { (u: JSON) -> String in
            let source = u["source"].string ?? "Some billing"
            if let why = u["why"].string { return "\(source) is unavailable (\(why))." }
            return "\(source) is unavailable."
        }
        if data["truncated"].bool == true {
            gaps.append("Gateway logs were truncated; model spend may be incomplete.")
        }
        self.gaps = gaps
        feeUSD = data["credits"]["fee_usd"].double
    }

    /// The headline: net of promotions when the platform sends it.
    var headline: Double? { netTotal ?? total }

    /// The period, as the web says it.
    var period: String {
        guard let last = days.last else { return "No days" }
        if days.count > 1 { return "\(days.count) UTC days, to \(PlatformFormat.dayLabel(last))" }
        return "\(PlatformFormat.dayLabel(last)), UTC"
    }

    /// The chart's bars: each day's AI Gateway and Cloudflare spend.
    var points: [Point] {
        byDay.reversed().flatMap { d -> [Point] in
            guard let date = PlatformFormat.date(ofDay: d.id) else { return [] }
            return [
                Point(id: "\(d.id)|ai", date: date, source: "AI Gateway", amount: d.ai),
                Point(id: "\(d.id)|cf", date: date, source: "Cloudflare", amount: d.cloudflare),
            ]
        }
    }
}

// MARK: - Sections

private struct CostReportSections: View {
    let report: CostReport

    var body: some View {
        Section {
            figures
        } footer: {
            Text("\(report.period) · platform-wide")
        }
        if !report.gaps.isEmpty || (report.feeUSD ?? 0) != 0 {
            Section {
                if !report.gaps.isEmpty {
                    Label("Partial totals. " + report.gaps.joined(separator: " "), systemImage: "exclamationmark.triangle")
                        .font(Theme.Fonts.meta).foregroundStyle(Theme.warn)
                }
                if let fee = report.feeUSD, fee != 0 {
                    Text("Totals include a \(PlatformFormat.money(fee)) estimated Unified Billing credits fee.")
                        .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                }
            }
        }
        if report.days.count > 1 {
            Section("By day") {
                chart
                    .padding(.vertical, 6)
                ForEach(report.byDay) { day in dayRow(day) }
            }
        }
        if !report.byBilling.isEmpty {
            Section("AI Gateway · by billing source") {
                ForEach(report.byBilling) { item in
                    shareRow(item, of: report.byBilling, color: Theme.accent)
                }
            }
        }
        if !report.byGateway.isEmpty {
            Section("AI Gateway · by gateway") {
                ForEach(report.byGateway) { item in
                    shareRow(item, of: report.byGateway, color: Theme.accent)
                }
            }
        }
        if !report.byProduct.isEmpty {
            Section("Cloudflare · by product") {
                ForEach(report.byProduct) { item in
                    shareRow(item, of: report.byProduct, color: Theme.info)
                }
            }
        }
        if !report.grants.isEmpty {
            Section("Promotional credits") {
                ForEach(report.grants) { grantRow($0) }
            }
        }
    }

    // MARK: Figures

    private var figures: some View {
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .top, spacing: 8) {
                figure("Net", report.headline)
                figure("AI Gateway", report.aiTotal)
                figure("Cloudflare", report.cloudflareTotal)
            }
            VStack(alignment: .leading, spacing: 8) {
                figure("Net", report.headline)
                figure("AI Gateway", report.aiTotal)
                figure("Cloudflare", report.cloudflareTotal)
            }
        }
        .padding(.vertical, 4)
    }

    private func figure(_ label: String, _ value: Double?) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(Theme.Fonts.caption).textCase(.uppercase)
                .foregroundStyle(Theme.muted)
            Text(PlatformFormat.money(value))
                .font(Theme.Fonts.figure)
                .foregroundStyle(Theme.ink)
                .lineLimit(1)
                .minimumScaleFactor(0.7)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }

    // MARK: Chart

    private var chart: some View {
        Chart(report.points) { p in
            BarMark(
                x: .value("Day", p.date, unit: .day),
                y: .value("Spend", p.amount)
            )
            .foregroundStyle(by: .value("Source", p.source))
        }
        .chartForegroundStyleScale(["AI Gateway": Theme.accent, "Cloudflare": Theme.info])
        .chartLegend(position: .bottom, alignment: .leading)
        .frame(height: 170)
        .accessibilityLabel("Spend by day, AI Gateway and Cloudflare")
    }

    // MARK: Rows

    private func dayRow(_ day: CostReport.Day) -> some View {
        let most = report.byDay.map { $0.total }.max() ?? 0
        return HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                Text(PlatformFormat.dayLabel(day.id)).font(Theme.Fonts.body).foregroundStyle(Theme.ink)
                ShareBar(
                    parts: [
                        .init(fraction: most > 0 ? day.ai / most : 0, color: Theme.accent),
                        .init(fraction: most > 0 ? day.cloudflare / most : 0, color: Theme.info),
                    ], width: 150)
            }
            Spacer(minLength: 8)
            Text(PlatformFormat.money(day.total)).font(Theme.Fonts.id).foregroundStyle(Theme.ink)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "\(PlatformFormat.dayLabel(day.id)): \(PlatformFormat.money(day.total)). AI Gateway \(PlatformFormat.money(day.ai)), Cloudflare \(PlatformFormat.money(day.cloudflare))"
        )
    }

    private func shareRow(_ item: CostReport.Item, of items: [CostReport.Item], color: Color) -> some View {
        let whole = items.reduce(0) { $0 + $1.total }
        let fraction = whole > 0 ? item.total / whole : 0
        return HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                Text(item.title).font(Theme.Fonts.body).foregroundStyle(Theme.ink).lineLimit(1)
                if let detail = item.detail {
                    Text(detail).font(Theme.Fonts.meta).foregroundStyle(Theme.muted).lineLimit(1)
                }
                ShareBar(parts: [.init(fraction: fraction, color: color)], width: 120)
            }
            Spacer(minLength: 8)
            VStack(alignment: .trailing, spacing: 2) {
                Text(PlatformFormat.money(item.total)).font(Theme.Fonts.id).foregroundStyle(Theme.ink)
                Text("\(Int((fraction * 100).rounded()))%")
                    .font(Theme.Fonts.caption).foregroundStyle(Theme.muted)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private func grantRow(_ g: CostReport.Grant) -> some View {
        let used = g.used ?? 0
        let fraction = (g.amount ?? 0) > 0 ? used / (g.amount ?? 1) : 0
        return HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                Text(g.name).font(Theme.Fonts.body).foregroundStyle(Theme.ink)
                Text("Expires \(g.expiry ?? "—") · \(Int((fraction * 100).rounded()))% used")
                    .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                BudgetMeter(fraction: fraction, width: 120)
            }
            Spacer(minLength: 8)
            StatusPill(
                "\(PlatformFormat.money(g.remaining)) of \(PlatformFormat.money(g.amount)) left",
                tone: grantTone(g))
        }
        .accessibilityElement(children: .combine)
    }

    /// Warn when a grant ends within two weeks or is mostly used; bad once it has expired.
    private func grantTone(_ g: CostReport.Grant) -> Tone {
        if let expiry = g.expiry, let date = PlatformFormat.date(ofDay: expiry) {
            let days = date.timeIntervalSinceNow / 86_400
            if days < -1 { return .bad }
            if days <= 14 { return .warn }
        }
        if let amount = g.amount, amount > 0, (g.remaining ?? 0) / amount < 0.2 { return .warn }
        return .ok
    }
}
