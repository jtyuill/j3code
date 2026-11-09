import SwiftUI

/// Adds a directory on a host as a project, browsing the host's directories
/// (`projects.browse`).
struct AddProjectView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let hostId: UUID
    @State private var path = ""
    @State private var title = ""
    @State private var titleEdited = false
    @State private var result: BrowseResult?
    @State private var error: String?
    @State private var adding = false

    private var connection: HostConnection? { app.connection(hostId) }

    var body: some View {
        Form {
            Section {
                TextField("Directory", text: $path)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .font(.body.monospaced())
                    .accessibilityIdentifier("project-path")
                TextField("Title", text: Binding(
                    get: { title },
                    set: { title = $0; titleEdited = true }
                ))
            } header: {
                Text(connection?.name ?? "")
            } footer: {
                if let error { Text(error).foregroundStyle(.red) }
            }
            Section("Folders") {
                if let parent = result?.parentPath, parent != "/", !parent.isEmpty {
                    Button {
                        open(parentOf(parent))
                    } label: {
                        Label("..", systemImage: "arrow.up.left")
                    }
                }
                ForEach(result?.entries ?? [], id: \.path) { entry in
                    Button {
                        open(entry.path)
                    } label: {
                        Label(entry.name, systemImage: "folder")
                    }
                    .foregroundStyle(.primary)
                }
                if result?.entries.isEmpty == true {
                    Text("No folders").foregroundStyle(.secondary)
                }
            }
        }
        .navigationTitle("Add Project")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
            ToolbarItem(placement: .confirmationAction) {
                Button("Add", action: add)
                    .disabled(adding || trimmedPath.isEmpty)
                    .accessibilityIdentifier("add-project")
            }
        }
        .task(id: path) { await browse() }
        .onAppear {
            if path.isEmpty, let home = connection?.snapshot?.info?.home {
                path = home.hasSuffix("/") ? home : home + "/"
            }
        }
        .onChange(of: path) { _, path in
            if !titleEdited { title = Self.defaultTitle(path) }
        }
    }

    private var trimmedPath: String {
        var path = self.path.trimmingCharacters(in: .whitespacesAndNewlines)
        while path.count > 1, path.hasSuffix("/") { path.removeLast() }
        return path
    }

    static func defaultTitle(_ path: String) -> String {
        path.split(separator: "/").last.map(String.init) ?? ""
    }

    private func parentOf(_ path: String) -> String {
        var parts = path.split(separator: "/")
        if !parts.isEmpty { parts.removeLast() }
        return "/" + parts.joined(separator: "/")
    }

    private func open(_ directory: String) {
        path = directory.hasSuffix("/") ? directory : directory + "/"
    }

    private func browse() async {
        guard let host = connection?.host, !path.isEmpty else { return }
        // Wait for typing to pause.
        try? await Task.sleep(for: .milliseconds(200))
        if Task.isCancelled { return }
        do {
            result = try await host.browseDirectories(partialPath: path)
            error = nil
        } catch {
            self.error = describe(error)
        }
    }

    private func add() {
        guard let host = connection?.host else { return }
        let path = trimmedPath
        let title = self.title.trimmingCharacters(in: .whitespacesAndNewlines)
        adding = true
        Task {
            defer { adding = false }
            do {
                _ = try await host.createProject(
                    title: title.isEmpty ? Self.defaultTitle(path) : title, path: path, defaultModel: nil
                )
                dismiss()
            } catch {
                self.error = describe(error)
            }
        }
    }
}
