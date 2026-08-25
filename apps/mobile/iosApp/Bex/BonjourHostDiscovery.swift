import Foundation
import Network
import Darwin

struct BexBonjourHost: Hashable, Identifiable {
    let name: String
    let type: String
    let domain: String
    let interfaceName: String?

    var id: String { "\(name)|\(type)|\(domain)|\(interfaceName ?? "")" }
}

/// Discovers only Bex's TCP service. Resolving and authenticating a result is
/// the transport adapter's job; a Bonjour name is never proof of Host identity.
final class BexBonjourHostDiscovery {
    private var browser: NWBrowser?
    private(set) var hosts: [BexBonjourHost] = [] {
        didSet { onHostsChanged?(hosts) }
    }
    var onHostsChanged: (([BexBonjourHost]) -> Void)?
    var onAddressesChanged: (([String]) -> Void)?
    var onStateChanged: ((NWBrowser.State) -> Void)?
    private var resolvers: [String: BexBonjourHostResolver] = [:]
    private var addressesByHost: [String: Set<String>] = [:]

    func start() {
        guard browser == nil else { return }
        let browser = NWBrowser(for: .bonjour(type: "_bex._tcp", domain: nil), using: .tcp)
        browser.stateUpdateHandler = { [weak self] state in
            DispatchQueue.main.async { self?.onStateChanged?(state) }
        }
        browser.browseResultsChangedHandler = { [weak self] results, _ in
            let hosts = results.compactMap(Self.host(from:)).sorted { $0.id < $1.id }
            DispatchQueue.main.async {
                self?.hosts = hosts
                self?.resolve(hosts)
            }
        }
        self.browser = browser
        browser.start(queue: .main)
    }

    func stop() {
        browser?.cancel()
        browser = nil
        resolvers.values.forEach { $0.stop() }
        resolvers = [:]
        addressesByHost = [:]
        hosts = []
        onAddressesChanged?([])
    }

    private static func host(from result: NWBrowser.Result) -> BexBonjourHost? {
        guard case let .service(name, type, domain, interface) = result.endpoint else { return nil }
        return BexBonjourHost(
            name: name,
            type: type,
            domain: domain,
            interfaceName: interface?.name
        )
    }

    private func resolve(_ hosts: [BexBonjourHost]) {
        let ids = Set(hosts.map(\.id))
        for (id, resolver) in resolvers where !ids.contains(id) {
            resolver.stop()
            resolvers[id] = nil
            addressesByHost[id] = nil
        }
        for host in hosts where resolvers[host.id] == nil {
            let resolver = BexBonjourHostResolver(host: host) { [weak self] addresses in
                guard let self else { return }
                self.addressesByHost[host.id] = Set(addresses)
                self.onAddressesChanged?(self.addressesByHost.values.flatMap(Array.init).sorted())
            }
            resolvers[host.id] = resolver
            resolver.start()
        }
    }
}

private final class BexBonjourHostResolver: NSObject, NetServiceDelegate {
    private let service: NetService
    private let completion: ([String]) -> Void

    init(host: BexBonjourHost, completion: @escaping ([String]) -> Void) {
        let type = host.type.hasSuffix(".") ? host.type : "\(host.type)."
        service = NetService(domain: host.domain, type: type, name: host.name)
        self.completion = completion
        super.init()
        service.delegate = self
    }

    func start() {
        service.schedule(in: .main, forMode: .default)
        service.resolve(withTimeout: 5)
    }

    func stop() {
        service.stop()
        service.remove(from: .main, forMode: .default)
    }

    func netServiceDidResolveAddress(_ sender: NetService) {
        completion((sender.addresses ?? []).compactMap(Self.socketAddress))
    }

    func netService(_ sender: NetService, didNotResolve errorDict: [String: NSNumber]) {
        completion([])
    }

    private static func socketAddress(_ data: Data) -> String? {
        data.withUnsafeBytes { raw in
            guard let address = raw.baseAddress?.assumingMemoryBound(to: sockaddr.self) else { return nil }
            switch Int32(address.pointee.sa_family) {
            case AF_INET:
                let v4 = address.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { $0.pointee }
                var buffer = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
                guard inet_ntop(AF_INET, [v4.sin_addr], &buffer, socklen_t(buffer.count)) != nil else { return nil }
                return "\(String(cString: buffer)):\(UInt16(bigEndian: v4.sin_port))"
            case AF_INET6:
                let v6 = address.withMemoryRebound(to: sockaddr_in6.self, capacity: 1) { $0.pointee }
                var buffer = [CChar](repeating: 0, count: Int(INET6_ADDRSTRLEN))
                guard inet_ntop(AF_INET6, [v6.sin6_addr], &buffer, socklen_t(buffer.count)) != nil else { return nil }
                return "[\(String(cString: buffer))]:\(UInt16(bigEndian: v6.sin6_port))"
            default:
                return nil
            }
        }
    }
}
