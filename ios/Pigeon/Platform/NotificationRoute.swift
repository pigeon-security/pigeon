import Foundation
import Observation
import UserNotifications

/// Where a tapped notification takes the app. Decision A6: a notification never approves or
/// denies by itself. Tapping it, or its one Review action, opens the Inbox; when it names an
/// approval, the Inbox opens that approval's confirm sheet (the person still confirms there,
/// with the passkey).
///
/// The platform's approval push (`approvals.rs`) carries `link: pigeon://approvals` and the
/// collapse id `approval:<id>`, which iOS uses as the request's identifier. The Review button
/// shows when the push's `aps.category` is `APPROVAL`.
@MainActor
@Observable
final class NotificationRoute {
    static let shared = NotificationRoute()

    /// The category of an approval push: one Review action, which opens the app.
    nonisolated static let approvalCategory = "APPROVAL"
    nonisolated static let reviewAction = "REVIEW"

    /// Bumped each time a notification asks to open the Inbox; `AppShell` follows it.
    private(set) var serial = 0
    /// The approval the last notification named, until the Inbox takes it.
    private(set) var pendingApprovalID: String?

    /// Asks the shell to open the Inbox, and the Inbox to review `approvalID` when given.
    func open(approvalID: String?) {
        pendingApprovalID = approvalID
        serial += 1
    }

    /// The approval to review, once: the Inbox calls this when it appears or `serial` changes,
    /// and presents that approval's confirm sheet if it lists it.
    func takeApproval() -> String? {
        let id = pendingApprovalID
        pendingApprovalID = nil
        return id
    }

    /// Registers the approval category with its Review action. Review opens the app
    /// (`.foreground`), so nothing is decided from the lock screen.
    nonisolated static func registerCategories() {
        let review = UNNotificationAction(
            identifier: reviewAction, title: "Review", options: [.foreground])
        let approval = UNNotificationCategory(
            identifier: approvalCategory, actions: [review], intentIdentifiers: [], options: [])
        UNUserNotificationCenter.current().setNotificationCategories([approval])
    }

    /// The approval a notification is about: `approval` in its data, else its identifier
    /// `approval:<id>` (the APNs collapse id). Nil for any other notification.
    nonisolated static func approvalID(identifier: String, userInfo: [AnyHashable: Any]) -> String? {
        if let id = userInfo["approval"] as? String, !id.isEmpty { return id }
        let prefix = "approval:"
        if identifier.hasPrefix(prefix) {
            let id = String(identifier.dropFirst(prefix.count))
            return id.isEmpty ? nil : id
        }
        return nil
    }

    /// Whether a notification should open the Inbox: an approval, or a link to approvals or
    /// landings.
    nonisolated static func opensInbox(identifier: String, userInfo: [AnyHashable: Any]) -> Bool {
        if approvalID(identifier: identifier, userInfo: userInfo) != nil { return true }
        let link = userInfo["link"] as? String ?? ""
        return link.hasPrefix("pigeon://approvals") || link.hasPrefix("pigeon://inbox")
            || link.hasPrefix("pigeon://land")
    }
}
