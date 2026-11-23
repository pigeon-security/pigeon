import OSLog
import UIKit
import UserNotifications

/// Registers this device for push notifications and tells the platform its APNs token.
@MainActor
final class Push {
    /// The one instance, shared by the app delegate and the views.
    static let shared = Push()

    private let log = Logger(subsystem: "dev.pigeon.app", category: "push")
    private var client: MCPClient?
    private var token: String?
    private var sent = false

    /// Asks the person to allow notifications, then registers with APNs when they do.
    /// Remembers `client` so the token can be sent once APNs hands it over.
    func start(_ client: MCPClient) async {
        self.client = client
        sent = false
        do {
            let granted = try await UNUserNotificationCenter.current()
                .requestAuthorization(options: [.alert, .sound, .badge])
            if granted { UIApplication.shared.registerForRemoteNotifications() }
        } catch {
            log.error("Notification authorization failed: \(error.localizedDescription)")
        }
        await send()
    }

    /// Keeps the device token APNs gave us (lowercase hex) and sends it if signed in.
    func didRegister(deviceToken: Data) async {
        token = deviceToken.map { String(format: "%02x", $0) }.joined()
        await send()
    }

    /// Logs a failed APNs registration. The error carries no token.
    func didFail(_ error: Error) {
        log.error("APNs registration failed: \(error.localizedDescription)")
    }

    /// Best effort: tells the platform to forget this device. A failure is only logged.
    func unregister(_ client: MCPClient) async {
        do {
            _ = try await client.call("apns_unregister", ["app": "pigeon"])
        } catch {
            log.error("Push unregister failed: \(error.localizedDescription)")
        }
        sent = false
    }

    /// Calls `apns_register` once per sign-in, when both the client and the token are known.
    private func send() async {
        guard !sent, let client, let token else { return }
        sent = true
        do {
            _ = try await client.call(
                "apns_register",
                ["app": "pigeon", "token": token, "environment": Push.environment])
        } catch {
            sent = false
            log.error("Push register failed: \(error.localizedDescription)")
        }
    }

    /// The APNs environment: the embedded profile's `aps-environment`, else by build type.
    static var environment: String {
        if let value = profileEnvironment(), value == "development" || value == "production" {
            return value
        }
        #if DEBUG
            return "development"
        #else
            return "production"
        #endif
    }

    /// Reads `aps-environment` from `embedded.mobileprovision`, or nil when there is none.
    private static func profileEnvironment() -> String? {
        guard let url = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision"),
            let data = try? Data(contentsOf: url),
            let start = data.range(of: Data("<?xml".utf8)),
            let end = data.range(of: Data("</plist>".utf8)),
            start.lowerBound < end.upperBound,
            let plist = try? PropertyListSerialization.propertyList(
                from: data.subdata(in: start.lowerBound..<end.upperBound), format: nil)
                as? [String: Any],
            let entitlements = plist["Entitlements"] as? [String: Any]
        else { return nil }
        return entitlements["aps-environment"] as? String
    }
}

/// Receives the APNs callbacks and shows notifications while the app is open.
final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    /// Becomes the notification center's delegate.
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        return true
    }

    /// Passes the device token to `Push`.
    func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        Task { @MainActor in await Push.shared.didRegister(deviceToken: deviceToken) }
    }

    /// Logs the failure.
    func application(
        _ application: UIApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        Task { @MainActor in Push.shared.didFail(error) }
    }

    /// Shows a banner, sound and badge while the app is in the foreground.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .sound, .badge]
    }

    /// A tap brings the app forward, which is all it does for now.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse
    ) async {}
}
