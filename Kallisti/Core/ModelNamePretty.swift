import Foundation

/// Humanizes raw gateway model IDs (e.g. "ds/deepseek-v4-pro") into clean
/// display names for the Hub model picker.
///
/// Two outputs:
///   - `prettyName(_:)`  -> "DeepSeek V4 Pro"   (top row)
///   - `familyName(_:)`  -> "DeepSeek"           (bottom row, after the provider)
///
/// Brand tokens are sourced from LiteLLM's model_prices_and_context_window.json
/// (BerriAI/litellm, MIT) which catalogs 3,000+ public cloud models across 123
/// providers. The humanizer itself is a pure string transform, so it stays
/// correct for models outside the catalog too (any prefix + any slug).
enum ModelNamePretty {
    private static let brands: [String: String] = [
        "deepseek": "DeepSeek", "minimax": "MiniMax", "openai": "OpenAI",
        "gpt": "GPT", "chatgpt": "ChatGPT", "claude": "Claude", "gemini": "Gemini",
        "llama": "Llama", "mistral": "Mistral", "qwen": "Qwen", "glm": "GLM",
        "kimi": "Kimi", "grok": "Grok", "moonshot": "Moonshot", "dalle": "DALL·E",
        "codestral": "Codestral", "mixtral": "Mixtral", "phi": "Phi",
        "nemotron": "Nemotron", "yi": "Yi", "olmo": "OLMo", "command": "Command",
        "titan": "Titan", "nova": "Nova", "jamba": "Jamba", "mimo": "MiMo",
        "gpt-oss": "GPT-OSS", "codex": "Codex", "devstral": "Devstral",
        "magistral": "Magistral", "seed": "Seed",
    ]

    private static let acronyms: Set<String> = ["ai", "ml", "api", "tts", "stt", "ocr", "rag", "fp8", "r1"]

    /// "ds/deepseek-v4-pro" -> "DeepSeek V4 Pro"
    static func prettyName(_ raw: String) -> String {
        let formatted = tokens(of: raw).compactMap { formatToken($0) }
        return formatted.isEmpty ? raw : formatted.joined(separator: " ")
    }

    /// Family display name from the slug's first recognizable brand token.
    /// "ds/deepseek-v4-pro" -> "DeepSeek", "openrouter/minimax-m3" -> "MiniMax".
    /// The prefix is ignored because it doesn't always match the family
    /// (openrouter/minimax-m3 is MiniMax, not OpenRouter).
    static func familyName(_ raw: String) -> String {
        for token in tokens(of: raw) {
            if let brand = brands[token.lowercased()] { return brand }
        }
        if let first = tokens(of: raw).first, !first.isEmpty {
            return first.prefix(1).uppercased() + first.dropFirst()
        }
        return raw
    }

    // MARK: - Internals

    /// Splits a model ID into its slug tokens. The provider prefix (anything
    /// before the first "/") is dropped, and the slug is split on "-", "_",
    /// ":", and spaces. "." is deliberately NOT a separator so version tokens
    /// (2.5, v3.2, m2.5) stay intact.
    private static func tokens(of raw: String) -> [String] {
        var slug = raw.split(separator: "/").last.map(String.init) ?? raw
        // Normalize "dall-e" so the brand token survives the dash split.
        slug = slug.replacingOccurrences(of: "dall-e", with: "dalle", options: .caseInsensitive)
        return slug
            .components(separatedBy: CharacterSet(charactersIn: "-_: "))
            .filter { !$0.isEmpty }
    }

    private static func formatToken(_ token: String) -> String? {
        let low = token.lowercased()
        if let brand = brands[low] { return brand }

        // Version tokens: v4 -> V4, v3.2 -> V3.2, m3 -> M3, m2.5 -> M2.5, k2.5 -> K2.5.
        if let first = low.first, ["v", "m", "k"].contains(first) {
            let rest = low.dropFirst()
            if !rest.isEmpty, rest.allSatisfy({ $0.isNumber || $0 == "." }) {
                return String(first).uppercased() + rest
            }
        }

        // Size suffixes: 70b -> 70B, 128k -> 128K, 128e -> 128E.
        if let last = low.last, ["b", "k", "m", "e"].contains(last) {
            let rest = low.dropLast()
            if !rest.isEmpty, rest.allSatisfy({ $0.isNumber || $0 == "." }) {
                return String(rest) + String(last).uppercased()
            }
        }

        // Numeric date/build stamps (>= 4 digits) drop out of the pretty name.
        if low.allSatisfy(\.isNumber), low.count >= 4 { return nil }

        // OpenAI o-series (o1, o3, o4) keeps its lowercase styling.
        if low.first == "o", low.count > 1, low.dropFirst().allSatisfy(\.isNumber) {
            return low
        }

        if acronyms.contains(low) { return low.uppercased() }

        return low.prefix(1).uppercased() + low.dropFirst()
    }
}
