import SwiftUI

@main
struct J3CodeApp: App {
    @State private var app = AppModel.launch()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(app)
                .onOpenURL { app.handle($0) }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { app.becameActive() }
        }
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("New Thread") {
                    if let host = app.activeConnections.first?.id { app.newThread = NewThreadTarget(host: host) }
                }
                .keyboardShortcut("n", modifiers: .command)
                .disabled(app.activeConnections.isEmpty)
            }
            CommandGroup(replacing: .appSettings) {
                Button("Settings…") { app.showingSettings = true }
                    .keyboardShortcut(",", modifiers: .command)
                Button("Connections…") { app.showingConnections = true }
                    .keyboardShortcut("k", modifiers: [.command, .shift])
            }
        }
    }
}

struct RootView: View {
    @Environment(AppModel.self) private var app
    @State private var columns = NavigationSplitViewVisibility.automatic

    var body: some View {
        @Bindable var app = app
        NavigationSplitView(columnVisibility: $columns) {
            ThreadListView()
        } detail: {
            if let ref = app.selection, let model = app.threadModel(ref) {
                ThreadScreen(model: model)
                    .id(ref)
            } else {
                ContentUnavailableView(
                    app.activeConnections.isEmpty ? "No servers" : "No thread selected",
                    systemImage: "bubble.left.and.text.bubble.right",
                    description: Text(app.activeConnections.isEmpty
                        ? "Add a j3 server in Connections."
                        : "Pick a thread or start a new one.")
                )
            }
        }
        .onChange(of: app.selection) { _, ref in app.releaseThreads(keeping: ref) }
        .sheet(isPresented: $app.showingConnections) {
            NavigationStack { ConnectionsView() }
        }
        .sheet(isPresented: $app.showingSettings) {
            NavigationStack { SettingsView() }
        }
        .sheet(item: $app.newThread) { target in
            NavigationStack { NewThreadView(target: target) }
        }
        .alert("Something went wrong", isPresented: Binding(
            get: { app.alert != nil },
            set: { if !$0 { app.alert = nil } }
        )) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(app.alert ?? "")
        }
    }
}
