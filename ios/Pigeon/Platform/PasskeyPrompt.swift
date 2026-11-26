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

/// What can go wrong asking for the person's passkey.
enum PasskeyError: Error {
    /// The person closed the passkey sheet.
    case cancelled
    /// The begin step's options were not a challenge this app can answer.
    case badOptions(String)
    /// iOS refused the request; the domain and code say why.
    case failed(domain: String, code: Int, text: String)
}

/// The passkey step of every console write: asks the person for an assertion over the begin
/// step's challenge, from this device or a security key, as the web's `pigeonPasskey.approve`
/// does. Only `ConsoleAPI.write` calls it.
@MainActor
final class PasskeyPrompt: NSObject, ASAuthorizationControllerDelegate,
    ASAuthorizationControllerPresentationContextProviding
{
    private var continuation: CheckedContinuation<ASAuthorization, Error>?
    private var controller: ASAuthorizationController?

    /// The assertion for the begin step's `options` (`{challenge, rpId, allow: [id]}`, all
    /// base64url), as finish takes it: `{id, clientDataJSON, authenticatorData, signature}`.
    func assertion(options: JSON) async throws -> JSON {
        guard let challengeText = options["challenge"].string,
            let challenge = Base64URL.decode(challengeText),
            let rpId = options["rpId"].string
        else { throw PasskeyError.badOptions("no challenge") }
        let allowText = options["allow"].items.compactMap(\.string)
        let allow = allowText.compactMap(Base64URL.decode)
        if allow.count != allowText.count {
            throw PasskeyError.badOptions("a passkey id is not base64url")
        }
        let credential = try await assert(challenge: challenge, rpId: rpId, allow: allow)
        return [
            "id": .string(Base64URL.encode(credential.credentialID)),
            "clientDataJSON": .string(Base64URL.encode(credential.rawClientDataJSON)),
            "authenticatorData": .string(Base64URL.encode(credential.rawAuthenticatorData)),
            "signature": .string(Base64URL.encode(credential.signature)),
        ]
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
        else { throw PasskeyError.badOptions("the passkey answer was not an assertion") }
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
        NSLog("Pigeon passkey: %@ %ld", ns.domain, ns.code)
        let failure: PasskeyError =
            (error as? ASAuthorizationError)?.code == .canceled
            ? .cancelled
            : .failed(domain: ns.domain, code: ns.code, text: ns.localizedDescription)
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
