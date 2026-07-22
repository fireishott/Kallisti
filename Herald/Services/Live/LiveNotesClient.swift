import CryptoKit
import Foundation
import os

/// Live client for the Notes API endpoints.
/// Handles note CRUD and run management via the relay.
actor LiveNotesClient {
    private let apiClient: RelayAPIClient
    private let accessTokenProvider: () async -> String?
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "notes-client")

    init(
        apiClient: RelayAPIClient,
        accessTokenProvider: @escaping () async -> String?
    ) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
    }

    // MARK: - Note CRUD

    func listNotes() async throws -> [NoteDTO] {
        let token = await accessTokenProvider()
        let response: NotesListResponse = try await apiClient.get(
            path: "notes",
            accessToken: token
        )
        return response.data
    }

    func createNote(title: String) async throws -> NoteDTO {
        let token = await accessTokenProvider()
        let body = NoteCreateRequest(title: title)
        let response: NoteResponse = try await apiClient.post(
            path: "notes",
            body: body,
            accessToken: token
        )
        return response.data
    }

    func getNote(id: String) async throws -> NoteDTO {
        let token = await accessTokenProvider()
        let response: NoteResponse = try await apiClient.get(
            path: "notes/\(id)",
            accessToken: token
        )
        return response.data
    }

    func updateNote(id: String, revision: Int, request: NoteUpdateRequest) async throws -> NoteDTO {
        let token = await accessTokenProvider()
        let requestBody = try RelayCoders.makeEncoder().encode(request)
        let response: NoteDTO = try await apiClient.patchWithHeaders(
            path: "notes/\(id)",
            body: requestBody,
            accessToken: token,
            additionalHeaders: ["If-Match": "\"\(revision)\""]
        )
        return response
    }

    func deleteNote(id: String) async throws {
        let token = await accessTokenProvider()
        let _: EmptyResponse = try await apiClient.delete(
            path: "notes/\(id)",
            accessToken: token
        )
    }

    func postRecognition(noteId: String, request: NoteRecognitionRequest) async throws -> NoteRecognitionDTO {
        let token = await accessTokenProvider()
        let response: NoteRecognitionResponse = try await apiClient.post(
            path: "notes/\(noteId)/recognitions",
            body: request,
            accessToken: token
        )
        return response.data
    }

    // MARK: - Run Management

    func createRun(noteId: String, clientRunId: UUID, request: EnrichmentRunRequest) async throws -> RunDTO {
        let token = await accessTokenProvider()
        let body = NoteRunCreateRequest(
            clientRunId: clientRunId.uuidString,
            sourceDrawingRevision: request.sourceDrawingRevision,
            sourceTextRevision: request.sourceTextRevision,
            directives: request.directives.map { DirectiveRequest(id: $0.id, command: $0.command.rawValue, arguments: $0.arguments) }
        )
        let response: RunResponse = try await apiClient.post(
            path: "notes/\(noteId)/runs",
            body: body,
            accessToken: token
        )
        return response.data
    }

    func getRun(id: String) async throws -> RunDTO {
        let token = await accessTokenProvider()
        let response: RunResponse = try await apiClient.get(
            path: "note-runs/\(id)",
            accessToken: token
        )
        return response.data
    }

    func cancelRun(id: String) async throws -> RunDTO {
        let token = await accessTokenProvider()
        let response: RunResponse = try await apiClient.post(
            path: "note-runs/\(id)/cancel",
            accessToken: token
        )
        return response.data
    }

    func getRunEvents(runId: String, lastEventID: String? = nil) async throws -> [RunEventDTO] {
        let token = await accessTokenProvider()
        var request = try await MainActor.run {
            try apiClient.makeRequest(
                path: "note-runs/\(runId)/events",
                method: "GET",
                accessToken: token,
                body: nil
            )
        }
        if let lastEventID, !lastEventID.isEmpty {
            request.setValue(lastEventID, forHTTPHeaderField: "Last-Event-ID")
        }
        let response: RunEventsResponse = try await apiClient.sendRequest(request)
        return response.data
    }
}

// MARK: - Request Types

struct NoteCreateRequest: Encodable {
    let title: String
}

struct NoteUpdateRequest: Encodable {
    let title: String?
    let folderId: String?
    let pinned: Bool?
}

struct NoteRecognitionRequest: Encodable {
    let drawingRevision: Int
    let engine: String
    let engineVersion: String?
    let languages: String?
    let rawText: String
    let userCorrectedText: String?
}

struct NoteRunCreateRequest: Encodable {
    let clientRunId: String
    let sourceDrawingRevision: Int
    let sourceTextRevision: Int
    let directives: [DirectiveRequest]
}

struct DirectiveRequest: Encodable {
    let id: String
    let command: String
    let arguments: String
}

// MARK: - Response Types

private struct EmptyResponse: Decodable, Sendable {}

struct NotesListResponse: Decodable {
    let data: [NoteDTO]
}

struct NoteResponse: Decodable {
    let data: NoteDTO
}

struct NoteRecognitionResponse: Decodable {
    let data: NoteRecognitionDTO
}

struct RunResponse: Decodable {
    let data: RunDTO
}

struct RunEventsResponse: Decodable, Sendable {
    let data: [RunEventDTO]
}

// MARK: - DTOs

struct NoteDTO: Decodable {
    let id: String
    let userId: String
    let title: String
    let folderId: String?
    let pinned: Bool
    let revision: Int
    let currentDrawingRevision: Int
    let currentTextRevision: Int
    let createdAt: String?
    let updatedAt: String?
    let deletedAt: String?

    func toLocal() -> HeraldNote {
        HeraldNote(
            id: UUID(uuidString: id) ?? UUID(),
            title: title,
            folderId: folderId.flatMap(UUID.init),
            pinned: pinned,
            currentDrawingRevision: currentDrawingRevision,
            currentTextRevision: currentTextRevision,
            createdAt: ISO8601DateFormatter().date(from: createdAt ?? "") ?? .now,
            updatedAt: ISO8601DateFormatter().date(from: updatedAt ?? "") ?? .now,
            deletedAt: deletedAt.flatMap { ISO8601DateFormatter().date(from: $0) }
        )
    }
}

struct RunDTO: Decodable {
    let id: String
    let userId: String
    let noteId: String
    let clientRunId: String
    let sourceDrawingRevision: Int
    let sourceTextRevision: Int
    let requestedDirectives: [[String: String]]
    let status: String
    let attempt: Int
    let leaseExpiresAt: String?
    let errorText: String?
    let createdAt: String?
    let completedAt: String?

    func toLocal() -> NoteRunStatus {
        NoteRunStatus(
            id: UUID(uuidString: id) ?? UUID(),
            noteId: UUID(uuidString: noteId) ?? UUID(),
            clientRunId: UUID(uuidString: clientRunId) ?? UUID(),
            status: NoteRunStatus.Status(rawValue: status) ?? .queued,
            attempt: attempt,
            errorText: errorText,
            createdAt: ISO8601DateFormatter().date(from: createdAt ?? "") ?? .now,
            completedAt: completedAt.flatMap { ISO8601DateFormatter().date(from: $0) }
        )
    }
}

// MARK: - Additional DTOs

struct NoteRecognitionDTO: Decodable {
    let id: String
    let noteId: String
    let drawingRevision: Int
    let engine: String
    let engineVersion: String?
    let languages: String?
    let rawText: String
    let userCorrectedText: String?
    let createdAt: String?
}

struct RunEventDTO: Decodable, Sendable {
    let id: String
    let runId: String
    let seq: Int
    let attempt: Int
    let sourceSeq: Int?
    let type: String
    let payload: [String: AnyCodable]?
    let createdAt: String?
}

enum AnyCodable: Decodable, Sendable {
    case string(String)
    case int(Int)
    case double(Double)
    case bool(Bool)
    case array([AnyCodable])
    case dictionary([String: AnyCodable])
    case null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode(Int.self) {
            self = .int(value)
        } else if let value = try? container.decode(Double.self) {
            self = .double(value)
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode([AnyCodable].self) {
            self = .array(value)
        } else if let value = try? container.decode([String: AnyCodable].self) {
            self = .dictionary(value)
        } else {
            self = .null
        }
    }
}

// MARK: - Local Types

struct NoteRunStatus {
    let id: UUID
    let noteId: UUID
    let clientRunId: UUID
    let status: Status
    let attempt: Int
    let errorText: String?
    let createdAt: Date
    let completedAt: Date?

    enum Status: String {
        case queued
        case claimed
        case completed
        case failed
        case cancelled
    }
}

struct EnrichmentRunRequest {
    let sourceDrawingRevision: Int
    let sourceTextRevision: Int
    let directives: [NoteDirective]
}
