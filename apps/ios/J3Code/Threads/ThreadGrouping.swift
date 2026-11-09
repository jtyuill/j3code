import Foundation

/// A project's threads in the thread list.
struct ProjectGroup: Identifiable, Equatable {
    let projectId: String
    let title: String
    let path: String?
    let threads: [ThreadRow]
    var id: String { projectId }
}

enum ThreadGrouping {
    /// Threads grouped by project. Threads are newest activity first; groups
    /// follow their newest thread, and projects without threads come last,
    /// by title. Archived threads are left out unless `archived`; `search`
    /// keeps threads whose title or last message contains it (and projects
    /// whose title does, with all their threads).
    static func groups(
        projects: [Project],
        threads: [ThreadRow],
        archived: Bool,
        search: String = ""
    ) -> [ProjectGroup] {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines)
        var byProject: [String: [ThreadRow]] = [:]
        for thread in threads where archived || thread.archivedAt == nil {
            byProject[thread.projectId, default: []].append(thread)
        }
        var groups: [ProjectGroup] = []
        var seen = Set<String>()
        for project in projects {
            seen.insert(project.id)
            groups.append(group(project.id, project.title, project.path, byProject[project.id] ?? [], query))
        }
        // Threads of a project the shell has not delivered yet.
        for (projectId, threads) in byProject where !seen.contains(projectId) {
            groups.append(group(projectId, "Unknown project", nil, threads, query))
        }
        if !query.isEmpty {
            groups.removeAll { $0.threads.isEmpty && !$0.title.localizedCaseInsensitiveContains(query) }
        }
        return groups.sorted { a, b in
            switch (a.threads.first.map(activity), b.threads.first.map(activity)) {
            case let (x?, y?): x != y ? x > y : a.title < b.title
            case (_?, nil): true
            case (nil, _?): false
            case (nil, nil): a.title.localizedCaseInsensitiveCompare(b.title) == .orderedAscending
            }
        }
    }

    private static func group(
        _ id: String, _ title: String, _ path: String?, _ threads: [ThreadRow], _ query: String
    ) -> ProjectGroup {
        var threads = threads.sorted { a, b in
            let (x, y) = (activity(a), activity(b))
            return x != y ? x > y : a.id < b.id
        }
        if !query.isEmpty, !title.localizedCaseInsensitiveContains(query) {
            threads = threads.filter {
                $0.title.localizedCaseInsensitiveContains(query)
                    || ($0.lastMessage?.text.localizedCaseInsensitiveContains(query) ?? false)
            }
        }
        return ProjectGroup(projectId: id, title: title, path: path, threads: threads)
    }

    /// When a thread last moved: its triage bump time (RFC 3339 strings of
    /// one format sort as their times do).
    static func activity(_ thread: ThreadRow) -> String {
        max(thread.triage.bumpedAt, thread.latestUserMessageAt ?? "", thread.createdAt)
    }
}
