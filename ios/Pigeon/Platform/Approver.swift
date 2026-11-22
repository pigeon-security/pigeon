import AuthenticationServices
import Foundation
import UIKit

/// Base64url without padding, as the platform's passkey answers are written.
enum Base64URL {
    /// The text for `data`.
    static func encode(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    /// The bytes for `text`, or nil when it is not base64url.
    static func decode(_ text: String) -> Data? {
        var s = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while s.count % 4 != 0 { s += "=" }
        return Data(base64Encoded: s)
    }
}

/// Approves or denies one waiting item with the person's passkey, through the console's write
/// endpoints: `begin` gives a challenge, the passkey signs it, `finish` makes the change.
@MainActor
final class Approver: NSObject, ASAuthorizationControllerDelegate,
    ASAuthorizationControllerPresentationContextProviding
{
    private var continuation: CheckedContinuation<ASAuthorization, Error>?
    private var controller: ASAuthorizationController?

    /// Runs write `action` (such as `approval-approve`) on `target` and answers the platform's
    /// `message`. Throws with the step that failed.
    func run(action: String, target: String, client: MCPClient) async throws -> String {
        let base = "/console/write/\(action)"
        let begin = try await client.post(
            "\(base)/begin", ["target": target], step: "\(action) begin")
        guard let challengeId = begin["challenge_id"] as? String,
            let options = begin["options"] as? [String: Any],
            let challengeText = options["challenge"] as? String,
            let challenge = Base64URL.decode(challengeText),
            let rpId = options["rpId"] as? String
        else { throw PlatformError.badResponse("\(action) begin: no challenge") }
        let allowText = options["allow"] as? [String] ?? []
        let allow = allowText.compactMap(Base64URL.decode)
        if allow.count != allowText.count {
            throw PlatformError.badResponse("\(action) begin: a passkey id is not base64url")
        }
        let credential = try await assert(challenge: challenge, rpId: rpId, allow: allow)
        let finish = try await client.post(
            "\(base)/finish",
            [
                "target": target, "challenge_id": challengeId,
                "assertion": [
                    "id": Base64URL.encode(credential.credentialID),
                    "clientDataJSON": Base64URL.encode(credential.rawClientDataJSON),
                    "authenticatorData": Base64URL.encode(credential.rawAuthenticatorData),
                    "signature": Base64URL.encode(credential.signature),
                ] as [String: Any],
            ], step: "\(action) finish")
        guard let message = finish["message"] as? String else {
            throw PlatformError.badResponse("\(action) finish: no message")
        }
        return message
    }

    /// Asks the person for a passkey assertion over `challenge`, from this device or a security key.
    private func assert(challenge: Data, rpId: String, allow: [Data]) async throws
        -> ASAuthorizationPublicKeyCredentialAssertion
    {
        let platform = ASAuthorizationPlatformPublicKeyCredentialProvider(
            relyingPartyIdentifier: rpId
        ).createCredentialAssertionRequest(challenge: challenge)
        platform.allowedCredentials = allow.map {
            ASAuthorizationPlatformPublicKeyCredentialDescriptor(credentialID: $0)
        }
        platform.userVerificationPreference = .required
        let key = ASAuthorizationSecurityKeyPublicKeyCredentialProvider(
            relyingPartyIdentifier: rpId
        ).createCredentialAssertionRequest(challenge: challenge)
        key.allowedCredentials = allow.map {
            ASAuthorizationSecurityKeyPublicKeyCredentialDescriptor(
                credentialID: $0,
                transports: ASAuthorizationSecurityKeyPublicKeyCredentialDescriptor.Transport
                    .allSupported)
        }
        key.userVerificationPreference = .required
        let controller = ASAuthorizationController(authorizationRequests: [platform, key])
        controller.delegate = self
        controller.presentationContextProvider = self
        self.controller = controller
        let result: ASAuthorization = try await withCheckedThrowingContinuation { continuation in
            self.continuation = continuation
            controller.performRequests()
        }
        self.controller = nil
        guard let assertion = result.credential as? ASAuthorizationPublicKeyCredentialAssertion
        else { throw PlatformError.badResponse("the passkey answer was not an assertion") }
        return assertion
    }

    nonisolated func authorizationController(
        controller: ASAuthorizationController,
        didCompleteWithAuthorization authorization: ASAuthorization
    ) {
        MainActor.assumeIsolated {
            continuation?.resume(returning: authorization)
            continuation = nil
        }
    }

    nonisolated func authorizationController(
        controller: ASAuthorizationController, didCompleteWithError error: Error
    ) {
        let ns = error as NSError
        NSLog("Pigeon passkey: %@ %ld %@", ns.domain, ns.code, ns.userInfo.description)
        let failure = PlatformError.server(
            "The passkey step failed: \(ns.localizedDescription) (\(ns.domain) \(ns.code)).")
        MainActor.assumeIsolated {
            continuation?.resume(throwing: failure)
            continuation = nil
            self.controller = nil
        }
    }

    nonisolated func presentationAnchor(for controller: ASAuthorizationController)
        -> ASPresentationAnchor
    {
        MainActor.assumeIsolated { WebAuth.appWindow() }
    }
}
