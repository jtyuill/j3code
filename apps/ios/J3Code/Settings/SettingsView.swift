import SwiftUI

struct SettingsView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        @Bindable var settings = app.settings
        Form {
            Section("Threads") {
                Toggle("Show archived threads", isOn: $settings.showArchived)
                Toggle("Expand thinking", isOn: $settings.expandThinking)
            }
            Section {
                Toggle("Return sends", isOn: $settings.returnSends)
            } header: {
                Text("Hardware keyboard")
            } footer: {
                Text(settings.returnSends
                    ? "Return sends; Shift-Return starts a new line."
                    : "⌘Return sends; Return starts a new line.")
            }
            Section("Servers") {
                Button("Connections…") {
                    dismiss()
                    app.showingConnections = true
                }
                ForEach(app.activeConnections) { connection in
                    if let info = connection.snapshot?.info {
                        LabeledContent(connection.name, value: "j3 \(info.version) · protocol \(info.protocol)")
                    }
                }
            }
            Section("About") {
                LabeledContent("Version", value: Bundle.main.version)
                Text("j3 is a client for omp coding agents running on your own machines.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done") { dismiss() }.keyboardShortcut(.defaultAction)
            }
        }
    }
}

private extension Bundle {
    var version: String {
        let short = infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"
        let build = infoDictionary?["CFBundleVersion"] as? String ?? "?"
        return "\(short) (\(build))"
    }
}
