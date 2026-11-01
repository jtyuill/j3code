import AVFoundation
import ExpoModulesCore
import UIKit

@ExpoModule("T3NativeControls")
public final class T3NativeControlsModule: Module {
  private let presentationSources = T3PresentationSources()
  private var videoPresentation: T3NativeVideoPresentation?
  private var filePresentation: T3NativeFilePresentation?

  public func definition() -> ModuleDefinition {
    Events("videoPlaybackChange")
    Constants {
      if #available(iOS 26.0, *) {
        return [
          "supportsWorkspaceColumns": NSClassFromString("RNSSplitHostComponentView") != nil,
        ]
      }
      return ["supportsWorkspaceColumns": false]
    }
    View(T3SidebarTabBarView.self) {
      ViewName("SidebarTabBar")
      Events("onSelectionChange", "onCompose")
      Prop("tabs") { (view: T3SidebarTabBarView, value: [String]) in view.tabs = value }
      Prop("selectedTab") { (view: T3SidebarTabBarView, value: String) in view.selectedTab = value }
      Prop("selectedColor") { (view: T3SidebarTabBarView, value: UIColor) in view.selectedColor = value }
      Prop("showsCompose") { (view: T3SidebarTabBarView, value: Bool) in view.showsCompose = value }
      Prop("compact") { (view: T3SidebarTabBarView, value: Bool) in view.compact = value }
      Prop("appearance") { (view: T3SidebarTabBarView, value: String) in view.appearance = value }
    }

    View(T3LayoutMetricsView.self) {
      ViewName("LayoutMetrics")
      Events("onMetricsChange")
    }

    View(T3ScrollEdgeBleedView.self) {
      ViewName("ScrollEdgeBleed")
    }

    AsyncFunction("presentVideo") { (url: URL, title: String, sourceIdentifier: String, identifier: String, promise: Promise) in
      try self.presentVideo(
        url: url,
        title: title,
        sourceIdentifier: sourceIdentifier,
        identifier: identifier,
        promise: promise
      )
    }.runOnQueue(.main)

    AsyncFunction("presentFile") { (url: URL, title: String, sourceIdentifier: String, identifier: String, promise: Promise) in
      try self.presentFile(url: url, title: title, sourceIdentifier: sourceIdentifier,
                           identifier: identifier, promise: promise)
    }.runOnQueue(.main)

    AsyncFunction("presentFiles") { (urls: [URL], titles: [String], selectedIndex: Int, sourceIdentifiers: [String], identifier: String, promise: Promise) in
      guard !urls.isEmpty, urls.count == titles.count, urls.count == sourceIdentifiers.count,
        urls.indices.contains(selectedIndex)
      else {
        throw URLError(.badURL)
      }
      let file = try self.makeFilePresentation(sourceIdentifiers: sourceIdentifiers, identifier: identifier,
                                               promise: promise)
      guard let presenter = self.appContext?.utilities?.currentViewController() else {
        file.dismiss()
        return
      }
      file.present(urls: urls, titles: titles, selectedIndex: selectedIndex, from: presenter)
    }.runOnQueue(.main)

    OnDestroy {
      let presentation = self.videoPresentation
      let file = self.filePresentation
      DispatchQueue.main.async {
        presentation?.dismiss()
        file?.dismiss()
      }
    }

    View(T3PresentationSourceView.self) {
      ViewName("PresentationSource")
      Prop("identifier") { (view: T3PresentationSourceView, identifier: String) in
        view.sources = self.presentationSources
        view.identifier = identifier
      }
    }

    AsyncFunction("shareFileFromSource") { (url: URL, title: String, identifier: String, promise: Promise) in
      try self.shareFile(url: url, title: title, sourceIdentifier: identifier, promise: promise)
    }.runOnQueue(.main)
  }

  // UIKit presenters are main-actor isolated rather than run on the JS thread
  // the macro would otherwise pick.
  @JS
  @MainActor
  func dismissVideo(identifier: String) async {
    dismissVideoPresentation(identifier: identifier)
  }

  // expo-video activates the session for inline playback but never deactivates
  // it. Any other category or mode means another feature (recording) owns it.
  @JS
  @MainActor
  func releaseVideoAudioSession() async {
    // The fullscreen presentation releases the session in its own teardown.
    guard videoPresentation == nil else { return }
    let session = AVAudioSession.sharedInstance()
    guard session.category == .playback, session.mode == .moviePlayback else { return }
    try? session.setActive(false, options: [.notifyOthersOnDeactivation])
  }

  @JS
  @MainActor
  func dismissFile(identifier: String) async {
    dismissFilePresentation(identifier: identifier)
  }

  @JS
  @MainActor
  func isProtectedDataAvailable() async -> Bool {
    UIApplication.shared.isProtectedDataAvailable
  }

  private func presentVideo(url: URL, title: String, sourceIdentifier: String, identifier: String, promise: Promise) throws {
    let isPlayableURL = url.isFileURL
      ? FileManager.default.isReadableFile(atPath: url.path)
      : (["https", "http"].contains(url.scheme?.lowercased() ?? "") && url.host != nil)
    guard videoPresentation == nil, filePresentation == nil,
      let presenter = appContext?.utilities?.currentViewController(),
      isPlayableURL
    else {
      throw NSError(
        domain: "T3NativeVideo",
        code: 2,
        userInfo: [NSLocalizedDescriptionKey: "The video preview is no longer available."]
      )
    }
    let presentation = T3NativeVideoPresentation(
      identifier: identifier, url: url, title: title,
      onPlaybackChange: { [weak self] isPlaying in
        self?.sendEvent("videoPlaybackChange", ["identifier": identifier, "isPlaying": isPlaying])
      },
      completion: { [weak self] error in
        self?.videoPresentation = nil
        if let error { promise.reject(error) } else { promise.resolve(nil) }
      }
    )
    videoPresentation = presentation
    presentation.present(from: presenter, sources: presentationSources, sourceIdentifier: sourceIdentifier)
  }

  private func dismissVideoPresentation(identifier: String) {
    if videoPresentation?.identifier == identifier { videoPresentation?.dismiss() }
  }

  private func makeFilePresentation(sourceIdentifiers: [String], identifier: String,
                                    promise: Promise) throws -> T3NativeFilePresentation {
    guard filePresentation == nil, videoPresentation == nil,
      appContext?.utilities?.currentViewController() != nil
    else { throw URLError(.cannotLoadFromNetwork) }
    let file = T3NativeFilePresentation(identifier: identifier, sources: presentationSources,
                                        sourceIdentifiers: sourceIdentifiers) { [weak self] error in
      self?.filePresentation = nil
      if let error { promise.reject(error) } else { promise.resolve(nil) }
    }
    filePresentation = file
    return file
  }

  private func presentFile(url: URL, title: String, sourceIdentifier: String,
                           identifier: String, promise: Promise) throws {
    let file = try makeFilePresentation(sourceIdentifiers: [sourceIdentifier], identifier: identifier, promise: promise)
    guard let presenter = appContext?.utilities?.currentViewController() else { file.dismiss(); return }
    file.present(url: url, title: title, from: presenter)
  }

  private func dismissFilePresentation(identifier: String) {
    if filePresentation?.identifier == identifier { filePresentation?.dismiss() }
  }

  private func shareFile(url: URL, title: String, sourceIdentifier: String, promise: Promise) throws {
    guard let presenter = appContext?.utilities?.currentViewController() else {
      throw NSError(
        domain: "T3NativePresentation",
        code: 2,
        userInfo: [NSLocalizedDescriptionKey: "The presenting screen is no longer open."]
      )
    }
    try presentFileShare(
      url: url,
      title: title,
      source: presentationSources.view(for: sourceIdentifier),
      presenter: presenter,
      promise: promise
    )
  }
}
