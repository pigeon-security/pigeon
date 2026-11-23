import SwiftUI
import UIKit

// The Cost tab, from the confirmed mockup (docs/plans/ios-app-next.md). A range control, then a grid
// of equal squares: the total and a details box, a Cloudflare chart and an AI Gateway chart, and
// an Anthropic card with what Anthropic itself billed (docs/plans/model-providers.md). The total
// counts Anthropic's billed amount in place of the gateway's Anthropic estimate.
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
    case anthropic

    /// The source's name as shown.
    var title: String {
        switch self {
        case .cloudflare: return "Cloudflare"
        case .aiGateway: return "AI Gateway"
        case .anthropic: return "Anthropic"
        }
    }
}

/// One line or model: a name, a total and its amount for each day.
private struct CostSeries {
    let name: String
    let total: Double
    let byDay: [String: Double]
    let perPeriod: Bool
    /// How many requests it made, when the server says.
    let requests: Double?
}

/// How one source (Cloudflare or the gateway) answered: its status and a reason when not ok.
private struct CostSourceState {
    /// "ok", "empty" or "unavailable"; nil when the server sent none.
    var status: String?
    /// A short sentence saying why the source has no figure.
    var why: String?

    /// Reads the source's object. Without a status, a matching entry in the `unavailable`
    /// list makes the source unavailable. `name` is the list's source name, any letter case.
    init(object: [String: Any], name: String, unavailable: [Any]) {
        status = object["status"] as? String
        why = object["why"] as? String
        let entry = unavailable.compactMap { $0 as? [String: Any] }.first {
            ($0["source"] as? String)?.lowercased() == name.lowercased()
        }
        if status == nil, entry != nil {
            status = "unavailable"
        }
        if (why ?? "").isEmpty, status == "unavailable" {
            why = entry?["why"] as? String
        }
    }

    /// Whether the source failed or timed out.
    var isUnavailable: Bool { status == "unavailable" }
    /// Whether the source answered with no cost for the range.
    var isEmpty: Bool { status == "empty" }
    /// The reason to show beneath the amount, if any.
    var reason: String? {
        guard isUnavailable || isEmpty, let why, !why.isEmpty else { return nil }
        return why
    }

    /// The text for the source's total: "No data", "No cost in this range" or the money.
    func money(_ value: Double) -> String {
        if isUnavailable { return "No data" }
        if isEmpty { return "No cost in this range" }
        return CostText.money(value)
    }
}

/// The parts of a `cost_daily` answer the screen uses. Every field is read as optional.
private struct CostData {
    /// How Cloudflare answered.
    var cloudflareState = CostSourceState(object: [:], name: "billable usage", unavailable: [])
    /// How the AI Gateway answered.
    var gatewayState = CostSourceState(object: [:], name: "AI Gateway", unavailable: [])
    var range = ""
    var days: [String] = []
    var total = 0.0
    var cloudflareTotal = 0.0
    var lines: [CostSeries] = []
    var aiTotal = 0.0
    var models: [CostSeries] = []
    var unavailable = false
    var storedFrom: String?
    /// Whether Anthropic's own report answered (`anthropic` is not null).
    var anthropicConnected = false
    /// What Anthropic billed over the range.
    var anthropicTotal = 0.0
    /// Anthropic's billed cost by model family.
    var anthropicModels: [CostSeries] = []
    /// What the gateway recorded for Anthropic requests over the range.
    var gatewayAnthropic = 0.0
    /// Anthropic's figure less the gateway's.
    var anthropicDifference = 0.0
    /// Whether any day of the range does not match (beyond a day Anthropic has not reported).
    var reconciliationWarns = false

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
        models = Self.series(
            gateway["by_model"], order: ["Opus", "Sonnet", "Haiku", "OpenAI", "Other"])
        // Anthropic not connected is shown on its own card, not as billing unavailable.
        unavailable = (root["unavailable"] as? [Any] ?? []).contains { entry in
            ((entry as? [String: Any])?["source"] as? String) != "Anthropic"
        }
        let unavailableList = root["unavailable"] as? [Any] ?? []
        cloudflareState = CostSourceState(
            object: cloudflare, name: "billable usage", unavailable: unavailableList)
        gatewayState = CostSourceState(
            object: gateway, name: "AI Gateway", unavailable: unavailableList)
        storedFrom = root["stored_from"] as? String
        if let anthropic = root["anthropic"] as? [String: Any] {
            anthropicConnected = true
            anthropicTotal = Self.number(anthropic["total"])
            anthropicModels = Self.series(
                anthropic["by_model"], order: ["Opus", "Sonnet", "Haiku", "Other"],
                label: Self.family)
            let reconciliation = anthropic["reconciliation"] as? [String: Any] ?? [:]
            gatewayAnthropic = Self.number(reconciliation["gateway_usd"])
            anthropicDifference = Self.number(reconciliation["difference_usd"])
            let quiet: Set<String> = ["match", "anthropic_pending"]
            reconciliationWarns = (reconciliation["days"] as? [Any] ?? []).contains { day in
                guard let status = (day as? [String: Any])?["status"] as? String else {
                    return false
                }
                return !quiet.contains(status)
            }
        }
    }

    /// A model id's short name: Opus, Sonnet or Haiku when the id holds it, else the id.
    static func family(_ model: String) -> String {
        let id = model.lowercased()
        for name in ["Opus", "Sonnet", "Haiku"] where id.contains(name.lowercased()) {
            return name
        }
        return model
    }

    /// A number that may arrive as an Int or a Double; anything else is 0.
    static func number(_ value: Any?) -> Double {
        (value as? NSNumber)?.doubleValue ?? 0
    }

    /// Reads a list of series, names them by `label` (rows with the same label are added
    /// together) and puts them in `order`; unknown names go last, in given order.
    private static func series(
        _ value: Any?, order: [String], label: (String) -> String = { $0 }
    ) -> [CostSeries] {
        let rows = (value as? [Any] ?? []).compactMap { $0 as? [String: Any] }
        var list: [CostSeries] = []
        for row in rows {
            let name = label(row["name"] as? String ?? "Other")
            var byDay: [String: Double] = [:]
            for (day, amount) in row["by_day"] as? [String: Any] ?? [:] {
                byDay[day] = number(amount)
            }
            let next = CostSeries(
                name: name, total: number(row["total"]),
                byDay: byDay, perPeriod: (row["granularity"] as? String) == "period",
                requests: (row["requests"] as? NSNumber)?.doubleValue)
            if let i = list.firstIndex(where: { $0.name == name }) {
                let old = list[i]
                let requests: Double? =
                    old.requests == nil && next.requests == nil
                    ? nil : (old.requests ?? 0) + (next.requests ?? 0)
                list[i] = CostSeries(
                    name: name, total: old.total + next.total,
                    byDay: old.byDay.merging(next.byDay, uniquingKeysWith: +),
                    perPeriod: old.perPeriod || next.perPeriod, requests: requests)
            } else {
                list.append(next)
            }
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

    /// The short x label for a today bar: CI, Wkrs, D1, R2, or the first five letters.
    static func shortName(_ name: String) -> String {
        switch name {
        case "CI Containers": return "CI"
        case "Workers": return "Wkrs"
        default: return String(name.prefix(5))
        }
    }

    /// The amount above a today bar: "$5.40", or "$12" from ten dollars up.
    static func shortMoney(_ v: Double) -> String {
        v >= 10 ? String(format: "$%.0f", v) : money(v)
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
                id: i, axis: CostText.shortName(s.name), date: day, isWeek: false,
                parts: [CostPart(name: s.name, value: s.byDay[day] ?? s.total)])
        }
    }
    if range == .week || days.count <= 31 {
        return days.enumerated().map { i, day in
            let initial =
                range == .week
                ? CostText.day(day, "EEEEE") : (i % 7 == 0 ? String(Int(day.suffix(2)) ?? 0) : "")
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

/// One row of the details box: a swatch, a name and an amount.
private struct DetailRow {
    let name: String
    let amount: Double
    /// The swatch colour; nil for the "+N more" row, which has none and is muted.
    let colour: Color?
    /// Text shown in place of the amount (for a source with no data), if any.
    var text: String? = nil
}

/// What the details box shows: a source line, a head line, then rows or note lines.
private struct Details {
    var source: String
    var head: String
    var rows: [DetailRow] = []
    var notes: [String] = []

    /// One sentence for VoiceOver.
    var summary: String {
        let parts = [source, head] + notes + rows.map {
            "\($0.name) \($0.text ?? CostText.money($0.amount))"
        }
        return parts.joined(separator: ", ")
    }
}

/// The Cost tab: the range control, then the grid of squares.
struct CostScreen: View {
    let session: Session

    @State private var range = CostRange.week
    @State private var data: CostData?
    @State private var error: String?
    @State private var selection: CostSelection?
    @State private var side: CGFloat = 173

    private let gap: CGFloat = 12

    var body: some View {
        Screen(title: "Cost", root: true, errors: error.map { [$0] } ?? [], refresh: { await load() }) {
            VStack(alignment: .leading, spacing: gap) {
                HStack {
                    Spacer(minLength: 0)
                    Picker("Range", selection: $range) {
                        ForEach(CostRange.allCases) { Text($0.title).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .frame(width: side)
                }
                HStack(spacing: gap) {
                    totalCard.frame(width: side, height: side)
                    detailsCard.frame(width: side, height: side)
                }
                HStack(spacing: gap) {
                    chartCard(.cloudflare).frame(width: side, height: side)
                    chartCard(.aiGateway).frame(width: side, height: side)
                }
                HStack(spacing: gap) {
                    anthropicCard.frame(width: side, height: side)
                    Spacer(minLength: 0)
                }
            }
            .padding(.top, 2)
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width in
                side = max((width - gap) / 2, 80)
            }
        }
        .task(id: range) { await load() }
        .onChange(of: range) { selection = nil }
        .onChange(of: selection) { announce() }
    }

    /// Says the details box's new text to VoiceOver.
    private func announce() {
        UIAccessibility.post(notification: .announcement, argument: details.summary)
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
        costBars(data?.models ?? [], days: data?.days ?? [], range: range)
    }

    private var anthropicBars: [CostBar] {
        costBars(data?.anthropicModels ?? [], days: data?.days ?? [], range: range)
    }

    private func bars(_ source: CostSource) -> [CostBar] {
        switch source {
        case .cloudflare: return cloudflareBars
        case .aiGateway: return gatewayBars
        case .anthropic: return anthropicBars
        }
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

    // MARK: Total

    /// The range in the total's label: "today", "7 days" or "31 days".
    private var rangeWords: String {
        switch range {
        case .today: return "today"
        case .week: return "7 days"
        case .all: return "\(data?.days.count ?? 0) days"
        }
    }

    private var totalCard: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Total · \(rangeWords)").font(AppFont.label).foregroundStyle(Tokens.muted)
                .lineLimit(1)
            Spacer(minLength: 0)
            Text(CostText.money(data?.total ?? 0))
                .font(AppFont.largeTitle).tracking(-0.8).monospacedDigit()
                .foregroundStyle(Tokens.ink)
                .lineLimit(1).minimumScaleFactor(0.5)
            if let partial = partialNote {
                Text(partial).font(.system(size: 13)).foregroundStyle(Tokens.muted)
                    .lineLimit(2)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .block()
    }

    /// A line saying the total leaves out a source with no data, or nil when it is whole.
    private var partialNote: String? {
        var missing: [String] = []
        if data?.cloudflareState.isUnavailable == true { missing.append("Cloudflare") }
        if data?.gatewayState.isUnavailable == true { missing.append("AI Gateway") }
        if missing.isEmpty { return nil }
        return "Without \(missing.joined(separator: " and ")): no data"
    }

    // MARK: Details

    /// The head of the box with nothing selected: "Last 7 days · $12.00".
    private var rangeHead: String {
        let name: String
        switch range {
        case .today: name = "Today"
        case .week: name = "Last 7 days"
        case .all: name = "All"
        }
        return "\(name) · \(CostText.money(data?.total ?? 0))"
    }

    /// The total by source, adding up to the total. With Anthropic connected, the gateway's
    /// Anthropic part leaves the gateway's row, and Anthropic's row is what the total counts for
    /// Anthropic (its billed amount, the gateway's estimate on days it has not reported yet).
    private var sourceRows: [DetailRow] {
        let cf = data?.cloudflareState
        let gw = data?.gatewayState
        func text(_ s: CostSourceState?) -> String? {
            guard let s, s.isUnavailable || s.isEmpty else { return nil }
            return s.money(0)
        }
        var rows = [
            DetailRow(
                name: "Cloudflare", amount: data?.cloudflareTotal ?? 0,
                colour: ChartColour.workers, text: text(cf))
        ]
        if let d = data, d.anthropicConnected {
            let others = max(d.aiTotal - d.gatewayAnthropic, 0)
            rows.append(
                DetailRow(
                    name: "Other models", amount: others, colour: Tokens.accent, text: text(gw)))
            rows.append(
                DetailRow(
                    name: "Anthropic", amount: max(d.total - d.cloudflareTotal - others, 0),
                    colour: ChartColour.opus))
        } else {
            rows.append(
                DetailRow(
                    name: "AI Gateway", amount: data?.aiTotal ?? 0, colour: Tokens.accent,
                    text: text(gw)))
        }
        return rows.sorted { $0.amount > $1.amount }
    }

    /// The reasons, one line per source with no figure, for the details box.
    private var sourceReasons: [String] {
        var out: [String] = []
        if let why = data?.cloudflareState.reason { out.append("Cloudflare: \(why)") }
        if let why = data?.gatewayState.reason { out.append("AI Gateway: \(why)") }
        return out
    }

    /// What the details box shows now.
    private var details: Details {
        guard let (source, bar) = selectedBar else {
            return Details(
                source: "By source", head: rangeHead, rows: sourceRows, notes: sourceReasons)
        }
        let items = bar.parts.filter { $0.value > 0 }.sorted { $0.value > $1.value }
        if range == .today {
            return todayDetails(source, bar)
        }
        let day = CostText.day(bar.date, "EEE d MMM")
        var out = Details(
            source: source.title, head: "\(bar.isWeek ? "Week of " : "")\(day) · \(CostText.money(bar.total))")
        func row(_ p: CostPart) -> DetailRow {
            DetailRow(name: p.name, amount: p.value, colour: ChartColour.of(p.name))
        }
        if items.count > 4 {
            out.rows = items.prefix(3).map(row)
            let rest = items.dropFirst(3)
            out.rows.append(
                DetailRow(
                    name: "+\(rest.count) more", amount: rest.reduce(0) { $0 + $1.value },
                    colour: nil))
        } else {
            out.rows = items.map(row)
        }
        return out
    }

    /// The details of one item of today: its amount, its share of the day and a note.
    private func todayDetails(_ source: CostSource, _ bar: CostBar) -> Details {
        let part = bar.parts.first
        let name = part?.name ?? source.title
        let amount = part?.value ?? 0
        var out = Details(source: source.title, head: "\(name) · \(CostText.money(amount))")
        let total = data?.total ?? 0
        let share = total > 0 ? Int((amount / total * 100).rounded()) : 0
        out.notes.append("\(share)% of today")
        let list: [CostSeries]?
        switch source {
        case .cloudflare: list = data?.lines
        case .aiGateway: list = data?.models
        case .anthropic: list = data?.anthropicModels
        }
        let series = list?.first { $0.name == name }
        if source == .cloudflare {
            out.notes.append(series?.perPeriod == true ? "Billed" : "Estimated")
        } else if source == .anthropic {
            out.notes.append("Billed by Anthropic")
        } else if let n = series?.requests {
            out.notes.append("\(Int(n)) requests")
        } else {
            out.notes.append("Estimated")
        }
        return out
    }

    private var detailsCard: some View {
        let d = details
        return VStack(alignment: .leading, spacing: 5) {
            Text(d.source).font(AppFont.label).foregroundStyle(Tokens.muted).lineLimit(1)
            Text(d.head).font(AppFont.button).monospacedDigit().foregroundStyle(Tokens.ink)
                .lineLimit(1).minimumScaleFactor(0.7)
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(d.rows.prefix(4).enumerated()), id: \.offset) { _, row in
                    detailRow(row)
                }
                ForEach(d.notes, id: \.self) { note in
                    Text(note).font(.system(size: 13)).foregroundStyle(Tokens.muted)
                        .lineLimit(3)
                        .frame(minHeight: 17, alignment: .leading)
                }
            }
            .padding(.top, 0)
            Spacer(minLength: 0)
        }
        .padding(EdgeInsets(top: 12, leading: 12, bottom: 14, trailing: 12))
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .contentShape(Rectangle())
        .onTapGesture { selection = nil }
        .block()
        .accessibilityElement(children: .combine)
    }

    /// One row of the details box.
    private func detailRow(_ row: DetailRow) -> some View {
        HStack(spacing: 6) {
            if let colour = row.colour {
                RoundedRectangle(cornerRadius: 2).fill(colour).frame(width: 8, height: 8)
            }
            Text(row.name).font(.system(size: 13))
                .foregroundStyle(row.colour == nil ? Tokens.muted : Tokens.ink)
                .lineLimit(1).truncationMode(.tail)
            Spacer(minLength: 2)
            Text(row.text ?? CostText.money(row.amount)).font(.system(size: 13)).monospacedDigit()
                .foregroundStyle(row.colour == nil ? Tokens.muted : Tokens.ink)
                .lineLimit(1).minimumScaleFactor(0.6)
        }
        .frame(height: 17)
    }

    // MARK: Charts

    private func chartCard(_ source: CostSource) -> some View {
        let isCloudflare = source == .cloudflare
        let label = isCloudflare && range == .all ? "Cloudflare · period" : source.title
        let state = isCloudflare ? data?.cloudflareState : data?.gatewayState
        let noFigure = state?.isUnavailable == true || state?.isEmpty == true
        return VStack(alignment: .leading, spacing: 6) {
            Text(label).font(AppFont.label).foregroundStyle(Tokens.muted)
                .lineLimit(1).minimumScaleFactor(0.7)
            if let state, noFigure {
                Text(state.money(0))
                    .font(AppFont.sourceTotal).tracking(-0.3)
                    .foregroundStyle(Tokens.ink)
                    .lineLimit(1).minimumScaleFactor(0.5)
                if let why = state.reason {
                    Text(why).font(.system(size: 13)).foregroundStyle(Tokens.muted)
                        .lineLimit(3)
                }
                Spacer(minLength: 0)
            } else {
                Text(CostText.money(isCloudflare ? data?.cloudflareTotal ?? 0 : data?.aiTotal ?? 0))
                    .font(AppFont.sourceTotal).tracking(-0.3).monospacedDigit()
                    .foregroundStyle(Tokens.ink)
                    .lineLimit(1).minimumScaleFactor(0.5)
                chart(source)
            }
        }
        .padding(EdgeInsets(top: 10, leading: 10, bottom: 8, trailing: 10))
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .block()
    }

    /// The Anthropic card: what Anthropic itself billed, by model, and how far the gateway's
    /// figure for the same days is from it. Not connected until ANTHROPIC_ADMIN_KEY is set.
    private var anthropicCard: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(CostSource.anthropic.title).font(AppFont.label).foregroundStyle(Tokens.muted)
                .lineLimit(1).minimumScaleFactor(0.7)
            if data?.anthropicConnected == true {
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(CostText.money(data?.anthropicTotal ?? 0))
                        .font(AppFont.sourceTotal).tracking(-0.3).monospacedDigit()
                        .foregroundStyle(Tokens.ink)
                        .lineLimit(1).minimumScaleFactor(0.5)
                    Spacer(minLength: 2)
                    Text(differenceText).font(AppFont.chart).monospacedDigit()
                        .foregroundStyle(
                            data?.reconciliationWarns == true ? Tokens.warn : Tokens.muted)
                        .lineLimit(1).minimumScaleFactor(0.6)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(reconciliationText)
                chart(.anthropic)
            } else {
                Text("Not connected").font(AppFont.meta).foregroundStyle(Tokens.ink)
                Text("Set ANTHROPIC_ADMIN_KEY on the console's Access tab.")
                    .font(AppFont.chart).foregroundStyle(Tokens.muted)
                    .lineLimit(4)
                Spacer(minLength: 0)
            }
        }
        .padding(EdgeInsets(top: 10, leading: 10, bottom: 8, trailing: 10))
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .block()
    }

    /// The difference beside the total: "±$Y vs gateway", marked when a day does not match.
    private var differenceText: String {
        let difference = data?.anthropicDifference ?? 0
        let sign = difference < 0 ? "−" : "+"
        let mark = data?.reconciliationWarns == true ? "⚠ " : ""
        return "\(mark)\(sign)\(CostText.money(abs(difference))) vs gateway"
    }

    /// For VoiceOver: Anthropic's total, the gateway's figure and the difference.
    private var reconciliationText: String {
        let difference = data?.anthropicDifference ?? 0
        let sign = difference < 0 ? "minus " : "plus "
        let warn = data?.reconciliationWarns == true ? ", some days do not match" : ""
        let billed = CostText.money(data?.anthropicTotal ?? 0)
        let gateway = CostText.money(data?.gatewayAnthropic ?? 0)
        return "Anthropic billed \(billed), the gateway recorded \(gateway), difference \(sign)"
            + "\(CostText.money(abs(difference)))\(warn)"
    }

    /// The space between bars: 6 for today, 5 for 7 days, 1 for All.
    private var barGap: CGFloat {
        switch range {
        case .today: return 6
        case .week: return 5
        case .all: return 1
        }
    }

    /// The plot and its x labels. The plot is 86 pt high with a 1 pt baseline.
    private func chart(_ source: CostSource) -> some View {
        let list = bars(source)
        let peak = list.map(\.total).max() ?? 0
        let valueHeight: CGFloat = range == .today ? 14 : 0
        let plot: CGFloat = 86
        return VStack(spacing: 0) {
            HStack(alignment: .bottom, spacing: barGap) {
                ForEach(list) { bar in
                    let faded = selection != nil && selection != CostSelection(
                        source: source, bar: bar.id)
                    VStack(spacing: 0) {
                        Spacer(minLength: 0)
                        if range == .today {
                            Text(CostText.shortMoney(bar.total))
                                .font(.system(size: 11, weight: .semibold)).monospacedDigit()
                                .foregroundStyle(Tokens.ink)
                                .lineLimit(1).minimumScaleFactor(0.5)
                                .frame(height: valueHeight)
                        }
                        stack(bar, peak: peak, height: plot - valueHeight)
                    }
                    .opacity(faded ? 0.4 : 1)
                    .frame(maxWidth: .infinity)
                    .contentShape(Rectangle())
                    .onTapGesture { tap(source, bar.id) }
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(accessibility(source, bar))
                    .accessibilityAddTraits(.isButton)
                }
            }
            .frame(height: plot)
            Rectangle().fill(Tokens.line).frame(height: 1)
            HStack(spacing: barGap) {
                ForEach(list) { bar in
                    Color.clear.frame(maxWidth: .infinity).frame(height: 14)
                        .overlay(
                            Text(bar.axis).font(AppFont.chart).foregroundStyle(Tokens.muted)
                                .lineLimit(1).fixedSize())
                }
            }
            .padding(.top, 3)
        }
    }

    /// The VoiceOver label of a bar: date, source and amount.
    private func accessibility(_ source: CostSource, _ bar: CostBar) -> String {
        var name = source.title
        if range == .today, let item = bar.parts.first?.name { name += ", \(item)" }
        let day = CostText.day(bar.date, "EEE d MMM")
        return "\(bar.isWeek ? "Week of " : "")\(day), \(name), \(CostText.money(bar.total))"
    }

    /// One bar's stack, the first part at the bottom, 2 pt between parts and rounded top
    /// corners. A zero bar is a 1 pt tick.
    private func stack(_ bar: CostBar, peak: Double, height: CGFloat) -> some View {
        let parts = bar.parts.filter { $0.value > 0 }
        let gaps = CGFloat(max(parts.count - 1, 0)) * 2
        let whole = peak > 0 ? CGFloat(bar.total / peak) * height : 0
        let usable = max(whole - gaps, 0)
        return VStack(spacing: 2) {
            if bar.total <= 0 || peak <= 0 {
                Rectangle().fill(Tokens.muted).frame(height: 1)
            } else {
                ForEach(Array(parts.enumerated().reversed()), id: \.offset) { i, part in
                    let h = max(CGFloat(part.value / bar.total) * usable, 1)
                    UnevenRoundedRectangle(
                        topLeadingRadius: i == parts.count - 1 ? 3 : 0,
                        topTrailingRadius: i == parts.count - 1 ? 3 : 0
                    )
                    .fill(ChartColour.of(part.name))
                    .frame(height: h)
                }
            }
        }
    }
}
