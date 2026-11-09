import Foundation
import Observation

/// A j3 server the user added.
struct SavedHost: Codable, Identifiable, Hashable, Sendable {
    var id: UUID
    /// Shown in lists; the server's own name when the user gave none.
    var name: String
    /// The server's HTTP base, e.g. `http://shieldy:3780`.
    var url: String
    var enabled: Bool

    init(id: UUID = UUID(), name: String, url: String, enabled: Bool = true) {
        self.id = id
        self.name = name
        self.url = url
        self.enabled = enabled
    }
}

enum HostURL {
    /// The port `j3-server` listens on unless told otherwise.
    static let defaultPort = 3780

    /// `text` as a server base URL: `http://` added when no scheme is given,
    /// the default port when none is, no path or trailing slash. `nil` when
    /// it is not a host.
    static func normalize(_ text: String) -> String? {
        var text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.isEmpty { return nil }
        if !text.contains("://") { text = "http://" + text }
        guard var components = URLComponents(string: text),
              let scheme = components.scheme?.lowercased(), scheme == "http" || scheme == "https",
              let host = components.host, !host.isEmpty
        else { return nil }
        components.scheme = scheme
        components.path = ""
        components.query = nil
        components.fragment = nil
        if components.port == nil, scheme == "http" { components.port = defaultPort }
        return components.string
    }

    /// The host part of a base URL, as a fallback display name.
    static func displayName(_ url: String) -> String {
        URLComponents(string: url)?.host ?? url
    }
}

/// The saved hosts, kept in `UserDefaults`.
@MainActor @Observable
final class HostStore {
    static let key = "j3.hosts.v1"

    private(set) var hosts: [SavedHost]
    @ObservationIgnored private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        if let data = defaults.data(forKey: Self.key),
           let hosts = try? JSONDecoder().decode([SavedHost].self, from: data)
        {
            self.hosts = hosts
        } else {
            hosts = []
        }
    }

    /// Adds a host, or returns the saved one with the same URL (enabling it).
    @discardableResult
    func add(url: String, name: String? = nil) -> SavedHost? {
        guard let url = HostURL.normalize(url) else { return nil }
        if let index = hosts.firstIndex(where: { $0.url == url }) {
            hosts[index].enabled = true
            save()
            return hosts[index]
        }
        let trimmed = name?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let host = SavedHost(name: trimmed.isEmpty ? HostURL.displayName(url) : trimmed, url: url)
        hosts.append(host)
        save()
        return host
    }

    /// Replaces the saved host with the same id; `false` when `host.url` is
    /// not a valid URL.
    @discardableResult
    func update(_ host: SavedHost) -> Bool {
        guard let index = hosts.firstIndex(where: { $0.id == host.id }),
              let url = HostURL.normalize(host.url)
        else { return false }
        var host = host
        host.url = url
        host.name = host.name.trimmingCharacters(in: .whitespacesAndNewlines)
        if host.name.isEmpty { host.name = HostURL.displayName(url) }
        hosts[index] = host
        save()
        return true
    }

    func setEnabled(_ id: UUID, _ enabled: Bool) {
        guard let index = hosts.firstIndex(where: { $0.id == id }) else { return }
        hosts[index].enabled = enabled
        save()
    }

    func remove(_ id: UUID) {
        hosts.removeAll { $0.id == id }
        save()
    }

    func move(from source: IndexSet, to destination: Int) {
        hosts.move(fromOffsets: source, toOffset: destination)
        save()
    }

    private func save() {
        if let data = try? JSONEncoder().encode(hosts) {
            defaults.set(data, forKey: Self.key)
        }
    }
}

/// `j3code://` links.
enum DeepLink: Equatable {
    /// `j3code://connections/new?host=<url>&name=<name>&autoConnect=1`: adds
    /// a host; with `autoConnect` it is enabled and connected at once,
    /// otherwise the add form opens prefilled.
    case addHost(url: String, name: String?, autoConnect: Bool)

    init?(_ url: URL) {
        guard url.scheme?.lowercased() == "j3code",
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
        else { return nil }
        let path = [url.host ?? ""] + url.pathComponents.filter { $0 != "/" }
        let query = Dictionary(
            (components.queryItems ?? []).map { ($0.name, $0.value ?? "") },
            uniquingKeysWith: { _, last in last }
        )
        switch path.first {
        case "connections" where path.dropFirst().first == "new":
            guard let host = query["host"], !host.isEmpty else { return nil }
            let flag = query["autoConnect"]?.lowercased()
            self = .addHost(
                url: host,
                name: query["name"].flatMap { $0.isEmpty ? nil : $0 },
                autoConnect: flag == "1" || flag == "true"
            )
        default:
            return nil
        }
    }
}
