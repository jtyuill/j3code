import SwiftUI

/// The saved hosts: add, rename, change URL, disable, remove.
struct ConnectionsView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var editing: SavedHost?
    @State private var adding = false

    var body: some View {
        List {
            Section {
                ForEach(app.store.hosts) { host in
                    HostRow(host: host, connection: app.connection(host.id))
                        .contentShape(Rectangle())
                        .onTapGesture { editing = host }
                        .swipeActions {
                            Button(role: .destructive) { app.removeHost(host.id) } label: {
                                Label("Remove", systemImage: "trash")
                            }
                            Button { app.setEnabled(host.id, !host.enabled) } label: {
                                Label(host.enabled ? "Disable" : "Enable",
                                      systemImage: host.enabled ? "pause.circle" : "play.circle")
                            }
                            .tint(.orange)
                        }
                        .contextMenu {
                            Button("Edit") { editing = host }
                            Button(host.enabled ? "Disable" : "Enable") { app.setEnabled(host.id, !host.enabled) }
                            Button("Remove", role: .destructive) { app.removeHost(host.id) }
                        }
                }
                .onMove { app.store.move(from: $0, to: $1) }
            } footer: {
                Text("Servers are reached over plain HTTP on your tailnet, e.g. `shieldy:3780`.")
            }
            Section {
                Button { adding = true } label: {
                    Label("Add Server", systemImage: "plus")
                }
                .accessibilityIdentifier("add-server")
            }
        }
        .navigationTitle("Connections")
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
            ToolbarItem(placement: .topBarLeading) { EditButton() }
        }
        .sheet(item: $editing) { host in
            NavigationStack { HostEditorView(host: host, isNew: false) }
        }
        .sheet(isPresented: $adding) {
            NavigationStack { HostEditorView(host: SavedHost(name: "", url: ""), isNew: true) }
        }
        .onAppear {
            if let draft = app.addHostDraft {
                app.addHostDraft = nil
                editing = nil
                adding = false
                pendingDraft = draft
            }
        }
        .sheet(item: $pendingDraft) { draft in
            NavigationStack { HostEditorView(host: draft, isNew: true) }
        }
    }

    @State private var pendingDraft: SavedHost?
}

private struct HostRow: View {
    let host: SavedHost
    let connection: HostConnection?

    var body: some View {
        HStack(spacing: 12) {
            StatusDot(status: host.enabled ? connection?.status : nil)
            VStack(alignment: .leading, spacing: 2) {
                Text(host.name).font(.body)
                Text(host.url).font(.caption).foregroundStyle(.secondary)
                if let connection, host.enabled, !connection.status.isConnected {
                    Text(connection.status.label)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                } else if let info = connection?.snapshot?.info {
                    Text("\(info.name) · j3 \(info.version)")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer()
            if !host.enabled {
                Text("Off").font(.caption).foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

/// A green, orange or gray dot for a connection's status.
struct StatusDot: View {
    let status: ConnectionStatus?

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 9, height: 9)
            .accessibilityLabel(status?.label ?? "Disabled")
    }

    private var color: Color {
        switch status {
        case .connected: .green
        case .connecting, .reconnecting: .orange
        case .incompatible: .red
        case .closed, nil: .gray
        }
    }
}

/// Adds or edits one saved host.
struct HostEditorView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State var host: SavedHost
    let isNew: Bool
    @State private var invalid = false

    var body: some View {
        Form {
            Section("Server") {
                TextField("Address, e.g. shieldy:3780", text: $host.url)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
                    .accessibilityIdentifier("host-url")
                TextField("Name (optional)", text: $host.name)
                    .accessibilityIdentifier("host-name")
            }
            if !isNew {
                Section {
                    Toggle("Connected", isOn: $host.enabled)
                }
                Section {
                    Button("Remove Server", role: .destructive) {
                        app.removeHost(host.id)
                        dismiss()
                    }
                }
            }
        }
        .navigationTitle(isNew ? "Add Server" : host.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { dismiss() }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button(isNew ? "Add" : "Save") { save() }
                    .disabled(HostURL.normalize(host.url) == nil)
                    .accessibilityIdentifier("save-host")
            }
        }
        .alert("Not a server address", isPresented: $invalid) {
            Button("OK", role: .cancel) {}
        }
    }

    private func save() {
        if isNew {
            if app.addHost(url: host.url, name: host.name) == nil { invalid = true; return }
        } else if !app.updateHost(host) {
            invalid = true
            return
        }
        dismiss()
    }
}
