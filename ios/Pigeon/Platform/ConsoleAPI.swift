import CryptoKit
import Foundation
import Observation

/// What a read or a write failed with, in words a person can act on.
enum ConsoleError: LocalizedError, Equatable {
    /// The session has ended; the app is back at sign-in.
    case signedOut
    /// The platform refused the app's token for this route (401 after a refresh).
    case unauthorized(String)
    /// The platform refused the op or write for this person or app (403).
    case notAllowed(String)
    /// The read's source did not answer in time (504); trying again may work.
    case timedOut(String)
    /// The platform answered an error, with its own text.
    case server(String)
    /// The write's begin step found something wrong with the change.
    case rejected([String])
    /// The request did not reach the platform.
    case network(String)
    /// The platform answered something this app cannot read.
    case badResponse(String)
    /// The person closed the passkey sheet.
    case passkeyCancelled
    /// iOS could not ask for the passkey.
    case passkeyFailed(String)

    var errorDescription: String? {
        switch self {
        case .signedOut: return "You are signed out. Sign in again."
        case .unauthorized(let why): return "The platform did not accept this app's sign-in here: \(why)"
        case .notAllowed(let why): return "Not allowed: \(why)"
        case .timedOut(let why): return "The platform did not answer in time (\(why)). Try again."
        case .server(let why): return why
        case .rejected(let why): return why.joined(separator: " ")
        case .network(let why): return "Couldn't reach the platform: \(why)"
        case .badResponse(let what): return "The platform answered something unexpected: \(what)."
        case .passkeyCancelled: return "The passkey step was cancelled. Nothing changed."
        case .passkeyFailed(let why): return "The passkey step failed: \(why)"
        }
    }

    /// Whether the same request may work if tried again.
    var retryable: Bool {
        switch self {
        case .timedOut, .network, .server, .badResponse: return true
        default: return false
        }
    }
}

/// One line of a change, as the confirm sheet draws it.
struct DiffLine: Hashable, Identifiable {
    enum Kind: Hashable { case add, remove, note }
    let kind: Kind
    let text: String
    var id: String { "\(kind)|\(text)" }

    static func add(_ text: String) -> Self { .init(kind: .add, text: text) }
    static func remove(_ text: String) -> Self { .init(kind: .remove, text: text) }
    static func note(_ text: String) -> Self { .init(kind: .note, text: text) }

    /// The lines for a summary's `change`: a string is a note; `{path, from, to}` is a removal
    /// of `from` and an addition of `to`, as the web's `summaryNode` draws it.
    static func lines(_ change: JSON) -> [DiffLine] {
        change.items.flatMap { c -> [DiffLine] in
            if let s = c.string { return [.note(s)] }
            let path = c["path"].text ?? ""
            var out: [DiffLine] = []
            if !c["from"].isNull { out.append(.remove("\(path): \(c["from"].compactText)")) }
            if !c["to"].isNull { out.append(.add("\(path): \(c["to"].compactText)")) }
            if out.isEmpty { out.append(.note(c.compactText)) }
            return out
        }
    }
}

/// What the platform says a write will do, from its begin step, before the passkey.
struct WriteSummary: Equatable {
    let action: String
    let title: String
    let target: String
    let lines: [DiffLine]
    let destructive: Bool
    /// The whole summary, with whatever the write's check added.
    let raw: JSON

    init(_ json: JSON) {
        action = json["action"].string ?? ""
        title = json["title"].string ?? ""
        target = json["target"].text ?? ""
        let change = json["change"].isNull ? json["changes"] : json["change"]
        lines = DiffLine.lines(change)
        destructive = json["destructive"].bool ?? false
        raw = json
    }
}

/// Something a write shows once: a token or an invite link.
struct Reveal: Equatable, Identifiable {
    let value: String
    let note: String?
    /// `token` or `invite`.
    let kind: String
    var id: String { value }

    /// The line under the value, as the web's `SHOW_ONCE` says it.
    var rule: String {
        kind == "invite"
            ? "Shown once. The link works once; make a new invite if it's lost."
            : "Shown once. After Done it can't be shown again; issue a new token if it's lost."
    }
}

/// What a finished write answered.
struct WriteResult: Equatable {
    let message: String
    let reveal: Reveal?
    let data: JSON
    let raw: JSON

    init(_ json: JSON) {
        message = json["message"].string ?? "Done."
        let r = json["reveal"]
        reveal = r["value"].string.map {
            Reveal(value: $0, note: r["note"].string, kind: r["kind"].string ?? "token")
        }
        data = json["data"]
        raw = json
    }
}

/// The console's one read path and one write path, as the web console's `shell.js` uses them,
/// with the app's OAuth bearer:
///
/// - Read: `POST /app/op` with `{name, args}` answers `{data}` or `{error}` ([`op`]).
/// - Write: `POST /console/write/<action>/begin` with `{target, change}` answers
///   `{summary, challenge_id, options, digest}` (or `{errors}`); the passkey signs
///   `options.challenge`; `POST …/finish` with `{target, change, challenge_id, assertion}`
///   makes the change once and answers `{message, data?, reveal?}` ([`write`]).
///
/// Every screen reads and writes through this; only `ConfirmSheet` calls [`write`].
@MainActor
@Observable
final class ConsoleAPI {
    @ObservationIgnored private let client: MCPClient
    @ObservationIgnored private let passkey = PasskeyPrompt()

    init(client: MCPClient) {
        self.client = client
    }

    // MARK: Read

    /// Runs read op `name` (one of the console's `READS`) with `args`, an object of only the
    /// arguments that op takes, and answers its `data`.
    func op(_ name: String, args: JSON = [:]) async throws -> JSON {
        let body: JSON = ["name": .string(name), "args": args.isNull ? [:] : args]
        let (status, json) = try await post("/app/op", body)
        if (200..<300).contains(status), let data = json.object?["data"] {
            return data
        }
        throw Self.failure(status: status, json: json, what: name)
    }

    /// Runs read op `name` and decodes its `data` as `T`.
    func op<T: Decodable>(_ name: String, args: JSON = [:], as type: T.Type) async throws -> T {
        let data = try await op(name, args: args)
        do {
            return try data.decode(T.self)
        } catch {
            throw ConsoleError.badResponse("\(name): \(Self.describe(error))")
        }
    }

    // MARK: Write

    /// Makes write `action` (one of the console's `WRITES`) on `target` with `change`: begin,
    /// the passkey, then finish. `onSummary` gets what the platform says it will do, before
    /// the passkey sheet. `beginChange` is sent to begin instead of `change` when they differ.
    ///
    /// A `secret-set` change is `{value}`: begin gets only the value's digest, the value is
    /// sent once, at finish, over a session that never follows a redirect, and no error
    /// carries the platform's text, which could echo it.
    func write(
        action: String, target: JSON, change: JSON = .null, beginChange: JSON? = nil,
        onSummary: (WriteSummary) -> Void = { _ in }
    ) async throws -> WriteResult {
        let secret = action == "secret-set"
        var change = change
        var beginChange = beginChange
        var session = URLSession.shared
        if secret {
            let value = (change["value"].string ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            guard !value.isEmpty else { throw ConsoleError.rejected(["Enter a value."]) }
            change = ["value": .string(value)]
            beginChange = ["digest": .string(Base64URL.encode(Data(SHA256.hash(data: Data(value.utf8)))))]
            session = URLSession(
                configuration: .ephemeral, delegate: NoRedirects(), delegateQueue: nil)
        }
        defer { if secret { session.finishTasksAndInvalidate() } }
        let path = "/console/write/\(action)"

        let begin: JSON
        do {
            let (status, json) = try await post(
                "\(path)/begin", ["target": target, "change": beginChange ?? change],
                session: session)
            guard (200..<300).contains(status), json["error"].isNull else {
                throw Self.failure(status: status, json: json, what: action)
            }
            begin = json
        } catch let error as ConsoleError where secret {
            throw Self.quiet(error, step: "begin")
        }
        if !begin["errors"].isNull {
            let errors = begin["errors"]
            let list = errors.array ?? [errors]
            throw ConsoleError.rejected(list.map { $0["message"].string ?? $0.text ?? $0.compactText })
        }
        onSummary(WriteSummary(begin["summary"]))
        guard let challengeId = begin["challenge_id"].string else {
            throw ConsoleError.badResponse("\(action) begin: no challenge")
        }

        let assertion: JSON
        do {
            assertion = try await passkey.assertion(options: begin["options"])
        } catch PasskeyError.cancelled {
            throw ConsoleError.passkeyCancelled
        } catch PasskeyError.badOptions(let why) {
            throw ConsoleError.badResponse("\(action) begin: \(why)")
        } catch PasskeyError.failed(let domain, let code, let text) {
            throw ConsoleError.passkeyFailed("\(text) (\(domain) \(code))")
        }
        try Task.checkCancellation()

        do {
            let (status, json) = try await post(
                "\(path)/finish",
                [
                    "target": target, "change": change,
                    "challenge_id": .string(challengeId), "assertion": assertion,
                ],
                session: session)
            guard (200..<300).contains(status), json["error"].isNull else {
                throw Self.failure(status: status, json: json, what: action)
            }
            return WriteResult(json)
        } catch let error as ConsoleError where secret {
            throw Self.quiet(error, step: "finish")
        }
    }

    // MARK: Transport

    /// POSTs `body` and answers the status and the JSON body (`.null` when it is not JSON).
    private func post(_ path: String, _ body: JSON, session: URLSession = .shared) async throws
        -> (Int, JSON)
    {
        let payload: Data
        do {
            payload = try JSONEncoder().encode(body)
        } catch {
            throw ConsoleError.badResponse("the request could not be written")
        }
        do {
            let (status, data) = try await client.send(
                path, body: payload, session: session, signOutOn401: false)
            let json = (try? JSONDecoder().decode(JSON.self, from: data)) ?? .null
            return (status, json)
        } catch PlatformError.signedOut {
            throw ConsoleError.signedOut
        } catch let error as URLError where error.code == .cancelled {
            throw CancellationError()
        } catch let error as URLError {
            throw ConsoleError.network(error.localizedDescription)
        } catch is CancellationError {
            throw CancellationError()
        } catch let error as PlatformError {
            throw ConsoleError.server(error.localizedDescription)
        }
    }

    /// The error for a failed answer, with the platform's own `error` text.
    private static func failure(status: Int, json: JSON, what: String) -> ConsoleError {
        let why = json["error"].string ?? "\(what): the platform answered HTTP \(status)"
        switch status {
        case 401: return .unauthorized(why)
        case 403: return .notAllowed(why)
        case 504: return .timedOut(why)
        case 200..<300: return .badResponse("\(what): \(why)")
        default: return json["retry"].bool == true ? .timedOut(why) : .server(why)
        }
    }

    /// A secret write's error without the platform's text.
    private static func quiet(_ error: ConsoleError, step: String) -> ConsoleError {
        switch error {
        case .signedOut, .passkeyCancelled, .passkeyFailed, .network: return error
        case .unauthorized: return .unauthorized("the secret write (\(step)) was refused")
        case .notAllowed: return .notAllowed("the secret write (\(step)) was refused")
        default: return .server("The secret write failed at \(step). Nothing was changed.")
        }
    }

    private static func describe(_ error: Error) -> String {
        guard let e = error as? DecodingError else { return error.localizedDescription }
        switch e {
        case .keyNotFound(let key, let c):
            return "no `\(key.stringValue)` at \(path(c.codingPath))"
        case .typeMismatch(_, let c), .valueNotFound(_, let c), .dataCorrupted(let c):
            return "unexpected value at \(path(c.codingPath))"
        @unknown default:
            return "unexpected shape"
        }
    }

    private static func path(_ keys: [CodingKey]) -> String {
        keys.isEmpty ? "the top" : keys.map { key in key.intValue.map { String($0) } ?? key.stringValue }.joined(separator: ".")
    }
}

/// Never forwards a request to a redirected URL (a secret's value must reach only the platform).
private final class NoRedirects: NSObject, URLSessionTaskDelegate {
    func urlSession(
        _ session: URLSession, task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void
    ) {
        completionHandler(nil)
    }
}
