import Foundation

// Conveniences over the UniFFI types of `j3_mobile.swift` (crates/j3-mobile).

extension ThreadRow: Identifiable {}
extension Project: Identifiable {}
extension Item: Identifiable {}
extension Request: Identifiable {}
extension QueueEntry: Identifiable {}
extension Attachment: Identifiable {}

extension ModelRef {
    /// `provider/id`, as omp's `^` mentions name a model.
    var slug: String { "\(provider)/\(id)" }

    func with(thinking: ThinkingLevel?) -> ModelRef {
        ModelRef(provider: provider, id: id, thinking: thinking)
    }
}

extension CatalogModel {
    var slug: String { "\(provider)/\(id)" }

    func ref(thinking: ThinkingLevel?) -> ModelRef {
        let level = thinking.flatMap { level in
            self.thinking?.contains(level) == true ? level : nil
        } ?? self.thinking?.first(where: { $0 == .medium }) ?? self.thinking?.last
        return ModelRef(provider: provider, id: id, thinking: self.thinking == nil ? nil : level)
    }
}

extension Catalog {
    func model(_ ref: ModelRef) -> CatalogModel? {
        models.first { $0.provider == ref.provider && $0.id == ref.id }
    }

    var defaultModel: CatalogModel? {
        models.first(where: \.default) ?? models.first
    }

    /// The name of `ref` for display: the catalog's, else its slug.
    func name(of ref: ModelRef) -> String {
        model(ref)?.name ?? ref.slug
    }
}

extension ThinkingLevel: CaseIterable, Identifiable {
    public static var allCases: [ThinkingLevel] { [.off, .minimal, .low, .medium, .high, .xhigh] }
    public var id: Self { self }

    var label: String {
        switch self {
        case .off: "Off"
        case .minimal: "Minimal"
        case .low: "Low"
        case .medium: "Medium"
        case .high: "High"
        case .xhigh: "Extra high"
        }
    }
}

extension ThreadStatus {
    var label: String {
        switch self {
        case .idle: "Idle"
        case .starting: "Starting"
        case .running: "Working"
        case .waiting: "Waiting for you"
        case .stopping: "Stopping"
        case .failed: "Failed"
        }
    }

    /// A turn is under way.
    var isActive: Bool {
        switch self {
        case .starting, .running, .waiting, .stopping: true
        case .idle, .failed: false
        }
    }
}

extension ConnectionStatus {
    var label: String {
        switch self {
        case .connecting: "Connecting"
        case .connected: "Connected"
        case let .reconnecting(attempt, error): "Reconnecting (\(attempt)): \(error)"
        case let .incompatible(message): "Incompatible server: \(message)"
        case .closed: "Disconnected"
        }
    }

    var isConnected: Bool {
        if case .connected = self { true } else { false }
    }
}

extension ClientError {
    /// A sentence for the user.
    var message: String {
        switch self {
        case let .Server(_, message, _): message
        case .Disconnected: "Not connected to the server."
        case let .Http(message): message
        case let .Decode(message): "Unexpected response from the server: \(message)"
        }
    }
}

/// A sentence for the user about any error a command threw.
func describe(_ error: any Error) -> String {
    if let error = error as? ClientError { return error.message }
    return error.localizedDescription
}

enum Time {
    nonisolated(unsafe) private static let fractional: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    nonisolated(unsafe) private static let whole = ISO8601DateFormatter()

    /// An RFC 3339 timestamp from the server.
    static func parse(_ text: String) -> Date? {
        fractional.date(from: text) ?? whole.date(from: text)
    }

    /// "5 min ago"-style text for a server timestamp.
    static func ago(_ text: String, now: Date = .now) -> String {
        guard let date = parse(text) else { return "" }
        let seconds = now.timeIntervalSince(date)
        if seconds < 60 { return "now" }
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return formatter.localizedString(for: date, relativeTo: now)
    }
}
