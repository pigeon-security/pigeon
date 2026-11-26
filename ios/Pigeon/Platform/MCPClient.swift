import Foundation

/// JSON-RPC 2.0 over platform.pigeon.dev/mcp, with the stored bearer token.
@MainActor
final class MCPClient {
    private let url = OAuth.base.appendingPathComponent("mcp")
    private var initialized = false
    private var nextId = 1
    private var sessionId: String?
    private let onSignedOut: @MainActor () -> Void

    /// A client that calls `onSignedOut` when the platform refuses the tokens for good.
    init(onSignedOut: @escaping @MainActor () -> Void) {
        self.onSignedOut = onSignedOut
    }

    /// Calls the MCP tool `name` and answers its structured content, or the JSON in its text.
    /// The app's own screens read through `ConsoleAPI.op` instead; this stays for the device
    /// registration (`apns_register`, `apns_unregister`).
    func call(_ name: String, _ arguments: [String: Any] = [:]) async throws -> Any {
        try await toolResult(name, arguments)
    }

    /// Answers structured content, or the JSON in the tool's text.
    private func toolResult(_ name: String, _ arguments: [String: Any]) async throws -> Any {
        try await ensureInitialized()
        let result = try await rpc("tools/call", ["name": name, "arguments": arguments])
        let text = ((result["content"] as? [[String: Any]])?.first?["text"]) as? String
        if result["isError"] as? Bool == true {
            throw PlatformError.server(text ?? "The tool failed.")
        }
        if let structured = result["structuredContent"] { return structured }
        guard let text, let data = text.data(using: .utf8),
            let json = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
        else { throw PlatformError.badResponse("tool result") }
        return json
    }

    private func ensureInitialized() async throws {
        if initialized { return }
        _ = try await rpc(
            "initialize",
            [
                "protocolVersion": "2025-03-26",
                "capabilities": [String: Any](),
                "clientInfo": ["name": "Pigeon for iOS", "version": "0.1.0"],
            ])
        initialized = true
    }

    /// Sends one request. A 401 refreshes the tokens once and retries; a second 401 signs out.
    private func rpc(_ method: String, _ params: [String: Any]) async throws -> [String: Any] {
        let id = nextId
        nextId += 1
        let body = try JSONSerialization.data(
            withJSONObject: [
                "jsonrpc": "2.0", "id": id, "method": method, "params": params,
            ] as [String: Any])
        for attempt in 0..<2 {
            let pair = try await currentPair(attempt: attempt)
            var req = URLRequest(url: url)
            req.httpMethod = "POST"
            req.httpBody = body
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.setValue("application/json", forHTTPHeaderField: "Accept")
            req.setValue("Bearer \(pair.access)", forHTTPHeaderField: "Authorization")
            if let sessionId { req.setValue(sessionId, forHTTPHeaderField: "Mcp-Session-Id") }
            let (data, response) = try await URLSession.shared.data(for: req)
            let http = response as? HTTPURLResponse
            if http?.statusCode == 401 {
                if attempt == 0 {
                    _ = try await refreshing(pair)
                    continue
                }
                break
            }
            if let value = http?.value(forHTTPHeaderField: "Mcp-Session-Id") { sessionId = value }
            guard let http, (200..<300).contains(http.statusCode) else {
                throw PlatformError.server(
                    "The platform answered HTTP \(http?.statusCode ?? 0).")
            }
            guard let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
            else { throw PlatformError.badResponse("not a JSON-RPC answer") }
            if let error = json["error"] as? [String: Any] {
                throw PlatformError.server(error["message"] as? String ?? "The request failed.")
            }
            guard let result = json["result"] as? [String: Any] else {
                throw PlatformError.badResponse("no result")
            }
            return result
        }
        onSignedOut()
        throw PlatformError.signedOut
    }

    /// The stored tokens, refreshed on the first attempt when they are about to end. No tokens
    /// signs out. Shared by the MCP calls and [`send`].
    private func currentPair(attempt: Int) async throws -> TokenPair {
        guard var pair = Keychain.load(TokenPair.self, for: Keychain.Key.tokens) else {
            onSignedOut()
            throw PlatformError.signedOut
        }
        if pair.expiresAt < Date().addingTimeInterval(30), attempt == 0 {
            pair = try await refreshing(pair)
        }
        return pair
    }

    /// POSTs `body` (JSON) to `path` on the platform with the bearer token and answers the
    /// status and the body. A 401 refreshes the tokens once and retries. A second 401 signs
    /// out when `signOutOn401`; otherwise it is answered like any other status, so a route
    /// that does not take a bearer yet does not end the session. `session` is the shared one
    /// unless the caller needs another, such as a secret write's, which never follows a redirect.
    func send(
        _ path: String, body: Data, session: URLSession = .shared, signOutOn401: Bool = true
    ) async throws -> (status: Int, data: Data) {
        guard let url = URL(string: OAuth.base.absoluteString + path) else {
            throw PlatformError.badResponse("\(path): URL")
        }
        var last: (Int, Data) = (401, Data())
        for attempt in 0..<2 {
            let pair = try await currentPair(attempt: attempt)
            var req = URLRequest(url: url)
            req.httpMethod = "POST"
            req.httpBody = body
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.setValue("application/json", forHTTPHeaderField: "Accept")
            req.setValue("Bearer \(pair.access)", forHTTPHeaderField: "Authorization")
            let (data, response) = try await session.data(for: req)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            last = (status, data)
            if status == 401 {
                if attempt == 0 {
                    _ = try await refreshing(pair)
                    continue
                }
                break
            }
            return (status, data)
        }
        if signOutOn401 {
            onSignedOut()
            throw PlatformError.signedOut
        }
        return last
    }

    /// Refreshes the tokens; a refused refresh signs out.
    private func refreshing(_ pair: TokenPair) async throws -> TokenPair {
        do {
            return try await OAuth.refresh(pair)
        } catch PlatformError.server {
            onSignedOut()
            throw PlatformError.signedOut
        }
    }
}
