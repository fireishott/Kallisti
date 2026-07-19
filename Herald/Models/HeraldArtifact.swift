import Foundation

/// The type of artifact displayed in the Canvas.
enum HeraldArtifactType: Codable, Equatable {
    case code(language: String)
    case markdown
    case svg

    private enum CodingKeys: String, CodingKey { case type, language }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)
        switch type {
        case "code":
            let lang = try c.decodeIfPresent(String.self, forKey: .language) ?? ""
            self = .code(language: lang)
        case "svg":
            self = .svg
        default:
            self = .markdown
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .code(let lang):
            try c.encode("code", forKey: .type)
            try c.encode(lang, forKey: .language)
        case .markdown:
            try c.encode("markdown", forKey: .type)
        case .svg:
            try c.encode("svg", forKey: .type)
        }
    }

    var displayName: String {
        switch self {
        case .code(let lang): return lang.isEmpty ? "Code" : lang
        case .markdown: return "Markdown"
        case .svg: return "SVG"
        }
    }
}

/// A persistent artifact shown in the Canvas panel.
struct HeraldArtifact: Codable, Identifiable {
    var id: UUID
    var sessionID: String
    var type: HeraldArtifactType
    var content: String
    var updatedAt: Date

    init(sessionID: String, type: HeraldArtifactType, content: String) {
        self.id = UUID()
        self.sessionID = sessionID
        self.type = type
        self.content = content
        self.updatedAt = Date()
    }
}
