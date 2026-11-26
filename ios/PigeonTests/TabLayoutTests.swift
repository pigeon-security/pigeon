import XCTest
@testable import Pigeon

final class TabLayoutTests: XCTestCase {
    private func areas(_ ids: String...) -> [Area] { ids.map { Area(id: $0, title: $0) } }

    func testDefaultsPinTheFirstThreeAreas() {
        let layout = TabLayout.defaults(for: areas("inbox", "overview", "site", "source", "ci"))
        XCTAssertEqual(layout.pinned, ["inbox", "overview", "site"])
        XCTAssertEqual(layout.overflow, ["source", "ci"])
    }

    func testDefaultsWithFewAreasPinThemAll() {
        let layout = TabLayout.defaults(for: areas("inbox", "site"))
        XCTAssertEqual(layout.pinned, ["inbox", "site"])
        XCTAssertEqual(layout.overflow, [])
    }

    func testResolvedDropsAreasTheManifestLacksAndAppendsNewOnes() {
        let stored = TabLayout(pinned: ["site", "platform", "inbox"], overflow: ["ci", "storage"])
        let layout = stored.resolved(for: areas("inbox", "site", "ci", "metrics"))
        XCTAssertEqual(layout.pinned, ["site", "inbox"])
        XCTAssertEqual(layout.overflow, ["ci", "metrics"])
    }

    func testResolvedFallsBackToDefaultsWhenNothingPinnedIsLeft() {
        let stored = TabLayout(pinned: ["platform"], overflow: ["inbox"])
        let layout = stored.resolved(for: areas("inbox", "overview", "site", "ci"))
        XCTAssertEqual(layout, TabLayout.defaults(for: areas("inbox", "overview", "site", "ci")))
    }

    func testResolvedRemovesDuplicatesAndCapsTheBar() {
        let stored = TabLayout(pinned: ["a", "a", "b", "c", "d"], overflow: ["b", "e"])
        let layout = stored.resolved(for: areas("a", "b", "c", "d", "e"))
        XCTAssertEqual(layout.pinned, ["a", "b", "c"])
        XCTAssertEqual(layout.overflow, ["e", "d"])
    }

    func testPinIsRefusedWhenTheBarIsFull() {
        var layout = TabLayout(pinned: ["a", "b", "c"], overflow: ["d"])
        layout.pin("d")
        XCTAssertEqual(layout.pinned, ["a", "b", "c"])
        XCTAssertEqual(layout.overflow, ["d"])
    }

    func testPinAppendsToTheBar() {
        var layout = TabLayout(pinned: ["a"], overflow: ["b", "c"])
        layout.pin("c")
        XCTAssertEqual(layout.pinned, ["a", "c"])
        XCTAssertEqual(layout.overflow, ["b"])
    }

    func testUnpinMovesToTheTopOfMoreAndKeepsOneInTheBar() {
        var layout = TabLayout(pinned: ["inbox", "site"], overflow: ["ci"])
        layout.unpin("inbox")
        XCTAssertEqual(layout.pinned, ["site"])
        XCTAssertEqual(layout.overflow, ["inbox", "ci"])
        layout.unpin("site")
        XCTAssertEqual(layout.pinned, ["site"])
    }

    func testMovedMatchesListOnMove() {
        XCTAssertEqual(TabLayout.moved(["a", "b", "c"], from: [0], to: 3), ["b", "c", "a"])
        XCTAssertEqual(TabLayout.moved(["a", "b", "c"], from: [2], to: 0), ["c", "a", "b"])
        XCTAssertEqual(TabLayout.moved(["a", "b", "c"], from: [0], to: 2), ["b", "a", "c"])
        XCTAssertEqual(TabLayout.moved(["a", "b", "c"], from: [1], to: 1), ["a", "b", "c"])
        XCTAssertEqual(TabLayout.moved(["a", "b", "c"], from: [7], to: 0), ["a", "b", "c"])
    }

    @MainActor
    func testStoreKeepsOneLayoutPerInstance() throws {
        let suite = "TabLayoutTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let all = areas("inbox", "overview", "site", "source")

        let store = TabLayoutStore(defaults: defaults)
        store.update("one", areas: all) { $0.unpin("inbox") }
        XCTAssertEqual(store.layout(for: "one", areas: all).pinned, ["overview", "site"])
        XCTAssertEqual(store.layout(for: "two", areas: all).pinned, ["inbox", "overview", "site"])

        let reopened = TabLayoutStore(defaults: defaults)
        XCTAssertEqual(reopened.layout(for: "one", areas: all).pinned, ["overview", "site"])
        reopened.reset("one")
        XCTAssertEqual(reopened.layout(for: "one", areas: all).pinned, ["inbox", "overview", "site"])
    }
}
