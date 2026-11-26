import SwiftUI

/// Metrics › Workers: request counts, errors and time for the instance's approved Worker over a
/// window of hours (`observability_worker_metrics` bound to the instance). Read-only.
struct MetricsScreen: View {
    let context: AreaContext
    @Environment(ConsoleAPI.self) private var api
    @State private var metrics = OpResource<JSON>("observability_worker_metrics")
    @State private var hours = 24

    private static let windows: [MetricsWindow] = [
        MetricsWindow(hours: 1, title: "1 h"), MetricsWindow(hours: 6, title: "6 h"),
        MetricsWindow(hours: 24, title: "24 h"), MetricsWindow(hours: 168, title: "7 d"),
    ]

    private var windowTitle: String {
        switch hours {
        case 1: return "Last hour"
        case 168: return "Last 7 days"
        default: return "Last \(hours) hours"
        }
    }

    var body: some View {
        List {
            ViewPickerSection(context: context)
            UpgradeSection(context: context)
            Section {
                Picker("Window", selection: $hours) {
                    ForEach(Self.windows, id: \.hours) { w in
                        Text(w.title).tag(w.hours)
                    }
                }
                .pickerStyle(.segmented)
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.clear)
            }
            LoadableSection(state: metrics.state, retry: { await load() }) { d in
                content(d)
            }
        }
        .listStyle(.insetGrouped)
        .refreshable { await load() }
        .task(id: hours) { await load() }
    }

    private func load() async {
        await metrics.load(api, args: ["instance": .string(context.instance.id), "hours": .number(Double(hours))])
    }

    /// "Worker x · from to to", as the web's line above the panels.
    private static func rangeText(_ d: JSON) -> String {
        var out = "Worker " + (d["worker"].string ?? "—")
        if let from = ConsoleFormat.ms(d["from"]) { out += " · " + ConsoleFormat.when(from) }
        if let to = ConsoleFormat.ms(d["to"]) { out += " to " + ConsoleFormat.when(to) }
        return out
    }

    @ViewBuilder
    private func content(_ d: JSON) -> some View {
        let m = d["metrics"]
        let rate = m["error_rate"].double ?? 0
        let statuses = (m["by_status"].object ?? [:])
            .map { StatusCount(status: $0.key, n: $0.value.double ?? 0) }
            .sorted { $0.n > $1.n }
        let total = statuses.reduce(0) { $0 + $1.n }
        let usd = d["estimated_cost"]["usd"].double
        let range = Self.rangeText(d)
        Section {
            ListRow(title: "Requests") {
                Text(ConsoleFormat.count(m["requests"])).font(Theme.Fonts.body.weight(.semibold).monospacedDigit())
            }
            ListRow(title: "Errors", subtitle: "\(ConsoleFormat.percent(rate)) error rate") {
                HStack(spacing: 6) {
                    if rate >= 0.01 {
                        StatusPill(ConsoleFormat.percent(rate), tone: .bad)
                    } else if rate >= 0.001 {
                        StatusPill(ConsoleFormat.percent(rate), tone: .warn)
                    }
                    Text(ConsoleFormat.count(m["errors"])).font(Theme.Fonts.body.weight(.semibold).monospacedDigit())
                }
            }
            ListRow(title: "Estimated cost", subtitle: d["estimated_cost"]["basis"].string) {
                Text(usd.map { String(format: $0 < 1 ? "$%.4f" : "$%.2f", $0) } ?? "—").idStyle()
            }
        } header: {
            Text(windowTitle)
        } footer: {
            Text(range)
        }
        Section("Time per request") {
            ListRow(title: "CPU p50 / p99") {
                Text("\(ConsoleFormat.millis(m["cpu_ms_p50"])) / \(ConsoleFormat.millis(m["cpu_ms_p99"]))").idStyle()
            }
            ListRow(title: "Wall p50 / p99") {
                Text("\(ConsoleFormat.millis(m["wall_ms_p50"])) / \(ConsoleFormat.millis(m["wall_ms_p99"]))").idStyle()
            }
        }
        Section("By status") {
            if statuses.isEmpty {
                MutedNoteRow("No requests in this window.")
            }
            ForEach(statuses, id: \.status) { s in
                VStack(alignment: .leading, spacing: 4) {
                    ListRow(title: ConsoleFormat.words(s.status), subtitle: total > 0 ? ConsoleFormat.percent(s.n / total) : nil) {
                        Text(ConsoleFormat.count(.number(s.n))).font(Theme.Fonts.body.monospacedDigit())
                    }
                    MetricsShareBar(fraction: total > 0 ? s.n / total : 0, tone: s.status == "success" ? .ok : .warn)
                }
            }
        }
    }
}

/// One choice of the metrics window.
struct MetricsWindow: Hashable {
    let hours: Int
    let title: String
}

/// Requests with one status.
struct StatusCount: Hashable {
    let status: String
    let n: Double
}

/// A thin bar for a share of a total.
struct MetricsShareBar: View {
    let fraction: Double
    let tone: Tone

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Theme.line)
                Capsule().fill(tone.ink).frame(width: max(2, geo.size.width * min(1, max(0, fraction))))
            }
        }
        .frame(height: 5)
        .accessibilityHidden(true)
    }
}
