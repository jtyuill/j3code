import XCTest
@testable import J3Code

private let model = ModelRef(provider: "p", id: "m", thinking: nil)

private func triage(_ bumped: String) -> Triage {
    Triage(
        visitedAt: nil, settled: nil, settledAt: nil, unsettledAt: nil, snoozedUntil: nil,
        snoozedAt: nil, lastWakeAt: nil, pinnedAt: nil, pinOrder: nil, activeOrder: nil,
        autoSettleOffAt: nil, bumpedAt: bumped
    )
}

private func thread(_ id: String, project: String, bumped: String, archived: Bool = false,
                    title: String? = nil) -> ThreadRow
{
    ThreadRow(
        id: id, projectId: project, title: title ?? id, createdAt: "2026-01-01T00:00:00.000Z",
        updatedAt: bumped, archivedAt: archived ? bumped : nil, status: .idle, model: model,
        mode: .default, activeTurnId: nil, pendingRequest: nil, queued: 0, lastMessage: nil,
        latestTurn: nil, latestUserMessageAt: nil, goal: nil, triage: triage(bumped)
    )
}

private func project(_ id: String, _ title: String) -> Project {
    Project(id: id, title: title, path: "/p/\(id)", defaultModel: nil,
            createdAt: "2026-01-01T00:00:00.000Z", updatedAt: "2026-01-01T00:00:00.000Z")
}

final class ThreadGroupingTests: XCTestCase {
    let projects = [project("a", "Alpha"), project("b", "Beta"), project("c", "Gamma")]
    let threads = [
        thread("a1", project: "a", bumped: "2026-11-01T10:00:00.000Z"),
        thread("a2", project: "a", bumped: "2026-11-03T10:00:00.000Z"),
        thread("b1", project: "b", bumped: "2026-11-05T10:00:00.000Z", title: "Fix the parser"),
        thread("b2", project: "b", bumped: "2026-11-09T10:00:00.000Z", archived: true),
    ]

    func testGroupsByProjectNewestFirst() {
        let groups = ThreadGrouping.groups(projects: projects, threads: threads, archived: false)
        XCTAssertEqual(groups.map(\.title), ["Beta", "Alpha", "Gamma"])
        XCTAssertEqual(groups[0].threads.map(\.id), ["b1"])
        XCTAssertEqual(groups[1].threads.map(\.id), ["a2", "a1"])
        XCTAssertTrue(groups[2].threads.isEmpty)
    }

    func testArchivedThreadsWhenAsked() {
        let groups = ThreadGrouping.groups(projects: projects, threads: threads, archived: true)
        XCTAssertEqual(groups[0].threads.map(\.id), ["b2", "b1"])
    }

    func testSearchKeepsMatchingThreadsAndProjects() {
        let byThread = ThreadGrouping.groups(projects: projects, threads: threads, archived: false, search: "parser")
        XCTAssertEqual(byThread.map(\.title), ["Beta"])
        XCTAssertEqual(byThread[0].threads.map(\.id), ["b1"])
        let byProject = ThreadGrouping.groups(projects: projects, threads: threads, archived: false, search: "alp")
        XCTAssertEqual(byProject.map(\.title), ["Alpha"])
        XCTAssertEqual(byProject[0].threads.count, 2)
    }

    func testThreadsOfUnknownProjectsAreKept() {
        let groups = ThreadGrouping.groups(
            projects: [], threads: [thread("x", project: "zz", bumped: "2026-11-01T00:00:00.000Z")], archived: false
        )
        XCTAssertEqual(groups.map(\.projectId), ["zz"])
    }
}

private func request(_ kind: RequestKind, _ questions: [Question]) -> Request {
    Request(
        id: "r", threadId: "t", turnId: "u", kind: kind, status: .open, title: nil, questions: questions,
        answers: nil, openedAt: "2026-11-09T00:00:00.000Z", expiresAt: nil, closedAt: nil
    )
}

private func question(_ id: String, options: [String] = [], multiple: Bool = false,
                      allowOther: Bool = false, default value: String? = nil) -> Question
{
    Question(
        id: id, text: "Q \(id)", header: nil,
        options: options.map { QuestionOption(label: $0, description: nil) },
        multiple: multiple, allowOther: allowOther, default: value
    )
}

final class RequestDraftTests: XCTestCase {
    func testSelectAnswersWithTheChosenLabel() {
        let req = request(.select, [question("q", options: ["red", "blue"])])
        XCTAssertTrue(RequestDraft.answersOnTap(req))
        var draft = RequestDraft(req)
        XCTAssertFalse(draft.isComplete)
        draft.toggle("blue", in: req.questions[0])
        XCTAssertEqual(draft.answers, [Answer(questionId: "q", values: ["blue"], other: nil)])
    }

    func testConfirmOffersYesAndNo() {
        let req = request(.confirm, [question("q")])
        XCTAssertEqual(RequestDraft.options(of: req.questions[0], kind: .confirm).map(\.label), ["Yes", "No"])
        XCTAssertTrue(RequestDraft.answersOnTap(req))
    }

    func testInputSendsTheTextAndKeepsTheDefault() {
        let req = request(.input, [question("q", default: "draft")])
        var draft = RequestDraft(req)
        XCTAssertEqual(draft.answers, [Answer(questionId: "q", values: ["draft"], other: nil)])
        draft.setText("final", for: req.questions[0])
        XCTAssertEqual(draft.answers.first?.values, ["final"])
        XCTAssertTrue(RequestDraft.takesText(req.questions[0], kind: .input))
    }

    func testAskCollectsLabelsAndOther() {
        let multi = question("a", options: ["x", "y", "z"], multiple: true, allowOther: true)
        let single = question("b", options: ["yes", "no"])
        let req = request(.ask, [multi, single])
        XCTAssertFalse(RequestDraft.answersOnTap(req))
        var draft = RequestDraft(req)
        draft.toggle("x", in: multi)
        draft.toggle("z", in: multi)
        draft.toggle("x", in: multi)
        draft.setText("  also w ", for: multi)
        XCTAssertFalse(draft.isComplete, "b is unanswered")
        draft.toggle("no", in: single)
        XCTAssertTrue(draft.isComplete)
        XCTAssertEqual(draft.answers, [
            Answer(questionId: "a", values: ["z"], other: "also w"),
            Answer(questionId: "b", values: ["no"], other: nil),
        ])
    }

    func testSelectWithOtherSendsTheTypedValue() {
        let req = request(.select, [question("q", options: ["a"], allowOther: true)])
        XCTAssertFalse(RequestDraft.answersOnTap(req))
        var draft = RequestDraft(req)
        draft.toggle("a", in: req.questions[0])
        draft.setText("custom", for: req.questions[0])
        XCTAssertEqual(draft.answers.first?.values, ["custom"])
    }
}
