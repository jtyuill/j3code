import PhotosUI
import SwiftUI
import UIKit

/// The message composer: completion, attachment strip, text field and the
/// model and thinking pickers.
struct ComposerView<Actions: View>: View {
    @Bindable var model: ComposerModel
    let onSubmit: () -> Void
    @ViewBuilder let actions: () -> Actions
    @State private var picked: [PhotosPickerItem] = []

    init(model: ComposerModel, onSubmit: @escaping () -> Void, @ViewBuilder actions: @escaping () -> Actions) {
        self.model = model
        self.onSubmit = onSubmit
        self.actions = actions
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if model.completionOpen {
                completionList
            }
            if !model.uploads.isEmpty {
                attachmentStrip
            }
            textField
            bottomBar
        }
    }

    // MARK: Text

    private var textField: some View {
        ZStack(alignment: .topLeading) {
            ComposerTextEditor(model: model, parts: model.parts, onSubmit: onSubmit)
            if model.isEmpty {
                Text("Message")
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .allowsHitTesting(false)
                    .accessibilityHidden(true)
            }
        }
    }

    private var bottomBar: some View {
        HStack(spacing: 14) {
            PhotosPicker(selection: $picked, maxSelectionCount: nil, matching: .images) {
                Image(systemName: "photo")
                    .font(.title3)
                    .foregroundStyle(.secondary)
            }
            .accessibilityLabel("Add photos")
            .onChange(of: picked) { _, items in load(items) }
            modelPicker
            thinkingPicker
            Spacer(minLength: 0)
            actions()
        }
    }

    private func load(_ items: [PhotosPickerItem]) {
        guard !items.isEmpty else { return }
        picked = []
        Task { @MainActor in
            for item in items {
                if let data = try? await item.loadTransferable(type: Data.self) {
                    model.addImage(data, baseName: "Photo")
                }
            }
        }
    }

    // MARK: Completion

    private var completionList: some View {
        ScrollView(.vertical, showsIndicators: false) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(model.completionRows.indices, id: \.self) { index in
                    let completion = model.completionRows[index]
                    Button {
                        model.accept(completion)
                    } label: {
                        completionRow(completion, selected: index == model.completionSelection)
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .frame(maxHeight: 280)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
    }

    private func completionRow(_ completion: Completion, selected: Bool) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(completion.label)
                    .font(.subheadline)
                    .lineLimit(1)
                if let hint = completion.hint {
                    Text(hint)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            if let detail = completion.detail {
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(selected ? Color.accentColor.opacity(0.15) : Color.clear)
        .contentShape(Rectangle())
    }

    // MARK: Attachments

    private var attachmentStrip: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(alignment: .top, spacing: 8) {
                ForEach(model.uploads) { upload in
                    uploadTile(upload)
                }
            }
        }
    }

    private func uploadTile(_ upload: ComposerUpload) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            ZStack {
                if let thumbnail = upload.thumbnail {
                    Image(uiImage: thumbnail)
                        .resizable()
                        .scaledToFill()
                } else {
                    Color(.secondarySystemBackground)
                }
            }
            .frame(width: 56, height: 56)
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .overlay {
                if upload.isUploading {
                    ProgressView()
                        .controlSize(.small)
                        .padding(6)
                        .background(.thinMaterial, in: Circle())
                }
            }
            .overlay(alignment: .topTrailing) {
                Button {
                    model.remove(upload.id)
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 16))
                        .symbolRenderingMode(.palette)
                        .foregroundStyle(Color(.systemBackground), Color(.secondaryLabel))
                }
                .buttonStyle(.plain)
                .offset(x: 6, y: -6)
                .accessibilityLabel("Remove \(upload.name)")
            }
            if case let .failed(message) = upload.state {
                Text(message)
                    .font(.caption2)
                    .foregroundStyle(Color(.systemRed))
                    .frame(width: 56, alignment: .leading)
                    .lineLimit(2)
            }
        }
    }

    // MARK: Pickers

    @ViewBuilder
    private var modelPicker: some View {
        if let catalog = model.catalog, !catalog.models.isEmpty {
            Menu {
                modelOptions(catalog)
            } label: {
                Text(modelName(catalog))
                    .font(.footnote)
                    .lineLimit(1)
            }
            .accessibilityIdentifier("composer-model")
        } else {
            Text(model.model?.slug ?? "Model")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .lineLimit(1)
        }
    }

    @ViewBuilder
    private func modelOptions(_ catalog: Catalog) -> some View {
        ForEach(providers(of: catalog), id: \.self) { provider in
            Section(provider) {
                ForEach(catalog.models.filter { $0.provider == provider }, id: \.slug) { choice in
                    Button {
                        model.model = choice.ref(thinking: model.model?.thinking)
                    } label: {
                        if model.model?.slug == choice.slug {
                            Label(ComposerCompletion.displayName(choice), systemImage: "checkmark")
                        } else {
                            Text(ComposerCompletion.displayName(choice))
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var thinkingPicker: some View {
        if let ref = model.model, let catalog = model.catalog, let choice = catalog.model(ref),
           let levels = choice.thinking, !levels.isEmpty
        {
            Menu {
                ForEach(levels) { level in
                    Button {
                        model.setThinking(level)
                    } label: {
                        if ref.thinking == level {
                            Label(level.label, systemImage: "checkmark")
                        } else {
                            Text(level.label)
                        }
                    }
                }
            } label: {
                Text(ref.thinking?.label ?? "Thinking")
                    .font(.footnote)
                    .lineLimit(1)
            }
            .accessibilityIdentifier("composer-thinking")
        }
    }

    private func modelName(_ catalog: Catalog) -> String {
        guard let ref = model.model else { return "Model" }
        return catalog.name(of: ref)
    }

    private func providers(of catalog: Catalog) -> [String] {
        catalog.models.reduce(into: []) { result, choice in
            if !result.contains(choice.provider) { result.append(choice.provider) }
        }
    }
}
