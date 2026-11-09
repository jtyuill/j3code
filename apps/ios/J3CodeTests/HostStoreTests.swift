import XCTest
@testable import J3Code

@MainActor
final class HostStoreTests: XCTestCase {
    private func defaults() -> UserDefaults {
        let name = "j3-tests-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }

    func testNormalizesAddresses() {
        XCTAssertEqual(HostURL.normalize("shieldy"), "http://shieldy:3780")
        XCTAssertEqual(HostURL.normalize(" shieldy:3790/ "), "http://shieldy:3790")
        XCTAssertEqual(HostURL.normalize("http://100.106.192.2:3790/api/v1"), "http://100.106.192.2:3790")
        XCTAssertEqual(HostURL.normalize("https://j3.example"), "https://j3.example")
        XCTAssertNil(HostURL.normalize(""))
        XCTAssertNil(HostURL.normalize("ftp://x"))
    }

    func testAddRenameDisableRemovePersist() {
        let defaults = defaults()
        let store = HostStore(defaults: defaults)
        let host = store.add(url: "shieldy:3780")!
        XCTAssertEqual(host.name, "shieldy")
        XCTAssertTrue(host.enabled)
        // The same URL is not added twice.
        XCTAssertEqual(store.add(url: "http://shieldy:3780")?.id, host.id)
        store.add(url: "revived", name: "Revived")

        var edited = host
        edited.name = "Shieldy"
        edited.url = "shieldy:3790"
        XCTAssertTrue(store.update(edited))
        store.setEnabled(host.id, false)

        let reloaded = HostStore(defaults: defaults)
        XCTAssertEqual(reloaded.hosts.map(\.name), ["Shieldy", "Revived"])
        XCTAssertEqual(reloaded.hosts[0].url, "http://shieldy:3790")
        XCTAssertFalse(reloaded.hosts[0].enabled)

        reloaded.remove(host.id)
        XCTAssertEqual(HostStore(defaults: defaults).hosts.map(\.name), ["Revived"])
    }

    func testRejectsInvalidEdits() {
        let store = HostStore(defaults: defaults())
        var host = store.add(url: "shieldy")!
        host.url = "::"
        XCTAssertFalse(store.update(host))
        XCTAssertEqual(store.hosts[0].url, "http://shieldy:3780")
    }

    func testParsesDeepLinks() {
        XCTAssertEqual(
            DeepLink(URL(string: "j3code://connections/new?host=shieldy:3790&autoConnect=1")!),
            .addHost(url: "shieldy:3790", name: nil, autoConnect: true)
        )
        XCTAssertEqual(
            DeepLink(URL(string: "j3code://connections/new?host=http%3A%2F%2Fx%3A1&name=Box")!),
            .addHost(url: "http://x:1", name: "Box", autoConnect: false)
        )
        XCTAssertNil(DeepLink(URL(string: "j3code://connections/new")!))
        XCTAssertNil(DeepLink(URL(string: "j3code://elsewhere")!))
        XCTAssertNil(DeepLink(URL(string: "https://connections/new?host=x")!))
    }

    func testDeepLinkAddsAndConnectsHost() {
        let defaults = defaults()
        let app = AppModel(store: HostStore(defaults: defaults), settings: AppSettings(defaults: defaults))
        XCTAssertTrue(app.showingConnections, "no hosts: connections open")
        app.handle(URL(string: "j3code://connections/new?host=127.0.0.1:9&autoConnect=1")!)
        XCTAssertEqual(app.store.hosts.map(\.url), ["http://127.0.0.1:9"])
        XCTAssertEqual(app.activeConnections.count, 1)
        XCTAssertFalse(app.showingConnections)

        app.setEnabled(app.store.hosts[0].id, false)
        XCTAssertTrue(app.activeConnections.isEmpty)

        app.handle(URL(string: "j3code://connections/new?host=other:1")!)
        XCTAssertEqual(app.addHostDraft?.url, "other:1")
        XCTAssertEqual(app.store.hosts.count, 1, "without autoConnect the form opens instead")
    }
}
