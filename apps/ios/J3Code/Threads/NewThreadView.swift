import Observation
import SwiftUI

/// The new-thread form's state: where the thread goes and its composer.
@MainActor @Observable
final class NewThreadModel {
    var hostId: UUID { didSet { if hostId != oldValue { projectId = nil; pickDefaults() } } }
    var projectId: String? { didSet { if projectId != oldValue { pickModel() } } }
    var planMode = false
    private(set) var creating = false
    @ObservationIgnored private weak var app: AppModel?
    @ObservationIgnored private(set) lazy var composer = ComposerModel(context: self, model: nil)

    init(target: NewThreadTarget, app: AppModel) {
        hostId = target.host
        self.app = app
        projectId = target.project
        pickDefaults()
    }

    var connection: HostConnection? { app?.connection(hostId) }
    var project: Project? { projectId.flatMap { connection?.project($0) } }

    /// The project of the host's most recent thread, then the model of the
    /// project (else the catalog's default).
    private func pickDefaults() {
        if projectId == nil, let connection {
            let groups = ThreadGrouping.groups(
                projects: connection.projects, threads: connection.threads, archived: false
            )
            projectId = groups.first?.projectId
        }
        pickModel()
    }

    private func pickModel() {
        if let model = project?.defaultModel {
            composer.model = model
        } else if let connection, let model = recentModel(connection) ?? catalog?.defaultModel?.ref(thinking: nil) {
            composer.model = model
        }
    }

    /// The model of the newest thread in the project.
    private func recentModel(_ connection: HostConnection) -> ModelRef? {
        connection.threads
            .filter { $0.projectId == projectId }
            .max { ThreadGrouping.activity($0) < ThreadGrouping.activity($1) }?
            .model
    }

    /// Creates the thread with the composer's message; returns its ref.
    func create() async -> ThreadRef? {
        guard let connection, let projectId, let app else { return nil }
        guard let model = composer.model ?? catalog?.defaultModel?.ref(thinking: nil) else {
            app.alert = "Pick a model first."
            return nil
        }
        guard let message = composer.take() else { return nil }
        creating = true
        defer { creating = false }
        do {
            let thread = try await connection.host.createThread(
                projectId: projectId,
                model: message.model ?? model,
                mode: planMode ? .plan : nil,
                title: nil,
                text: message.text,
                attachmentIds: message.attachments.map(\.id)
            )
            return ThreadRef(host: connection.id, thread: thread.id)
        } catch {
            composer.load(text: message.text)
            app.alert = describe(error)
            return nil
        }
    }
}

extension NewThreadModel: ComposerContext {
    var catalog: Catalog? { connection?.catalog }
    var returnSends: Bool { app?.settings.returnSends ?? true }

    func searchEntries(query: String) async throws -> EntriesResult {
        guard let connection, let projectId else { return EntriesResult(entries: [], truncated: false) }
        return try await connection.host.searchEntries(
            projectId: projectId, query: query, limit: 50, kind: nil, imagesOnly: false
        )
    }

    func upload(name: String, mimeType: String, data: Data) async throws -> Attachment {
        guard let connection else { throw ClientError.Disconnected }
        return try await connection.host.uploadAttachment(name: name, mimeType: mimeType, bytes: data)
    }
}

struct NewThreadView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let target: NewThreadTarget
    @State private var model: NewThreadModel?
    @State private var addingProject = false

    var body: some View {
        Group {
            if let model {
                form(model)
            } else {
                ProgressView()
            }
        }
        .navigationTitle("New Thread")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            if model == nil { model = NewThreadModel(target: target, app: app) }
        }
    }

    @ViewBuilder
    private func form(_ model: NewThreadModel) -> some View {
        @Bindable var model = model
        VStack(spacing: 0) {
            Form {
                if app.activeConnections.count > 1 {
                    Picker("Server", selection: $model.hostId) {
                        ForEach(app.activeConnections) { connection in
                            Text(connection.name).tag(connection.id)
                        }
                    }
                }
                Picker("Project", selection: $model.projectId) {
                    Text("Choose…").tag(String?.none)
                    ForEach((model.connection?.projects ?? []).sorted { $0.title < $1.title }) { project in
                        Text(project.title).tag(Optional(project.id))
                    }
                }
                .accessibilityIdentifier("new-thread-project")
                if let project = model.project {
                    Text(project.path)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                }
                Button("Add Project…") { addingProject = true }
                Toggle("Plan mode", isOn: $model.planMode)
            }
            .frame(maxHeight: 320)
            Divider()
            Spacer(minLength: 0)
            ComposerView(model: model.composer, onSubmit: { create(model) }) {
                Button {
                    create(model)
                } label: {
                    Image(systemName: "arrow.up.circle.fill").font(.title)
                }
                .disabled(model.creating || model.projectId == nil || !model.composer.canSend)
                .accessibilityLabel("Start thread")
                .accessibilityIdentifier("start-thread")
            }
            .padding(.horizontal)
            .padding(.bottom, 8)
        }
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
        }
        .sheet(isPresented: $addingProject) {
            NavigationStack { AddProjectView(hostId: model.hostId) }
        }
    }

    private func create(_ model: NewThreadModel) {
        Task {
            if let ref = await model.create() {
                app.selection = ref
                dismiss()
            }
        }
    }
}
