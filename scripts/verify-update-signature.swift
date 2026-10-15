#!/usr/bin/env swift
// Independent public-key verification of a Sparkle archive or signed appcast.
// No Keychain access; no private keys; the feed bytes are never reserialized.
import CryptoKit
import Foundation

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}

let arguments = CommandLine.arguments
guard arguments.count == 3 || arguments.count == 4,
      let keyData = Data(base64Encoded: arguments[1]), keyData.count == 32 else {
    fail("Usage: verify-update-signature.swift PUBLIC_KEY FILE [ARCHIVE_SIGNATURE]")
}
do {
    let key = try Curve25519.Signing.PublicKey(rawRepresentation: keyData)
    let data = try Data(contentsOf: URL(fileURLWithPath: arguments[2]))
    let signedData: Data
    let signature: Data
    if arguments.count == 4 {
        signedData = data
        guard let value = Data(base64Encoded: arguments[3]), value.count == 64 else { fail("Invalid archive signature") }
        signature = value
    } else {
        let marker = Data("<!-- sparkle-signatures:\n".utf8)
        guard let range = data.range(of: marker, options: .backwards),
              let suffix = data.range(of: Data("-->".utf8), in: range.upperBound..<data.count),
              let block = String(data: data[range.upperBound..<suffix.lowerBound], encoding: .utf8),
              let trailer = String(data: data[suffix.upperBound...], encoding: .utf8),
              trailer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            fail("Missing or malformed signed appcast block")
        }
        let fields = block.split(separator: "\n").map { $0.split(separator: ":", maxSplits: 1).map(String.init) }
        let signatures = fields.filter { $0.count == 2 && $0[0] == "edSignature" }
        let lengths = fields.filter { $0.count == 2 && $0[0] == "length" }
        signedData = data[..<range.lowerBound]
        guard signatures.count == 1, lengths.count == 1,
              let length = Int(lengths[0][1].trimmingCharacters(in: .whitespaces)), length == signedData.count,
              let value = Data(base64Encoded: signatures[0][1].trimmingCharacters(in: .whitespaces)), value.count == 64 else {
            fail("Invalid signed appcast length or signature")
        }
        signature = value
    }
    guard key.isValidSignature(signature, for: signedData) else { fail("Ed25519 verification failed") }
    print("Verified Ed25519 signature:", URL(fileURLWithPath: arguments[2]).lastPathComponent)
} catch { fail(error.localizedDescription) }
