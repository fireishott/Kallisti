import SwiftUI

/// Lightweight keyword-based syntax tokenizer for code blocks.
/// No third-party dependencies — just a ~150-keyword switch per language.
struct SyntaxHighlighter {
    // Colors matched to Herald brand palette
    static let keywordColor  = Color(hex: 0xFF6B00)   // molten orange
    static let stringColor   = Color(hex: 0x90EE90)   // soft green
    static let commentColor  = Color(hex: 0x888888)   // muted grey
    static let numberColor   = Color(hex: 0xFFF5E0)   // white-hot
    static let defaultColor  = Color(hex: 0xF5F0E8)   // warm off-white

    private static let keywords: [String: Set<String>] = [
        "swift": ["func", "var", "let", "class", "struct", "enum", "protocol",
                  "import", "return", "if", "else", "guard", "for", "while",
                  "switch", "case", "break", "continue", "nil", "true", "false",
                  "self", "super", "init", "deinit", "extension", "override",
                  "final", "static", "async", "await", "throws", "throw", "try",
                  "catch", "in", "where", "typealias", "associatedtype",
                  "public", "private", "internal", "fileprivate", "open"],
        "python": ["def", "class", "import", "from", "return", "if", "elif",
                   "else", "for", "while", "try", "except", "finally", "with",
                   "as", "pass", "break", "continue", "None", "True", "False",
                   "and", "or", "not", "in", "is", "lambda", "yield", "async",
                   "await", "raise", "del", "global", "nonlocal", "assert"],
        "javascript": ["function", "const", "let", "var", "return", "if", "else",
                       "for", "while", "class", "import", "export", "default",
                       "async", "await", "try", "catch", "throw", "new", "this",
                       "null", "undefined", "true", "false", "typeof", "instanceof",
                       "switch", "case", "break", "continue", "from", "of"],
        "typescript": ["function", "const", "let", "var", "return", "if", "else",
                       "for", "while", "class", "import", "export", "default",
                       "async", "await", "try", "catch", "throw", "new", "this",
                       "null", "undefined", "true", "false", "type", "interface",
                       "enum", "namespace", "extends", "implements", "readonly",
                       "private", "public", "protected", "abstract", "switch"],
        "bash": ["if", "then", "else", "elif", "fi", "for", "do", "done",
                 "while", "case", "esac", "function", "return", "echo", "export",
                 "local", "readonly", "shift", "exit", "source", "true", "false"],
        "sql": ["SELECT", "FROM", "WHERE", "JOIN", "LEFT", "RIGHT", "INNER",
                "OUTER", "ON", "GROUP", "BY", "ORDER", "HAVING", "INSERT",
                "INTO", "VALUES", "UPDATE", "SET", "DELETE", "CREATE", "TABLE",
                "DROP", "ALTER", "ADD", "COLUMN", "INDEX", "PRIMARY", "KEY",
                "FOREIGN", "REFERENCES", "NOT", "NULL", "UNIQUE", "DEFAULT",
                "AND", "OR", "IN", "LIKE", "BETWEEN", "AS", "DISTINCT",
                "COUNT", "SUM", "AVG", "MAX", "MIN", "WITH", "UNION"],
    ]

    /// Returns an AttributedString with syntax coloring for the given language.
    /// Falls back to plain off-white text for unknown languages.
    static func tokenize(code: String, language: String?) -> AttributedString {
        let lang = language?.lowercased() ?? ""
        guard let kwSet = keywords[lang] else {
            var attr = AttributedString(code)
            attr.foregroundColor = defaultColor
            return attr
        }

        var result = AttributedString()
        let tokens = splitTokens(raw: code)
        for token in tokens {
            var attr = AttributedString(token)
            if kwSet.contains(token) {
                attr.foregroundColor = keywordColor
            } else if token.hasPrefix("//") || token.hasPrefix("#") {
                attr.foregroundColor = commentColor
            } else if token.hasPrefix("\"") || token.hasPrefix("'") || token.hasPrefix("`") {
                attr.foregroundColor = stringColor
            } else if Double(token) != nil {
                attr.foregroundColor = numberColor
            } else {
                attr.foregroundColor = defaultColor
            }
            result += attr
        }
        return result
    }

    private static func splitTokens(raw: String) -> [String] {
        var tokens: [String] = []
        var current = ""
        var i = raw.startIndex
        while i < raw.endIndex {
            let ch = raw[i]
            if ch.isLetter || ch == "_" || (ch.isNumber && !current.isEmpty) {
                current.append(ch)
            } else {
                if !current.isEmpty {
                    tokens.append(current)
                    current = ""
                }
                tokens.append(String(ch))
            }
            i = raw.index(after: i)
        }
        if !current.isEmpty { tokens.append(current) }
        return tokens
    }
}
