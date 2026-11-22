import AuthenticationServices
import CryptoKit
import Foundation
import UIKit

/// What can go wrong talking to the platform, with a message a person can read.
enum PlatformError: LocalizedError {
    case badResponse(String)
    case server(String)
    case cancelled(String)
    case stateMismatch
    case signedOut
    case keychain(OSStatus)

    var errorDescription: String? {
        switch self {
        case .badResponse(let what): return "The platform answered something unexpected: \(what)."
        case .server(let text): return text
        case .cancelled(let code):
            return "Sign-in was closed before it finished (\(code)). If you did not close it, the platform's link to this app may not be ready yet: try again in a few minutes."
        case .stateMismatch: return "Sign-in came back with the wrong state. Try again."
        case .signedOut: return "You are signed out. Sign in again."
        case .keychain(let status): return "The keychain refused the request (\(status))."
        }
    }
}

/// OAuth with platform.pigeon.dev, as any MCP client does it: register once, PKCE, code exchange.
enum OAuth {
    static let host = "platform.pigeon.dev"
    static let base = URL(string: "https://platform.pigeon.dev")!
    static let redirect = "https://platform.pigeon.dev/app/oauth"

    /// Runs the whole sign-in and stores the client id and tokens in the keychain.
    @MainActor
    static func signIn() async throws -> TokenPair {
        let clientId = try await registeredClient()
        let verifier = randomString(32)
        let state = randomString(16)
        var parts = URLComponents(
            url: base.appendingPathComponent("oauth/authorize"), resolvingAgainstBaseURL: false)
        parts?.queryItems = [
            URLQueryItem(name: "response_type", value: "code"),
            URLQueryItem(name: "client_id", value: clientId),
            URLQueryItem(name: "redirect_uri", value: redirect),
            URLQueryItem(name: "code_challenge", value: challenge(verifier)),
            URLQueryItem(name: "code_challenge_method", value: "S256"),
            URLQueryItem(name: "state", value: state),
        ]
        guard let url = parts?.url else { throw PlatformError.badResponse("authorize URL") }
        let callback = try await WebAuth().run(url: url)
        let items = URLComponents(url: callback, resolvingAgainstBaseURL: false)?.queryItems ?? []
        func item(_ name: String) -> String? { items.first { $0.name == name }?.value }
        if let error = item("error") {
            throw PlatformError.server(item("error_description") ?? error)
        }
        guard item("state") == state else { throw PlatformError.stateMismatch }
        guard let code = item("code") else {
            throw PlatformError.badResponse("no code in the callback")
        }
        let pair = try await token([
            "grant_type": "authorization_code", "code": code, "redirect_uri": redirect,
            "client_id": clientId, "code_verifier": verifier,
        ])
        try Keychain.save(pair, for: Keychain.Key.tokens)
        return pair
    }

    /// Trades the refresh token for a new pair and stores it.
    static func refresh(_ old: TokenPair) async throws -> TokenPair {
        guard let clientId = Keychain.load(String.self, for: Keychain.Key.clientId) else {
            throw PlatformError.signedOut
        }
        let pair = try await token([
            "grant_type": "refresh_token", "refresh_token": old.refresh, "client_id": clientId,
        ])
        try Keychain.save(pair, for: Keychain.Key.tokens)
        return pair
    }

    /// The stored client id, or a new registration.
    private static func registeredClient() async throws -> String {
        if let id = Keychain.load(String.self, for: Keychain.Key.clientId) { return id }
        var req = URLRequest(url: base.appendingPathComponent("oauth/register"))
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try JSONSerialization.data(withJSONObject: [
            "client_name": "Pigeon for iOS", "redirect_uris": [redirect],
        ] as [String: Any])
        let json = try await send(req)
        guard let id = json["client_id"] as? String else {
            throw PlatformError.badResponse("no client_id")
        }
        try Keychain.save(id, for: Keychain.Key.clientId)
        return id
    }

    /// Posts a form to the token endpoint and reads the token pair.
    private static func token(_ fields: [String: String]) async throws -> TokenPair {
        var req = URLRequest(url: base.appendingPathComponent("oauth/token"))
        req.httpMethod = "POST"
        req.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        var form = URLComponents()
        form.queryItems = fields.map { URLQueryItem(name: $0.key, value: $0.value) }
        // `+` is not escaped by URLComponents in a query, and a form reads it as a space.
        req.httpBody = form.percentEncodedQuery?
            .replacingOccurrences(of: "+", with: "%2B").data(using: .utf8)
        let json = try await send(req)
        guard let access = json["access_token"] as? String,
            let refresh = json["refresh_token"] as? String,
            let seconds = json["expires_in"] as? Double
        else { throw PlatformError.badResponse("token response") }
        return TokenPair(
            access: access, refresh: refresh, expiresAt: Date().addingTimeInterval(seconds))
    }

    /// Sends `req` and reads a JSON object, turning an OAuth error body into a thrown error.
    private static func send(_ req: URLRequest) async throws -> [String: Any] {
        let (data, response) = try await URLSession.shared.data(for: req)
        let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else {
            let text = json?["error_description"] as? String ?? json?["error"] as? String
            throw PlatformError.server(text ?? "The platform answered HTTP \(status).")
        }
        guard let json else { throw PlatformError.badResponse("not JSON") }
        return json
    }

    /// A random URL-safe string from `count` random bytes.
    private static func randomString(_ count: Int) -> String {
        base64url(Data((0..<count).map { _ in UInt8.random(in: .min ... .max) }))
    }

    /// The S256 challenge for `verifier`.
    private static func challenge(_ verifier: String) -> String {
        base64url(Data(SHA256.hash(data: Data(verifier.utf8))))
    }

    private static func base64url(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

/// One web authentication session, shown over the app's window.
@MainActor
final class WebAuth: NSObject, ASWebAuthenticationPresentationContextProviding {
    /// Opens `url` and answers the callback URL.
    func run(url: URL) async throws -> URL {
        try await withCheckedThrowingContinuation { continuation in
            let session = ASWebAuthenticationSession(
                url: url, callback: .https(host: OAuth.host, path: "/app/oauth")
            ) { callback, error in
                if let callback {
                    continuation.resume(returning: callback)
                } else {
                    continuation.resume(throwing: WebAuth.explain(error))
                }
            }
            session.prefersEphemeralWebBrowserSession = false
            session.presentationContextProvider = self
            if !session.start() {
                continuation.resume(
                    throwing: PlatformError.badResponse("the sign-in page did not open"))
            }
        }
    }

    /// A readable error for what the session answered: never silent, always with iOS's code.
    nonisolated static func explain(_ error: Error?) -> Error {
        guard let error else {
            return PlatformError.badResponse("the sign-in sheet closed without an answer")
        }
        let ns = error as NSError
        NSLog("Pigeon sign-in: %@ %ld %@", ns.domain, ns.code, ns.userInfo.description)
        if let e = error as? ASWebAuthenticationSessionError {
            switch e.code {
            case .canceledLogin:
                return PlatformError.cancelled("\(ns.domain) \(ns.code)")
            case .presentationContextNotProvided, .presentationContextInvalid:
                return PlatformError.server(
                    "iOS could not show the sign-in sheet (ASWebAuthenticationSession \(ns.code)).")
            @unknown default:
                break
            }
        }
        return PlatformError.server(
            "Sign-in stopped: \(ns.localizedDescription) (\(ns.domain) \(ns.code)).")
    }

    /// The app's own window, so the sheet is shown over it. A new window that belongs to no
    /// scene is not a valid anchor, and iOS then refuses the session.
    nonisolated func presentationAnchor(for session: ASWebAuthenticationSession)
        -> ASPresentationAnchor
    {
        MainActor.assumeIsolated {
            let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            let scene = scenes.first { $0.activationState == .foregroundActive } ?? scenes.first
            if let window = scene?.keyWindow ?? scene?.windows.first { return window }
            if let scene { return UIWindow(windowScene: scene) }
            return ASPresentationAnchor()
        }
    }
}
