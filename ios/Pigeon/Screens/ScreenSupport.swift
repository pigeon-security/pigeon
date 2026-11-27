import Foundation
import SwiftUI

// Helpers shared by the phase A screens (Inbox, Overview, Capabilities, Site, Storage, Source,
// CI, Metrics, Ingest). Formatting follows the web console's views (`views/*.js`).

/// The console's short formats: ages, deadlines, sizes, counts and durations.
enum ConsoleFormat {
    /// A time in epoch ms from a number, or from an ISO 8601 string.
    static func ms(_ value: JSON) -> Double? {
        if let n = value.double, n.isFinite, n > 0 { return n }
        guard let s = value.string, !s.isEmpty else { return nil }
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = iso.date(from: s) { return d.timeIntervalSince1970 * 1000 }
        iso.formatOptions = [.withInternetDateTime]
        if let d = iso.date(from: s) { return d.timeIntervalSince1970 * 1000 }
        return nil
    }

    private static func nowMilliseconds() -> Double { Date().timeIntervalSince1970 * 1000 }

    /// Whole minutes in `ms`, never negative.
    static func minutes(_ ms: Double) -> Int { max(0, Int((ms / 60_000).rounded())) }

    /// How long ago: "4 min", "3 h", "2 d"; "—" when unknown.
    static func ago(_ ms: Double?) -> String {
        guard let ms else { return "—" }
        let m = minutes(nowMilliseconds() - ms)
        if m < 60 { return "\(m) min" }
        if m < 2880 { return "\(m / 60) h" }
        return "\(m / 1440) d"
    }

    /// How long ago, as a phrase: "just now", "4 min ago".
    static func agoPhrase(_ ms: Double?) -> String {
        guard let ms else { return "—" }
        let m = minutes(nowMilliseconds() - ms)
        if m < 1 { return "just now" }
        return "\(ago(ms)) ago"
    }

    /// Time left until `ms`: "4 min", "2 h"; nil when unknown.
    static func left(_ ms: Double?) -> String? {
        guard let ms else { return nil }
        let m = minutes(ms - nowMilliseconds())
        return m < 60 ? "\(m) min" : "\(m / 60) h"
    }

    /// Minutes left until `ms` (negative once passed).
    static func minutesLeft(_ ms: Double) -> Double { (ms - nowMilliseconds()) / 60_000 }

    /// A moment in the device's time zone: "26 Nov, 14:05".
    static func when(_ ms: Double?) -> String {
        guard let ms else { return "—" }
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_GB")
        f.timeZone = .autoupdatingCurrent
        f.dateFormat = "d MMM yyyy, HH:mm"
        return f.string(from: Date(timeIntervalSince1970: ms / 1000))
    }

    /// The first `n` characters of `s`.
    static func short(_ s: String?, _ n: Int = 12) -> String { String((s ?? "").prefix(n)) }

    /// A branch without `refs/heads/`.
    static func branch(_ s: String?) -> String {
        let b = s ?? ""
        return b.hasPrefix("refs/heads/") ? String(b.dropFirst("refs/heads/".count)) : b
    }

    /// A count with thousands separators: "12,661".
    static func count(_ value: JSON) -> String {
        guard let n = value.double, n.isFinite else { return "—" }
        let f = NumberFormatter()
        f.numberStyle = .decimal
        f.locale = Locale(identifier: "en_US")
        f.maximumFractionDigits = 0
        return f.string(from: NSNumber(value: n.rounded())) ?? String(Int(n.rounded()))
    }

    /// A size in bytes: "214 KB", "8.1 KB".
    static func bytes(_ value: JSON) -> String {
        guard var n = value.double, n.isFinite else { return "—" }
        let units = ["B", "KB", "MB", "GB", "TB"]
        var i = 0
        while n >= 1000 && i < units.count - 1 {
            n /= 1000
            i += 1
        }
        if i == 0 { return "\(Int(n)) B" }
        return String(format: n < 10 ? "%.1f" : "%.0f", n) + " " + units[i]
    }

    /// A duration in ms: "48 s", "2 min 48 s", "1 h 3 min".
    static func duration(_ value: JSON) -> String {
        guard let ms = value.double, ms.isFinite else { return "—" }
        let s = Int((ms / 1000).rounded())
        if s < 60 { return "\(s) s" }
        if s < 3600 { return "\(s / 60) min \(s % 60) s" }
        return "\(s / 3600) h \((s % 3600) / 60) min"
    }

    /// Milliseconds of time: "0.6 ms", "12 ms".
    static func millis(_ value: JSON) -> String {
        guard let v = value.double, v.isFinite else { return "—" }
        return String(format: v < 10 ? "%.1f ms" : "%.0f ms", v)
    }

    /// A ratio as a percentage: "0.12%", "4.5%".
    static func percent(_ value: Double?) -> String {
        guard let r = value, r.isFinite else { return "—" }
        return String(format: r < 0.01 ? "%.2f%%" : "%.1f%%", r * 100)
    }

    /// Underscores as spaces: "timed out".
    static func words(_ s: String?) -> String { (s ?? "").replacingOccurrences(of: "_", with: " ") }

    /// Comma-separated text as a list, trimmed, without empties.
    static func list(_ text: String) -> [String] {
        text.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
    }

    /// The zone of the platform's own subdomains: the platform's host less its first label.
    static var platformZone: String {
        let host = OAuth.base.host ?? ""
        let parts = host.split(separator: ".")
        return parts.count > 1 ? parts.dropFirst().joined(separator: ".") : host
    }
}

/// The instance's approved record, read fresh from `instances_list` (as the web's `recordOf`).
enum InstanceRecordLoader {
    @MainActor
    static func load(_ api: ConsoleAPI, id: String) async throws -> JSON? {
        let data = try await api.op("instances_list")
        let rows = data.array ?? data["instances"].items
        return rows.first { $0["id"].string == id }
    }
}

/// One path of a record diff, as the platform diffs it.
struct RecordChange: Hashable {
    let path: String
    let from: JSON
    let to: JSON
}

/// The record diff the web console shows before `instance-put` (`views/capabilities.js`).
enum RecordDiff {
    /// Absent, null, false and empty are the same to the record (serde's defaults).
    static func isNone(_ v: JSON) -> Bool {
        switch v {
        case .null: return true
        case .bool(let b): return !b
        case .array(let a): return a.isEmpty
        case .object(let o): return o.isEmpty
        default: return false
        }
    }

    /// The paths that differ between `from` and `to`; the top-level `version` is left out.
    static func changes(_ from: JSON, _ to: JSON, path: String = "") -> [RecordChange] {
        if let a = from.object, let b = to.object {
            let keys = Set(a.keys).union(b.keys)
                .filter { !path.isEmpty || $0 != "version" }
                .sorted()
            var out: [RecordChange] = []
            for k in keys {
                let sub = path.isEmpty ? k : "\(path).\(k)"
                out.append(contentsOf: changes(a[k] ?? .null, b[k] ?? .null, path: sub))
            }
            return out
        }
        // A capability's own entry counts `{}` as a value: `capabilities.data = {}` turns it on
        // with every setting at its default, as record::diff counts it.
        let entry = path.hasPrefix("capabilities.")
            && path.count > "capabilities.".count
            && !path.dropFirst("capabilities.".count).contains(".")
        let none = { (v: JSON) -> Bool in entry && v == .object([:]) ? false : isNone(v) }
        let x: JSON = none(from) ? .null : from
        let y: JSON = none(to) ? .null : to
        return x == y ? [] : [RecordChange(path: path, from: x, to: y)]
    }

    /// The diff as lines: the version step, then each removal and addition.
    static func lines(_ record: JSON, _ list: [RecordChange]) -> [DiffLine] {
        let v = record["version"].int ?? 0
        var out: [DiffLine] = [.note("record version \(v) → \(v + 1)")]
        for c in list {
            if !c.from.isNull { out.append(.remove("\(c.path): \(c.from.compactText)")) }
            if !c.to.isNull { out.append(.add("\(c.path): \(c.to.compactText)")) }
        }
        return out
    }

    /// `object` with `key` set to `value` (removed when `value` is nil).
    static func setting(_ object: JSON, _ key: String, _ value: JSON?) -> JSON {
        var o = object.object ?? [:]
        if let value {
            o[key] = value
        } else {
            o.removeValue(forKey: key)
        }
        return .object(o)
    }

    /// The `instance-put` change for `next`: the whole record as JSON text.
    static func putChange(_ next: JSON) -> JSON {
        ["record": .string(next.compactText)]
    }
}

/// A short grey line in a list, such as "Read-only. …".
struct MutedNoteRow: View {
    let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(Theme.Fonts.meta)
            .foregroundStyle(Theme.muted)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// A failure inside a list section: the message and a retry.
struct SectionRetryError: View {
    let message: String
    let retry: () async -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(message)
                .font(Theme.Fonts.meta)
                .foregroundStyle(Theme.bad)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            Button("Try again") { Task { await retry() } }
                .buttonStyle(RowButtonStyle(.quiet))
        }
        .padding(.vertical, 4)
    }
}

/// The selected view's upgrade notice, as a section, when the manifest marks it.
struct UpgradeSection: View {
    let context: AreaContext

    var body: some View {
        if let view = context.view, view.upgrade != nil {
            Section { UpgradeNotice(view: view) }
        }
    }
}

/// The area's view picker, as the list's first section, when the area has views to pick.
struct ViewPickerSection: View {
    let context: AreaContext

    var body: some View {
        if context.area.views.count > 1 {
            Section {
                ViewPicker(views: context.area.views, selection: context.viewID)
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
            }
        }
    }
}
