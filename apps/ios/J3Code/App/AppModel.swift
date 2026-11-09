import Foundation
import Observation

/// A thread on one of the connected hosts.
struct ThreadRef: Hashable, Sendable {
    var host: UUID
    var thread: String
}

/// Where the new-thread form starts: a host and maybe a project.
struct NewThreadTarget: Identifiable, Hashable {
    var host: UUID
    var project: String?
    var id: Self { self }
}

/// The app's state: saved hosts, their live connections, the selection and
/// the open thread models.
@MainActor @Observable
final class AppModel {
    let store: HostStore
    let settings: AppSettings
    private(set) var connections: [UUID: HostConnection] = [:]
    var selection: ThreadRef?
    /// The add-host form, prefilled by a deep link without `autoConnect`.
    var addHostDraft: SavedHost?
    var showingConnections = false
    var newThread: NewThreadTarget?
    var showingSettings = false
    /// A failed command, shown as an alert.
    var alert: String?
    @ObservationIgnored private var threadModels: [ThreadRef: ThreadModel] = [:]

    init(store: HostStore, settings: AppSettings) {
        self.store = store
        self.settings = settings
        syncConnections()
        if store.hosts.isEmpty { showingConnections = true }
    }

    /// The app's model at launch. `-j3-reset` (UI tests) starts with no
    /// saved hosts.
    static func launch() -> AppModel {
        if ProcessInfo.processInfo.arguments.contains("-j3-reset") {
            UserDefaults.standard.removeObject(forKey: HostStore.key)
        }
        return AppModel(store: HostStore(), settings: AppSettings())
    }

    /// Enabled hosts, in the saved order, with their connections.
    var activeConnections: [HostConnection] {
        store.hosts.compactMap { $0.enabled ? connections[$0.id] : nil }
    }

    func connection(_ id: UUID) -> HostConnection? { connections[id] }

    /// Opens a connection for each enabled host and closes the rest; a host
    /// whose URL changed gets a new connection.
    func syncConnections() {
        var wanted: [UUID: SavedHost] = [:]
        for host in store.hosts where host.enabled { wanted[host.id] = host }
        for (id, connection) in connections where wanted[id] != connection.saved {
            connection.close()
            connections[id] = nil
            threadModels = threadModels.filter { $0.key.host != id }
            if selection?.host == id { selection = nil }
        }
        for (id, host) in wanted where connections[id] == nil {
            connections[id] = HostConnection(saved: host)
        }
    }

    // MARK: Hosts

    @discardableResult
    func addHost(url: String, name: String?) -> SavedHost? {
        let host = store.add(url: url, name: name)
        syncConnections()
        return host
    }

    func updateHost(_ host: SavedHost) -> Bool {
        let ok = store.update(host)
        syncConnections()
        return ok
    }

    func setEnabled(_ id: UUID, _ enabled: Bool) {
        store.setEnabled(id, enabled)
        syncConnections()
    }

    func removeHost(_ id: UUID) {
        store.remove(id)
        syncConnections()
    }

    // MARK: Lifecycle

    /// The scene became active again: sockets may have died in the
    /// background, so idle connections reconnect now.
    func becameActive() {
        for connection in connections.values { connection.reconnectIfIdle() }
    }

    func handle(_ url: URL) {
        guard let link = DeepLink(url) else { return }
        switch link {
        case let .addHost(url, name, autoConnect):
            if autoConnect {
                if addHost(url: url, name: name) == nil {
                    alert = "\(url) is not a server address."
                } else {
                    showingConnections = false
                }
            } else {
                addHostDraft = SavedHost(name: name ?? "", url: url)
                showingConnections = true
            }
        }
    }

    // MARK: Threads

    /// The model of an open thread, kept while the thread stays selected
    /// (and reused when it is reopened).
    func threadModel(_ ref: ThreadRef) -> ThreadModel? {
        if let model = threadModels[ref] { return model }
        guard let connection = connections[ref.host] else { return nil }
        let model = ThreadModel(connection: connection, threadId: ref.thread, app: self)
        threadModels[ref] = model
        return model
    }

    /// Drops thread models that are neither selected nor busy sending.
    func releaseThreads(keeping ref: ThreadRef?) {
        threadModels = threadModels.filter { $0.key == ref || $0.value.isBusy }
    }

    /// Runs a command, reporting a failure in an alert.
    func run(_ command: @escaping @MainActor () async throws -> Void) {
        Task { @MainActor in
            do { try await command() } catch { alert = describe(error) }
        }
    }
}

/// User preferences kept in `UserDefaults`.
@MainActor @Observable
final class AppSettings {
    @ObservationIgnored private let defaults: UserDefaults

    /// Show archived threads in the thread list.
    var showArchived: Bool { didSet { defaults.set(showArchived, forKey: "j3.showArchived") } }
    /// Thinking blocks start expanded.
    var expandThinking: Bool { didSet { defaults.set(expandThinking, forKey: "j3.expandThinking") } }
    /// Return sends (hardware keyboard); otherwise ⌘Return does.
    var returnSends: Bool { didSet { defaults.set(returnSends, forKey: "j3.returnSends") } }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        showArchived = defaults.bool(forKey: "j3.showArchived")
        expandThinking = defaults.bool(forKey: "j3.expandThinking")
        returnSends = defaults.object(forKey: "j3.returnSends") as? Bool ?? true
    }
}
