import SwiftUI
import Foundation

/// Manages the active Canvas artifact — extracted from messages or edited by the user.
/// Persisted to UserDefaults keyed by sessionID.
@Observable
@MainActor
final class HeraldCanvasStore {
    var activeArtifact: KallistiArtifact?

    /// Live tool activities from the current streaming message. Set by
    /// ChatStore when toolActivity events arrive. Drives the Live tab in
    /// the canvas panel (build 78).
    var liveToolActivities: [ToolActivity] = []

    /// Selected tab in the canvas panel. Auto-switches to .live when
    /// liveToolActivities becomes non-empty.
    var activeTab: CanvasTab = .artifact

    /// True when there is at least one live tool activity to show.
    var isLiveActivityVisible: Bool { !liveToolActivities.isEmpty }

    enum CanvasTab: Hashable { case artifact, live }

    private let defaults = UserDefaults.standard
    private let storageKey = "herald.canvas.artifacts"

    /// Load the stored artifact for a given session on startup.
    func loadArtifact(for sessionID: String) {
        guard let data = defaults.data(forKey: storageKey + "." + sessionID),
              let artifact = try? JSONDecoder().decode(KallistiArtifact.self, from: data)
        else { return }
        activeArtifact = artifact
    }

    /// Open an artifact extracted from a message's first code block.
    func open(message: Message, sessionID: String) {
        let segments = parseMarkdownSegments(message.content)
        guard let codeBlock = segments.first(where: {
            if case .codeBlock = $0 { return true }
            return false
        }), case .codeBlock(_, let lang, let code) = codeBlock else { return }

        let type: KallistiArtifactType = (lang?.lowercased() == "svg") ? .svg
            : lang.map { .code(language: $0) } ?? .markdown
        let artifact = KallistiArtifact(
            sessionID: sessionID,
            type: type,
            content: code
        )
        activeArtifact = artifact
        persist(artifact)
    }

    /// Called from CanvasView when user edits content.
    func updateContent(_ newContent: String) {
        guard var artifact = activeArtifact else { return }
        artifact.content = newContent
        artifact.updatedAt = Date()
        activeArtifact = artifact
        persist(artifact)
    }

    func clear() {
        if let sessionID = activeArtifact?.sessionID {
            defaults.removeObject(forKey: storageKey + "." + sessionID)
        }
        activeArtifact = nil
    }

    private func persist(_ artifact: KallistiArtifact) {
        if let data = try? JSONEncoder().encode(artifact) {
            defaults.set(data, forKey: storageKey + "." + artifact.sessionID)
        }
    }
}
