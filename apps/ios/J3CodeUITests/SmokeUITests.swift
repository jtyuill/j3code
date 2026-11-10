import XCTest

/// The simulator smoke test (`contrib/ios/smoke.sh`): connects to a
/// `j3-test-server` through the deep link, opens its thread, sends a prompt,
/// answers the scripted omp's question and asks a side question (`/btw`).
///
/// The server's URL comes from `J3_SMOKE_URL` (`TEST_RUNNER_J3_SMOKE_URL`
/// for `xcodebuild test`); without it the test is skipped.
final class SmokeUITests: XCTestCase {
    private let wait: TimeInterval = 30

    override func setUp() {
        continueAfterFailure = false
    }

    func testPromptAndAsk() throws {
        guard let server = ProcessInfo.processInfo.environment["J3_SMOKE_URL"], !server.isEmpty else {
            throw XCTSkip("J3_SMOKE_URL is not set")
        }
        let app = XCUIApplication()
        app.launchArguments = ["-j3-reset"]
        app.launch()

        var link = URLComponents(string: "j3code://connections/new")!
        link.queryItems = [URLQueryItem(name: "host", value: server), URLQueryItem(name: "autoConnect", value: "1")]
        app.open(link.url!)
        // iOS may ask before opening the link.
        let open = XCUIApplication(bundleIdentifier: "com.apple.springboard").buttons["Open"]
        if open.waitForExistence(timeout: 5) { open.tap() }

        // The scripted server's thread, with the reply to its first prompt.
        let thread = app.descendants(matching: .any)["thread-Hello"].firstMatch
        XCTAssertTrue(thread.waitForExistence(timeout: wait), "the thread list shows the server's thread")
        thread.tap()
        XCTAssertTrue(text(app, containing: "Echo: Hello").waitForExistence(timeout: wait), "the first reply")

        let composer = app.textViews["composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: wait))
        composer.tap()
        composer.typeText("Please ask me something")
        app.buttons["send"].firstMatch.tap()

        let blue = app.buttons["option-blue"]
        XCTAssertTrue(blue.waitForExistence(timeout: wait), "the select request")
        blue.tap()
        XCTAssertTrue(text(app, containing: "You picked blue.").waitForExistence(timeout: wait), "the answer reached omp")
        XCTAssertFalse(app.buttons["option-blue"].exists, "the request closed")

        // A `/btw` draft is answered in its own sheet and is not a message.
        composer.tap()
        composer.typeText("/btw is it lunchtime")
        app.buttons["send"].firstMatch.tap()
        XCTAssertTrue(
            text(app, containing: "Aside: is it lunchtime").waitForExistence(timeout: wait), "the side answer"
        )
        XCTAssertFalse(text(app, containing: "Echo: /btw").exists, "the thread did not get the question")
        app.buttons["side-answer-close"].firstMatch.tap()
        XCTAssertTrue(composer.waitForExistence(timeout: wait), "the sheet closed")
        XCTAssertFalse(text(app, containing: "Aside: is it lunchtime").exists, "the answer went with it")
    }

    /// Read-only tour of a real server (`J3_BROWSE_URL`): connects, opens the
    /// newest thread and writes screenshots to `J3_SHOTS` (a directory on the
    /// Mac running the simulator). Changes nothing on the server.
    func testBrowse() throws {
        let env = ProcessInfo.processInfo.environment
        guard let server = env["J3_BROWSE_URL"], !server.isEmpty else {
            throw XCTSkip("J3_BROWSE_URL is not set")
        }
        let shots = env["J3_SHOTS"] ?? NSTemporaryDirectory()
        let app = XCUIApplication()
        app.launchArguments = ["-j3-reset"]
        app.launch()
        var link = URLComponents(string: "j3code://connections/new")!
        link.queryItems = [URLQueryItem(name: "host", value: server), URLQueryItem(name: "autoConnect", value: "1")]
        app.open(link.url!)
        let open = XCUIApplication(bundleIdentifier: "com.apple.springboard").buttons["Open"]
        if open.waitForExistence(timeout: 5) { open.tap() }

        let thread = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'thread-'")).firstMatch
        XCTAssertTrue(thread.waitForExistence(timeout: wait), "threads listed")
        shot(app, "\(shots)/list.png")
        thread.tap()
        sleep(4)
        shot(app, "\(shots)/thread.png")
        app.swipeDown()
        sleep(2)
        shot(app, "\(shots)/thread-up.png")
    }

    private func shot(_ app: XCUIApplication, _ path: String) {
        let data = app.screenshot().pngRepresentation
        try? data.write(to: URL(fileURLWithPath: path))
        let attachment = XCTAttachment(data: data, uniformTypeIdentifier: "public.png")
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    /// End to end on a real server (`J3_DEVICE_URL`), for the iPhone: adds
    /// the directory `J3_SCRATCH` (relative to the server user's home, which
    /// the path field starts with) as a project, starts a thread in it with a
    /// prompt, and waits for omp's answer. Screenshots are test attachments.
    func testNewThreadOnServer() throws {
        let env = ProcessInfo.processInfo.environment
        guard let server = env["J3_DEVICE_URL"], let scratch = env["J3_SCRATCH"] else {
            throw XCTSkip("J3_DEVICE_URL and J3_SCRATCH are not set")
        }
        let app = XCUIApplication()
        app.launchArguments = ["-j3-reset"]
        app.launch()
        var link = URLComponents(string: "j3code://connections/new")!
        link.queryItems = [URLQueryItem(name: "host", value: server), URLQueryItem(name: "autoConnect", value: "1")]
        app.open(link.url!)
        let open = XCUIApplication(bundleIdentifier: "com.apple.springboard").buttons["Open"]
        if open.waitForExistence(timeout: 5) { open.tap() }

        let anyThread = app.descendants(matching: .any)
            .matching(NSPredicate(format: "identifier BEGINSWITH 'thread-'")).firstMatch
        XCTAssertTrue(anyThread.waitForExistence(timeout: 60), "threads listed")
        shot(app, "\(NSTemporaryDirectory())/device-list.png")

        let name = (scratch as NSString).lastPathComponent
        let newInProject = app.buttons["New thread in \(name)"]
        // Projects without threads come last in a lazy list; searching for the
        // project's title brings its section to the top. The bottom search bar
        // takes focus only after it has expanded.
        let search = app.searchFields.firstMatch
        XCTAssertTrue(search.waitForExistence(timeout: wait))
        search.tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: wait))
        app.typeText(name)
        // Add the project only when an earlier run has not.
        if !newInProject.waitForExistence(timeout: 3) {
            app.buttons["list-menu"].tap()
            app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Add Project on'")).firstMatch.tap()
            let path = app.textFields["project-path"]
            XCTAssertTrue(path.waitForExistence(timeout: wait))
            path.tap()
            path.typeText(scratch)
            app.buttons["add-project"].tap()
        }
        XCTAssertTrue(newInProject.waitForExistence(timeout: wait), "the scratch project is listed")
        newInProject.tap()

        let composer = app.textViews["composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: wait))
        composer.tap()
        composer.typeText("What is 17 times 3? Answer with the number only.")
        app.buttons["start-thread"].tap()

        XCTAssertTrue(text(app, containing: "51").waitForExistence(timeout: 180), "omp answered")
        shot(app, "\(NSTemporaryDirectory())/device-thread.png")
    }

    private func text(_ app: XCUIApplication, containing text: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@ OR value CONTAINS %@", text, text))
            .firstMatch
    }
}
