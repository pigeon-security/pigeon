import Foundation

/// JSON-RPC 2.0 over platform.pigeon.dev/mcp, with the stored bearer token.
@MainActor
final class MCPClient {
    private let url = OAuth.base.appendingPathComponent("mcp")
    private var initialized = false
    private var nextId = 1
    private var sessionId: String?
    private var lastCost: (date: Date, payload: [String: Any])?
    private var costGeneration = 0
    private let onSignedOut: @MainActor () -> Void

    /// A client that calls `onSignedOut` when the platform refuses the tokens for good.
    init(onSignedOut: @escaping @MainActor () -> Void) {
        self.onSignedOut = onSignedOut
    }

    /// Calls the tool and remembers the latest Cost payload for the grant notices.
    func call(_ name: String, _ arguments: [String: Any] = [:]) async throws -> Any {
        let started = Date()
        let generation = costGeneration
        let data = try await toolResult(name, arguments)
        if name == "cost_daily", generation == costGeneration, !Task.isCancelled,
            let payload = data as? [String: Any], lastCost.map({ $0.date <= started }) ?? true
        {
            lastCost = (started, payload)
        }
        return data
    }

    /// Reads Cost once for notices, reusing a recent payload from any range on detail opening.
    func grantCost(reuse: Bool) async throws -> Any {
        let now = Date()
        var utc = Calendar(identifier: .gregorian)
        utc.timeZone = .gmt
        if reuse, let lastCost, now.timeIntervalSince(lastCost.date) < 300,
            utc.isDate(lastCost.date, inSameDayAs: now),
            (lastCost.payload["credits"] as? [String: Any])?["grants"] is [[String: Any]]
        {
            return lastCost.payload
        }
        return try await call("cost_daily", ["range": "today"])
    }

    /// Drops shared Cost data and prevents in-flight requests from repopulating it on sign-out.
    func forgetCost() {
        costGeneration += 1
        lastCost = nil
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
    /// signs out. Shared by the MCP calls and [`post`].
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

    /// POSTs `body` as JSON to `path` on the platform (such as `/console/write/x/begin`) with
    /// the bearer token, and answers the JSON object. A 401 refreshes the tokens once and
    /// retries. Any other failure is thrown with `step` and the platform's own `error` text.
    func post(_ path: String, _ body: [String: Any], step: String) async throws -> [String: Any] {
        guard let url = URL(string: OAuth.base.absoluteString + path) else {
            throw PlatformError.badResponse("\(step): URL")
        }
        let payload = try JSONSerialization.data(withJSONObject: body)
        for attempt in 0..<2 {
            let pair = try await currentPair(attempt: attempt)
            var req = URLRequest(url: url)
            req.httpMethod = "POST"
            req.httpBody = payload
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.setValue("application/json", forHTTPHeaderField: "Accept")
            req.setValue("Bearer \(pair.access)", forHTTPHeaderField: "Authorization")
            let (data, response) = try await URLSession.shared.data(for: req)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            if status == 401 {
                if attempt == 0 {
                    _ = try await refreshing(pair)
                    continue
                }
                break
            }
            let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
            guard (200..<300).contains(status) else {
                let why = json?["error"] as? String ?? "no error text"
                throw PlatformError.server("\(step): HTTP \(status): \(why)")
            }
            guard let json else { throw PlatformError.badResponse("\(step): not a JSON object") }
            return json
        }
        onSignedOut()
        throw PlatformError.signedOut
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
