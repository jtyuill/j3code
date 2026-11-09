import SwiftUI

/// One thread: the feed, open requests, the queue and the composer.
struct ThreadScreen: View {
    @Environment(AppModel.self) private var app
    @Bindable var model: ThreadModel
    @State private var showingGoal = false
    @State private var renaming: RenameTarget?

    var body: some View {
        VStack(spacing: 0) {
            if let message = model.syncMessage {
                Text(message)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(6)
            }
            FeedView(thread: model)
                .frame(maxHeight: .infinity)
            bottom
        }
        .navigationTitle(model.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbar }
        .onAppear { model.ensureWatching() }
        .onChange(of: ObjectIdentifier(model.connection.host)) { _, _ in model.ensureWatching() }
        .sheet(isPresented: $showingGoal) {
            NavigationStack { GoalSheet(model: model) }
        }
        .sheet(item: $renaming) { target in
            NavigationStack { RenameThreadView(target: target) }
        }
    }

    @ViewBuilder
    private var bottom: some View {
        VStack(spacing: 8) {
            ForEach(model.openRequests) { request in
                RequestCard(request: request, model: model)
                    .id(request.id)
            }
            if !model.queue.isEmpty {
                QueueView(model: model)
            }
            if let entry = model.editingEntry {
                HStack {
                    Label("Editing queued prompt", systemImage: "pencil")
                        .font(.caption)
                    Spacer()
                    Button("Cancel") { model.cancelEdit() }.font(.caption)
                }
                .foregroundStyle(.secondary)
                .id(entry.id)
            }
            if !model.isMissing {
                ComposerView(model: model.composer, onSubmit: { model.send() }) {
                    actions
                }
            }
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
        .background(.bar)
    }

    @ViewBuilder
    private var actions: some View {
        if model.isActive, model.editingEntry == nil {
            HStack(spacing: 6) {
                if model.composer.canSend {
                    Menu {
                        Button {
                            model.send(.steer)
                        } label: { Label("Steer Now", systemImage: "arrow.turn.down.right") }
                        Button {
                            model.send(.queue)
                        } label: { Label("Queue", systemImage: "text.append") }
                        Button(role: .destructive) {
                            model.send(.restart)
                        } label: { Label("Stop and Send", systemImage: "arrow.clockwise") }
                    } label: {
                        Image(systemName: "arrow.up.circle.fill").font(.title)
                    } primaryAction: {
                        model.send(.queue)
                    }
                    .accessibilityLabel("Queue message")
                    .accessibilityIdentifier("send")
                }
                Button {
                    model.stop()
                } label: {
                    Image(systemName: "stop.circle.fill").font(.title).foregroundStyle(.red)
                }
                .keyboardShortcut(".", modifiers: .command)
                .accessibilityLabel("Stop")
                .contextMenu {
                    Button("Stop and Hold Queue") { model.stop(holdQueue: true) }
                    Button("Force Stop", role: .destructive) { model.stop(force: true) }
                }
            }
        } else {
            Button {
                model.send()
            } label: {
                Image(systemName: model.editingEntry == nil ? "arrow.up.circle.fill" : "checkmark.circle.fill")
                    .font(.title)
            }
            .disabled(!model.composer.canSend)
            .accessibilityLabel(model.editingEntry == nil ? "Send" : "Save")
            .accessibilityIdentifier("send")
        }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .topBarTrailing) {
            Menu {
                if let thread = model.thread {
                    Button {
                        renaming = RenameTarget(
                            ref: ThreadRef(host: model.connection.id, thread: thread.id), title: thread.title
                        )
                    } label: { Label("Rename…", systemImage: "pencil") }
                    if thread.archivedAt == nil {
                        Button {
                            let host = model.host
                            app.run { _ = try await host.archiveThread(threadId: thread.id) }
                        } label: { Label("Archive", systemImage: "archivebox") }
                    } else {
                        Button {
                            let host = model.host
                            app.run { _ = try await host.unarchiveThread(threadId: thread.id) }
                        } label: { Label("Unarchive", systemImage: "tray.and.arrow.up") }
                    }
                    Picker("Mode", selection: Binding(
                        get: { thread.mode },
                        set: { model.setMode($0) }
                    )) {
                        Text("Default").tag(ThreadMode.default)
                        Text("Plan").tag(ThreadMode.plan)
                    }
                    Divider()
                    goalMenu(thread)
                    if model.queue.contains(where: \.held) {
                        Button { model.resumeQueue() } label: {
                            Label("Resume Queue", systemImage: "play")
                        }
                    }
                }
            } label: {
                Label("Thread", systemImage: "ellipsis.circle")
            }
        }
    }

    @ViewBuilder
    private func goalMenu(_ thread: ThreadRow) -> some View {
        switch thread.goal?.state {
        case .active?:
            Button { model.pauseGoal() } label: { Label("Pause Goal", systemImage: "pause") }
            Button(role: .destructive) { model.dropGoal() } label: { Label("Drop Goal", systemImage: "xmark") }
        case .paused?, .budgetLimited?:
            Button { model.resumeGoal() } label: { Label("Resume Goal", systemImage: "play") }
            Button(role: .destructive) { model.dropGoal() } label: { Label("Drop Goal", systemImage: "xmark") }
        default:
            Button { showingGoal = true } label: { Label("New Goal…", systemImage: "target") }
        }
    }
}

/// Starts omp goal mode.
private struct GoalSheet: View {
    @Environment(\.dismiss) private var dismiss
    let model: ThreadModel
    @State private var objective = ""
    @State private var budget = ""

    var body: some View {
        Form {
            Section("Objective") {
                TextField("What should be achieved?", text: $objective, axis: .vertical)
                    .lineLimit(3...8)
            }
            Section("Token budget (optional)") {
                TextField("e.g. 500000", text: $budget)
                    .keyboardType(.numberPad)
            }
        }
        .navigationTitle("New Goal")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
            ToolbarItem(placement: .confirmationAction) {
                Button("Start") {
                    model.createGoal(
                        objective: objective.trimmingCharacters(in: .whitespacesAndNewlines),
                        budget: UInt64(budget.filter(\.isNumber))
                    )
                    dismiss()
                }
                .disabled(objective.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
    }
}
