import Foundation

/// A JSON number as a Double; nil for anything else, so a missing or odd field never crashes.
func num(_ value: Any?) -> Double? {
    if let n = value as? NSNumber { return n.doubleValue }
    return nil
}

/// A cost as dollars. The server sends numbers or strings such as "$1.50"; both are read.
func parseCost(_ value: Any?) -> Double? {
    if let n = num(value) { return n }
    guard let s = value as? String else { return nil }
    let cleaned = s.filter { $0.isNumber || $0 == "." || $0 == "-" }
    return Double(cleaned)
}

/// A length of time in ms as "5 min", "1 h 12 min" or "2 d 3 h".
func durationText(_ ms: Double) -> String {
    let minutes = Int(max(0, ms) / 60_000)
    if minutes < 1 { return "under 1 min" }
    if minutes < 60 { return "\(minutes) min" }
    if minutes < 1440 { return "\(minutes / 60) h \(minutes % 60) min" }
    return "\(minutes / 1440) d \((minutes % 1440) / 60) h"
}

/// The current time in ms since the epoch.
func nowMs() -> Double { Date().timeIntervalSince1970 * 1000 }

/// "12 min left" for a deadline in ms, or "expired".
func timeLeftText(_ expiresMs: Double) -> String {
    let left = expiresMs - nowMs()
    return left <= 0 ? "expired" : "\(durationText(left)) left"
}

/// A timestamp in epoch milliseconds or ISO 8601, shown in the device's time zone.
func timestampText(_ value: Any?, now: Date = Date()) -> String {
    let date: Date
    if let value = value as? Date {
        date = value
    } else if let ms = num(value), ms.isFinite {
        date = Date(timeIntervalSince1970: ms / 1000)
    } else if let text = value as? String {
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let parsed = iso.date(from: text) {
            date = parsed
        } else {
            iso.formatOptions = [.withInternetDateTime]
            guard let parsed = iso.date(from: text) else { return "unknown" }
            date = parsed
        }
    } else {
        return "unknown"
    }

    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = .autoupdatingCurrent
    let formatter = DateFormatter()
    formatter.locale = Locale(identifier: "en_GB")
    formatter.calendar = calendar
    formatter.timeZone = calendar.timeZone
    formatter.dateFormat = "HH:mm"
    if calendar.isDate(date, inSameDayAs: now) {
        return "Today, \(formatter.string(from: date))"
    }
    if let yesterday = calendar.date(byAdding: .day, value: -1, to: now),
        calendar.isDate(date, inSameDayAs: yesterday)
    {
        return "Yesterday, \(formatter.string(from: date))"
    }
    formatter.dateFormat = "d MMM, HH:mm"
    return formatter.string(from: date)
}

/// One or two capital letters from a name: the first letters of its first two words.
func initialsOf(_ name: String) -> String {
    let letters = name.split(whereSeparator: { $0.isWhitespace }).prefix(2).compactMap { $0.first }
    let text = String(letters).uppercased()
    return text.isEmpty ? "?" : text
}

/// A run's short name: its `name` when the server sends one, else its title, else "Untitled run".
func runName(_ run: [String: Any]) -> String {
    if let n = run["name"] as? String, !n.trimmingCharacters(in: .whitespaces).isEmpty { return n }
    return runTitle(run)
}

/// A run's title: its `title` when the server sends one, else the first line of its brief.
func runTitle(_ run: [String: Any]) -> String {
    if let t = run["title"] as? String, !t.isEmpty { return t }
    let brief = run["brief"] as? String ?? ""
    let first = brief.split(whereSeparator: { $0.isNewline })
        .map { $0.trimmingCharacters(in: .whitespaces) }.first { !$0.isEmpty } ?? ""
    var line = first
    for prefix in ["# ", "Brief:", "Task:"] where line.hasPrefix(prefix) {
        line = String(line.dropFirst(prefix.count)).trimmingCharacters(in: .whitespaces)
    }
    if line.isEmpty { return "Untitled run" }
    return line.count > 80 ? String(line.prefix(80)) + "…" : line
}
