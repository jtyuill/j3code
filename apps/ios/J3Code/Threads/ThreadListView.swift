import SwiftUI

/// The sidebar: every enabled host's threads, grouped by project.
struct ThreadListView: View {
    @Environment(AppModel.self) private var app
    @State private var search = ""
    @State private var renaming: RenameTarget?
    @State private var deleting: RenameTarget?
    @State private var addingProject: UUID?

    var body: some View {
        @Bindable var app = app
        List(selection: $app.selection) {
            if app.activeConnections.isEmpty {
                Section {
                    Button("Add a Server") { app.showingConnections = true }
                }
            }
            ForEach(app.activeConnections) { connection in
                HostSections(
                    connection: connection,
                    showHost: app.activeConnections.count > 1,
                    search: search,
                    renaming: $renaming,
                    deleting: $deleting,
                    addingProject: $addingProject
                )
            }
        }
        .listStyle(.sidebar)
        .searchable(text: $search, placement: .sidebar, prompt: "Threads")
        .navigationTitle("j3")
        .toolbar {
            ToolbarItemGroup(placement: .topBarTrailing) {
                Menu {
                    Toggle("Show Archived", isOn: Bindable(app.settings).showArchived)
                    Divider()
                    ForEach(app.activeConnections) { connection in
                        Button("Add Project on \(connection.name)…") { addingProject = connection.id }
                    }
                    Divider()
                    Button("Connections…") { app.showingConnections = true }
                    Button("Settings…") { app.showingSettings = true }
                } label: {
                    Label("More", systemImage: "ellipsis.circle")
                }
                .accessibilityIdentifier("list-menu")
                Button {
                    if let host = app.activeConnections.first?.id { app.newThread = NewThreadTarget(host: host) }
                } label: {
                    Label("New Thread", systemImage: "square.and.pencil")
                }
                .disabled(app.activeConnections.isEmpty)
                .accessibilityIdentifier("new-thread")
            }
        }
        .sheet(item: $renaming) { target in
            NavigationStack { RenameThreadView(target: target) }
        }
        .sheet(item: $addingProject) { host in
            NavigationStack { AddProjectView(hostId: host) }
        }
        .confirmationDialog(
            "Delete this thread?",
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            titleVisibility: .visible,
            presenting: deleting
        ) { target in
            Button("Delete “\(target.title)”", role: .destructive) {
                guard let host = app.connection(target.ref.host)?.host else { return }
                if app.selection == target.ref { app.selection = nil }
                app.run { _ = try await host.deleteThread(threadId: target.ref.thread, onlyIfUnused: false) }
            }
        } message: { _ in
            Text("The conversation is removed from the server.")
        }
        .refreshable { app.becameActive() }
    }
}

/// A thread to rename or delete.
struct RenameTarget: Identifiable, Hashable {
    let ref: ThreadRef
    let title: String
    var id: ThreadRef { ref }
}

extension UUID: @retroactive Identifiable {
    public var id: UUID { self }
}

private struct HostSections: View {
    @Environment(AppModel.self) private var app
    let connection: HostConnection
    let showHost: Bool
    let search: String
    @Binding var renaming: RenameTarget?
    @Binding var deleting: RenameTarget?
    @Binding var addingProject: UUID?

    var body: some View {
        if !connection.status.isConnected || connection.snapshot?.synced == false {
            Section {
                HStack(spacing: 10) {
                    StatusDot(status: connection.status)
                    VStack(alignment: .leading) {
                        Text(connection.name).font(.subheadline.weight(.semibold))
                        Text(connection.status.label)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                    }
                    if case .connected = connection.status {
                        Spacer()
                        ProgressView()
                    }
                }
            }
        }
        let groups = ThreadGrouping.groups(
            projects: connection.projects,
            threads: connection.threads,
            archived: app.settings.showArchived,
            search: search
        )
        ForEach(groups) { group in
            Section {
                ForEach(group.threads) { thread in
                    let ref = ThreadRef(host: connection.id, thread: thread.id)
                    NavigationLink(value: ref) {
                        ThreadRowView(thread: thread)
                    }
                    .tag(ref)
                    .accessibilityIdentifier("thread-\(thread.title)")
                    .swipeActions(edge: .trailing) {
                        Button(role: .destructive) {
                            deleting = RenameTarget(ref: ref, title: thread.title)
                        } label: { Label("Delete", systemImage: "trash") }
                        archiveButton(thread)
                    }
                    .contextMenu {
                        Button {
                            renaming = RenameTarget(ref: ref, title: thread.title)
                        } label: { Label("Rename…", systemImage: "pencil") }
                        archiveButton(thread)
                        Button(role: .destructive) {
                            deleting = RenameTarget(ref: ref, title: thread.title)
                        } label: { Label("Delete…", systemImage: "trash") }
                    }
                }
                if group.threads.isEmpty {
                    Text("No threads").font(.caption).foregroundStyle(.secondary)
                }
            } header: {
                HStack {
                    Text(showHost ? "\(group.title) · \(connection.name)" : group.title)
                    Spacer()
                    Button {
                        app.newThread = NewThreadTarget(host: connection.id, project: group.projectId)
                    } label: {
                        Image(systemName: "plus")
                    }
                    .buttonStyle(.borderless)
                    .accessibilityLabel("New thread in \(group.title)")
                }
            }
        }
        if connection.status.isConnected, groups.isEmpty, search.isEmpty {
            Section(connection.name) {
                Button("Add a Project…") { addingProject = connection.id }
            }
        }
    }

    @ViewBuilder
    private func archiveButton(_ thread: ThreadRow) -> some View {
        let host = connection.host
        if thread.archivedAt == nil {
            Button {
                app.run { _ = try await host.archiveThread(threadId: thread.id) }
            } label: { Label("Archive", systemImage: "archivebox") }
                .tint(.indigo)
        } else {
            Button {
                app.run { _ = try await host.unarchiveThread(threadId: thread.id) }
            } label: { Label("Unarchive", systemImage: "tray.and.arrow.up") }
                .tint(.indigo)
        }
    }
}

/// One thread in the list: title, state and the last message.
struct ThreadRowView: View {
    let thread: ThreadRow

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                indicator
                Text(thread.title.isEmpty ? "Untitled" : thread.title)
                    .font(.body)
                    .lineLimit(1)
                Spacer(minLength: 4)
                Text(Time.ago(ThreadGrouping.activity(thread)))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if let message = thread.lastMessage {
                Text(message.text.replacingOccurrences(of: "\n", with: " "))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            HStack(spacing: 8) {
                if thread.pendingRequest != nil {
                    Label("Needs answer", systemImage: "questionmark.bubble")
                        .foregroundStyle(.orange)
                }
                if thread.queued > 0 {
                    Label("\(thread.queued) queued", systemImage: "text.line.first.and.arrowtriangle.forward")
                }
                if thread.archivedAt != nil {
                    Label("Archived", systemImage: "archivebox")
                }
                if let goal = thread.goal, goal.state == .active {
                    Label("Goal", systemImage: "target")
                }
            }
            .font(.caption2)
            .foregroundStyle(.secondary)
            .labelStyle(.titleAndIcon)
        }
        .padding(.vertical, 2)
    }

    @ViewBuilder
    private var indicator: some View {
        switch thread.status {
        case .starting, .running, .stopping:
            ProgressView().controlSize(.mini)
        case .waiting:
            Image(systemName: "questionmark.circle.fill").foregroundStyle(.orange)
        case .failed:
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
        case .idle:
            EmptyView()
        }
    }
}

/// Renames a thread.
struct RenameThreadView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let target: RenameTarget
    @State private var title = ""

    var body: some View {
        Form {
            TextField("Title", text: $title)
                .onSubmit(save)
        }
        .navigationTitle("Rename Thread")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { title = target.title }
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
            ToolbarItem(placement: .confirmationAction) {
                Button("Save", action: save)
                    .disabled(title.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
    }

    private func save() {
        let title = self.title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty, let host = app.connection(target.ref.host)?.host else { return }
        let thread = target.ref.thread
        app.run { _ = try await host.renameThread(threadId: thread, title: title) }
        dismiss()
    }
}
