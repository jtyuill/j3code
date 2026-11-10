import Foundation
import Observation

/// A side question (`/btw`) and its answer: omp answers beside whatever the
/// thread is doing, and the conversation keeps no trace of it.
struct SideAnswer {
    /// The question on its way, until the topic carries it.
    var asked: String?
    /// The question and its follow-ups with their answers.
    var topic: BtwRecord?
    /// Why the question could not be answered.
    var error: String?
    var isRunning = true

    /// The topic a new question follows up on: the one shown, once its last
    /// answer is complete.
    var followUp: BtwRecord? {
        guard error == nil, topic?.turns.last?.status == .complete else { return nil }
        return topic
    }
}

/// An open thread: its live snapshot, its composer, and the commands of the
/// thread screen (send, stop, requests, queue, side questions).
@MainActor @Observable
final class ThreadModel {
    let connection: HostConnection
    let threadId: String
    private(set) var snapshot: ThreadSnapshot?
    /// The queue entry the composer is editing, if any.
    var editingEntry: QueueEntry?
    /// A command is in flight (sending a prompt, answering a request).
    private(set) var isBusy = false
    /// The side question's sheet, while it shows.
    private(set) var sideAnswer: SideAnswer?
    @ObservationIgnored private var sideSubscription: Subscription?
    /// Counts asks, so a late update of an earlier one is dropped.
    @ObservationIgnored private var sideGeneration = 0
    @ObservationIgnored private weak var app: AppModel?
    @ObservationIgnored private var subscription: Subscription?
    @ObservationIgnored private var watchedHost: Host?
    @ObservationIgnored private(set) lazy var composer = ComposerModel(context: self, model: thread?.model)

    init(connection: HostConnection, threadId: String, app: AppModel?) {
        self.connection = connection
        self.threadId = threadId
        self.app = app
        watch()
    }

    var host: Host { connection.host }
    /// The row from the thread snapshot, else from the host's thread list.
    var thread: ThreadRow? { snapshot?.thread ?? connection.thread(threadId) }
    var items: [Item] { snapshot?.items ?? [] }
    var queue: [QueueEntry] { snapshot?.queue ?? [] }
    var title: String { thread?.title ?? "Thread" }
    var status: ThreadStatus { thread?.status ?? .idle }
    var isActive: Bool { status.isActive }
    var project: Project? { thread.flatMap { connection.project($0.projectId) } }
    /// Requests waiting for an answer, oldest first.
    var openRequests: [Request] { (snapshot?.requests ?? []).filter { $0.status == .open } }
    var isMissing: Bool { if case .missing = snapshot?.sync { true } else { false } }

    /// Thinking blocks start expanded (Settings).
    var expandThinking: Bool { app?.settings.expandThinking ?? false }
    var syncMessage: String? {
        switch snapshot?.sync {
        case nil, .loading: snapshot == nil ? "Loading…" : nil
        case .synced: nil
        case .missing: "This thread was deleted."
        case let .error(message): message
        }
    }

    /// Subscribes again when the host's connection was replaced (after a
    /// reconnect on returning to the foreground).
    func ensureWatching() {
        if watchedHost !== connection.host { watch() }
    }

    private func watch() {
        subscription?.cancel()
        let host = connection.host
        watchedHost = host
        let forwarder = MainActorForwarder<ThreadModel, ThreadSnapshot>(
            owner: self, source: host, current: { $0.watchedHost }
        ) { model, snapshot in
            model.apply(snapshot)
        }
        subscription = host.watchThread(threadId: threadId, listener: forwarder)
    }

    private func apply(_ snapshot: ThreadSnapshot) {
        let first = self.snapshot?.thread == nil
        self.snapshot = snapshot
        if first, let model = snapshot.thread?.model, composer.model == nil {
            composer.model = model
        }
        if let editing = editingEntry, !snapshot.queue.contains(where: { $0.id == editing.id }) {
            editingEntry = nil
        }
    }

    // MARK: Commands

    private func perform(_ command: @escaping @MainActor () async throws -> Void) {
        isBusy = true
        Task { @MainActor in
            defer { isBusy = false }
            do { try await command() } catch { app?.alert = describe(error) }
        }
    }

    /// Sends the composer's message: starts a turn when idle; while a turn
    /// runs, `delivery` decides (queue by default, or steer). A `/btw` draft
    /// is asked as a side question instead.
    func send(_ delivery: SendDelivery = .auto) {
        if let entry = editingEntry {
            saveEdit(of: entry)
            return
        }
        if connection.answersSideQuestions, let question = composer.sideQuestion {
            if question.isEmpty {
                app?.alert = "Type a question after /btw."
            } else if !composer.uploads.isEmpty {
                app?.alert = "A side question takes no attachments."
            } else {
                composer.clear()
                askSideQuestion(question)
            }
            return
        }
        guard let message = composer.take() else { return }
        let changed = message.model.flatMap { $0 == thread?.model ? nil : $0 }
        perform { [host, threadId] in
            _ = try await host.send(
                threadId: threadId,
                text: message.text,
                attachmentIds: message.attachments.map(\.id),
                model: changed,
                delivery: delivery
            )
        }
    }

    func stop(holdQueue: Bool = false, force: Bool = false) {
        perform { [host, threadId] in
            _ = try await host.stop(threadId: threadId, holdQueue: holdQueue, force: force)
        }
    }

    func answer(_ request: Request, _ answers: [Answer]) {
        perform { [host, threadId] in
            _ = try await host.answerRequest(threadId: threadId, requestId: request.id, answers: answers)
        }
    }

    func dismiss(_ request: Request) {
        perform { [host, threadId] in
            _ = try await host.dismissRequest(threadId: threadId, requestId: request.id)
        }
    }

    // MARK: Side questions

    /// Asks omp `question` beside the thread, following up on the topic the
    /// sheet shows.
    func askSideQuestion(_ question: String) {
        sideSubscription?.cancel()
        sideGeneration += 1
        let topic = sideAnswer?.followUp
        sideAnswer = SideAnswer(asked: question, topic: topic)
        sideSubscription = host.askBtw(
            threadId: threadId, question: question, recordId: topic?.id,
            listener: SideAnswerForwarder(owner: self, generation: sideGeneration)
        )
    }

    /// Closes the sheet, cancelling a question still being answered.
    func closeSideAnswer() {
        sideSubscription?.cancel()
        sideSubscription = nil
        sideGeneration += 1
        sideAnswer = nil
    }

    fileprivate func sideAnswerChanged(_ topic: BtwRecord, generation: Int) {
        guard generation == sideGeneration, sideAnswer != nil else { return }
        sideAnswer?.topic = topic
        sideAnswer?.asked = nil
    }

    fileprivate func sideAnswerEnded(_ error: String?, generation: Int) {
        guard generation == sideGeneration, sideAnswer != nil else { return }
        sideAnswer?.error = error
        sideAnswer?.isRunning = false
        sideSubscription = nil
    }

    // MARK: Queue

    func edit(_ entry: QueueEntry) {
        editingEntry = entry
        composer.load(text: entry.text)
    }

    func cancelEdit() {
        editingEntry = nil
        composer.load(text: "")
    }

    private func saveEdit(of entry: QueueEntry) {
        guard let message = composer.take() else { return }
        editingEntry = nil
        let attachments = message.attachments.isEmpty ? nil : (entry.attachments + message.attachments).map(\.id)
        perform { [host, threadId] in
            _ = try await host.editQueued(
                threadId: threadId, entryId: entry.id, text: message.text, attachmentIds: attachments
            )
        }
    }

    func cancel(_ entry: QueueEntry) {
        if editingEntry?.id == entry.id { cancelEdit() }
        perform { [host, threadId] in
            _ = try await host.cancelQueued(threadId: threadId, entryId: entry.id)
        }
    }

    /// Sends a queued prompt into the running turn now.
    func steer(_ entry: QueueEntry) {
        perform { [host, threadId] in
            _ = try await host.steerQueued(threadId: threadId, entryId: entry.id)
        }
    }

    /// Stops the running turn and starts this prompt instead.
    func startNow(_ entry: QueueEntry) {
        perform { [host, threadId] in
            _ = try await host.startQueued(threadId: threadId, entryId: entry.id)
        }
    }

    func resumeQueue() {
        perform { [host, threadId] in
            _ = try await host.resumeQueue(threadId: threadId)
        }
    }

    // MARK: Goals

    func createGoal(objective: String, budget: UInt64?) {
        perform { [host, threadId] in
            _ = try await host.createGoal(threadId: threadId, objective: objective, budget: budget)
        }
    }

    func pauseGoal() {
        perform { [host, threadId] in _ = try await host.pauseGoal(threadId: threadId) }
    }

    func resumeGoal() {
        perform { [host, threadId] in _ = try await host.resumeGoal(threadId: threadId) }
    }

    func dropGoal() {
        perform { [host, threadId] in _ = try await host.dropGoal(threadId: threadId) }
    }

    func setMode(_ mode: ThreadMode) {
        perform { [host, threadId] in _ = try await host.setMode(threadId: threadId, mode: mode) }
    }
}

extension ThreadModel: ComposerContext {
    var catalog: Catalog? { connection.catalog }
    var projectId: String? { thread?.projectId }
    var returnSends: Bool { app?.settings.returnSends ?? true }

    func searchEntries(query: String) async throws -> EntriesResult {
        guard let projectId else { return EntriesResult(entries: [], truncated: false) }
        return try await host.searchEntries(
            projectId: projectId, query: query, limit: 50, kind: nil, imagesOnly: false
        )
    }

    func upload(name: String, mimeType: String, data: Data) async throws -> Attachment {
        try await host.uploadAttachment(name: name, mimeType: mimeType, bytes: data)
    }
}

/// Hands a side question's updates to its thread on the main actor, in
/// order. Listener calls arrive on a library runtime thread; this returns at
/// once.
private final class SideAnswerForwarder: BtwListener, @unchecked Sendable {
    private weak var owner: ThreadModel?
    private let generation: Int

    @MainActor
    init(owner: ThreadModel, generation: Int) {
        self.owner = owner
        self.generation = generation
    }

    func btwChanged(topic: BtwRecord) {
        DispatchQueue.main.async { [self] in
            MainActor.assumeIsolated { owner?.sideAnswerChanged(topic, generation: generation) }
        }
    }

    func btwEnded(error: String?) {
        DispatchQueue.main.async { [self] in
            MainActor.assumeIsolated { owner?.sideAnswerEnded(error, generation: generation) }
        }
    }
}
