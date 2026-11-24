import SwiftUI

/// A real balance from the promotional grant ledger, with an inclusive UTC expiry date.
struct GrantExpiry: Decodable {
    let name: String
    let remaining: Double
    let expiry: String
    let expiryDate: Date

    private enum CodingKeys: String, CodingKey {
        case name, remaining, expiry
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decode(String.self, forKey: .name)
        remaining = try container.decode(Double.self, forKey: .remaining)
        expiry = try container.decode(String.self, forKey: .expiry)
        let format = DateFormatter()
        format.locale = Locale(identifier: "en_US_POSIX")
        format.calendar = Self.utc
        format.timeZone = .gmt
        format.dateFormat = "yyyy-MM-dd"
        guard !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
            remaining.isFinite, let date = format.date(from: expiry), format.string(from: date) == expiry
        else { throw PlatformError.badResponse("promotional grant") }
        expiryDate = date
    }

    static var utc: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = .gmt
        return calendar
    }

    /// Days from today to the expiry, independent of the device's time zone.
    func daysLeft(on day: Date) -> Int? {
        Self.utc.dateComponents([.day], from: Self.utc.startOfDay(for: day), to: expiryDate).day
    }

    /// Reads only the promotional ledger; other billing sources can fail independently.
    static func read(_ json: Any) throws -> [Self] {
        let root = json as? [String: Any]
        guard let grants = (root?["credits"] as? [String: Any])?["grants"],
            !(root?["notes"] as? [String] ?? []).contains(where: {
                $0.hasPrefix("The promotional grant configuration is invalid")
            })
        else { throw PlatformError.badResponse("promotional grants") }
        return try decodeNoticeJSON([Self].self, grants)
            .filter { $0.remaining > 0 }.sorted { $0.expiry < $1.expiry }
    }
}

/// One actual step of the server plan that crosses a major or has breaking work.
struct BreakingUpdate: Identifiable {
    struct ID: Hashable {
        let instance: String
        let component: String
    }

    let instance: String
    let step: CapabilitySnapshot.Step
    var id: ID { ID(instance: instance, component: step.component) }

    private struct Plan: Decodable {
        struct Instance: Decodable {
            let id: String
            let steps: [CapabilitySnapshot.Step]
        }
        let instances: [Instance]
    }

    /// Uses the plan directly; no additional version or record reads are needed for notices.
    static func read(_ json: Any) throws -> [Self] {
        let plan = try decodeNoticeJSON(Plan.self, json)
        return plan.instances.flatMap { instance in
            instance.steps.filter { !$0.work.isEmpty || $0.breaking }.map {
                Self(instance: instance.id, step: $0)
            }
        }
    }
}

private func decodeNoticeJSON<T: Decodable>(_ type: T.Type, _ json: Any) throws -> T {
    try JSONDecoder().decode(type, from: JSONSerialization.data(withJSONObject: json))
}

/// Displays notices without putting actions on them.
struct NeedsYouNotices: View {
    let model: NeedsYouModel
    @Environment(InstanceStore.self) private var store

    var body: some View {
        let grants = model.expiringGrants
        if !grants.isEmpty {
            CardSection("Grants expiring soon") {
                VStack(spacing: Metrics.gap) {
                    ForEach(grants.indices, id: \.self) { index in
                        let (grant, days) = grants[index]
                        PlainCard {
                            Text(grant.name).font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
                            Text("\(grant.remaining.formatted(.currency(code: "USD"))) remaining")
                                .font(AppFont.body).foregroundStyle(Tokens.ink)
                            Text("\(days == 0 ? "Expires today" : "Expires") · \(grant.expiry) UTC")
                                .font(AppFont.meta).foregroundStyle(days <= 3 ? Tokens.warn : Tokens.muted)
                        }
                    }
                }
            }
        }
        if !model.breakingUpdates.isEmpty {
            CardSection("Breaking capability updates") {
                VStack(spacing: Metrics.gap) {
                    ForEach(model.breakingUpdates) { update in
                        PlainCard {
                            Text(update.step.component).font(AppFont.cardTitle).foregroundStyle(Tokens.ink)
                            Text("\(instanceName(update.instance)) · Target \(update.step.to)")
                                .font(AppFont.body).foregroundStyle(Tokens.warn)
                            ForEach(update.step.work, id: \.version) { work in
                                Text("\(work.version): \(work.summary)")
                                    .font(AppFont.meta).foregroundStyle(Tokens.muted)
                            }
                        }
                    }
                }
            }
        }
    }

    private func instanceName(_ id: String) -> String {
        store.instances.first { $0.id == id }?.title ?? id
    }
}
