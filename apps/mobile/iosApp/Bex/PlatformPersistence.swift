import AgentCore
import Foundation
import Security

/// Where each Host's state lives; core keeps the state file written. The model
/// preferences every Host shares stay in the app's defaults.
enum SnapshotFiles {
    private static func location(_ host: String) throws -> URL {
        let directory = try FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true
        ).appendingPathComponent("orchestration", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let name = Data(host.utf8).base64EncodedString()
            .replacingOccurrences(of: "/", with: "_")
        return directory.appendingPathComponent(name).appendingPathExtension("json")
    }

    static func stateFile(_ host: String) throws -> String {
        try location(host).path
    }

    static func cacheDirectory(_ host: String) throws -> String {
        let directory = try location(host).deletingPathExtension().appendingPathExtension("cache")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory.path
    }

    static func diagnosticsDirectory(_ host: String) throws -> String {
        try location(host).deletingPathExtension().appendingPathExtension("diagnostics").path
    }

    static func modelDefaults() -> Data {
        UserDefaults.standard.data(forKey: "bex.orchestration-model-defaults") ?? Data()
    }

    static func saveModelPreferences(_ data: Data) async throws {
        try await Task.detached(priority: .utility) {
            UserDefaults.standard.set(data, forKey: "bex.orchestration-model-defaults")
        }.value
    }
}

enum DeviceIdentity {
    static func remove(_ reference: String) throws {
        let status = SecItemDelete([
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "app.bex.iroh.identity",
            kSecAttrAccount as String: reference
        ] as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(status))
        }
    }

    static func loadOrGenerate(_ reference: String) throws -> Data {
        let query: [String: CFTypeRef] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "app.bex.iroh.identity" as CFString,
            kSecAttrAccount as String: reference as CFString,
            kSecReturnData as String: kCFBooleanTrue!
        ]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecSuccess, let data = result as? Data {
            return data
        }
        guard status == errSecItemNotFound else { throw NSError(domain: NSOSStatusErrorDomain, code: Int(status)) }
        let data = generateIdentity()
        var record = query
        record.removeValue(forKey: kSecReturnData as String)
        record[kSecValueData as String] = data as CFData
        record[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let saved = SecItemAdd(record as CFDictionary, nil)
        guard saved == errSecSuccess else { throw NSError(domain: NSOSStatusErrorDomain, code: Int(saved)) }
        return data
    }
}

/// Runs `body` with a fresh private temporary directory and removes the
/// directory when `body` fails, so partial files never outlive an error.
func inTemporaryDirectory<T>(_ body: (URL) throws -> T) throws -> T {
    let directory = try makeTemporaryDirectory()
    do {
        return try body(directory)
    } catch {
        try? FileManager.default.removeItem(at: directory)
        throw error
    }
}

func inTemporaryDirectory<T>(_ body: (URL) async throws -> T) async throws -> T {
    let directory = try makeTemporaryDirectory()
    do {
        return try await body(directory)
    } catch {
        try? FileManager.default.removeItem(at: directory)
        throw error
    }
}

private func makeTemporaryDirectory() throws -> URL {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    return directory
}
