import SwiftUI

/// The conversation feed: the thread's items, a working indicator and any
/// turn error. It opens on the newest row and follows new output while the
/// reader stays at the bottom.
struct FeedView: View {
    let thread: ThreadModel
    @State private var position = ScrollPosition(idType: String.self)
    @State private var pinned = true
    @State private var router = FeedRouter()

    init(thread: ThreadModel) {
        self.thread = thread
    }

    var body: some View {
        @Bindable var router = router
        let errors = turnErrors()
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 10) {
                ForEach(thread.items) { item in
                    ItemRow(thread: thread, item: item, router: router)
                    if let error = errors[item.id] {
                        TurnErrorCard(error: error)
                    }
                }
                if thread.isActive {
                    working
                }
                // Scrolling targets this marker: a lazy stack realizes an id
                // it scrolls to, where an edge offset uses estimated heights.
                Color.clear.frame(height: 1).id(Self.end)
            }
            .scrollTargetLayout()
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .defaultScrollAnchor(.bottom, for: .initialOffset)
        .scrollPosition($position, anchor: .bottom)
        .onScrollGeometryChange(for: Bool.self) { geometry in
            geometry.contentOffset.y + geometry.containerSize.height
                >= geometry.contentSize.height - 40
        } action: { _, atBottom in
            if pinned != atBottom { pinned = atBottom }
        }
        // Follow the tail: a row arrived or the last row changed. Comparing
        // these instead of the whole array keeps streaming updates from
        // re-running the scroll on every unrelated change.
        .onChange(of: thread.items.count) { _, _ in follow() }
        .onChange(of: thread.items.last) { _, _ in follow() }
        .task(id: thread.items.isEmpty) {
            if !thread.items.isEmpty { position.scrollTo(id: Self.end, anchor: .bottom) }
        }
        .overlay(alignment: .bottomTrailing) {
            if !pinned, !thread.items.isEmpty {
                jumpButton
            }
        }
        .sheet(item: $router.sheet) { sheet in
            sheetContent(sheet)
        }
        .modifier(MediaPresenting())
    }

    private static let end = "feed-end"

    /// Scrolls to the newest row unless the reader scrolled away.
    private func follow() {
        guard pinned else { return }
        position.scrollTo(id: Self.end, anchor: .bottom)
    }

    /// The active turn's indicator, after the last row.
    private var working: some View {
        HStack(spacing: 8) {
            Text("Working…")
                .font(.callout)
                .foregroundStyle(.secondary)
            Spacer(minLength: 0)
            ProgressView()
                .controlSize(.small)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Working")
    }

    private var jumpButton: some View {
        Button {
            pinned = true
            position.scrollTo(id: Self.end, anchor: .bottom)
        } label: {
            Image(systemName: "arrow.down")
                .font(.callout.weight(.semibold))
                .foregroundStyle(Color.primary)
                .frame(width: 36, height: 36)
                .background(.regularMaterial, in: Circle())
                .overlay(Circle().strokeBorder(Color(.separator)))
        }
        .buttonStyle(.plain)
        .padding(12)
        .accessibilityLabel("Scroll to latest")
    }

    @ViewBuilder
    private func sheetContent(_ sheet: FeedRouter.Sheet) -> some View {
        switch sheet {
        case let .tool(itemId):
            if let item = thread.items.first(where: { $0.id == itemId }) {
                ToolDetailView(thread: thread, item: item)
                    .modifier(MediaPresenting())
            } else {
                ContentUnavailableView("Tool call unavailable", systemImage: "wrench.and.screwdriver")
            }
        case let .transcript(_, subagentId, agent):
            TranscriptView(thread: thread, subagentId: subagentId, agent: agent)
                .modifier(MediaPresenting())
        }
    }

    /// Failed turns' errors, keyed by the id of the turn's last item.
    private func turnErrors() -> [String: TurnError] {
        let failed = (thread.snapshot?.turns ?? []).filter { $0.error != nil }
        guard !failed.isEmpty else { return [:] }
        var last: [String: String] = [:]
        for item in thread.items { last[item.turnId] = item.id }
        var errors: [String: TurnError] = [:]
        for turn in failed {
            if let itemId = last[turn.id], let error = turn.error { errors[itemId] = error }
        }
        return errors
    }
}
