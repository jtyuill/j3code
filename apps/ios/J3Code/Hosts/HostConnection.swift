import Foundation
import Observation

/// One enabled saved host: its `Host` (the Rust connection, which reconnects
/// by itself) and the last snapshot it pushed.
@MainActor @Observable
final class HostConnection: Identifiable {
    let saved: SavedHost
    private(set) var host: Host
    private(set) var snapshot: HostSnapshot?
    @ObservationIgnored private var subscription: Subscription?

    nonisolated let id: UUID
    var status: ConnectionStatus { snapshot?.status ?? .connecting }
    var catalog: Catalog? { snapshot?.catalog }
    /// The server answers side questions (`/btw`).
    var answersSideQuestions: Bool { snapshot?.info?.capabilities.contains("btw") ?? false }
    var projects: [Project] { snapshot?.projects ?? [] }
    var threads: [ThreadRow] { snapshot?.threads ?? [] }
    var name: String { saved.name }

    init(saved: SavedHost) {
        self.saved = saved
        id = saved.id
        host = Host(baseUrl: saved.url)
        watch()
    }

    private func watch() {
        let forwarder = MainActorForwarder<HostConnection, HostSnapshot>(
            owner: self, source: host, current: { $0.host }
        ) { connection, snapshot in
            connection.snapshot = snapshot
        }
        subscription = host.watch(listener: forwarder)
    }

    /// After the app comes back to the foreground: a connection that is not
    /// live is replaced by a fresh one, so it does not sit out its backoff.
    func reconnectIfIdle() {
        switch status {
        case .connected, .closed:
            return
        case .connecting, .reconnecting, .incompatible:
            let old = host
            subscription?.cancel()
            host = Host(baseUrl: saved.url)
            watch()
            Task { await old.close() }
        }
    }

    func close() {
        subscription?.cancel()
        subscription = nil
        let host = self.host
        Task { await host.close() }
    }

    func project(_ id: String) -> Project? {
        projects.first { $0.id == id }
    }

    func thread(_ id: String) -> ThreadRow? {
        threads.first { $0.id == id }
    }
}

/// Hands a listener's snapshots to `owner` on the main actor, in order, and
/// only while `owner` still uses `source` (a replaced `Host`'s late calls are
/// dropped). Listener calls arrive on a library runtime thread; this returns
/// at once.
final class MainActorForwarder<Owner: AnyObject, Snapshot>: HostListener, ThreadListener, @unchecked Sendable {
    private weak var owner: Owner?
    private weak var source: Host?
    private let deliver: @MainActor (Owner, Snapshot) -> Void
    private let current: @MainActor (Owner) -> Host?

    @MainActor
    init(owner: Owner, source: Host, current: @escaping @MainActor (Owner) -> Host?,
         deliver: @escaping @MainActor (Owner, Snapshot) -> Void)
    {
        self.owner = owner
        self.source = source
        self.deliver = deliver
        self.current = current
    }

    private func forward(_ snapshot: Snapshot) {
        // The main queue is FIFO, so snapshots land in the order they came.
        DispatchQueue.main.async { [self] in
            MainActor.assumeIsolated {
                guard let owner, let source, current(owner) === source else { return }
                deliver(owner, snapshot)
            }
        }
    }

    func hostChanged(snapshot: HostSnapshot) {
        if let snapshot = snapshot as? Snapshot { forward(snapshot) }
    }

    func threadChanged(snapshot: ThreadSnapshot) {
        if let snapshot = snapshot as? Snapshot { forward(snapshot) }
    }
}
