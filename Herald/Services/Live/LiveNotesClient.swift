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
}

// MARK: - Request Types

struct NoteCreateRequest: Encodable {
    let title: String
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

struct NotesListResponse: Decodable {
    let data: [NoteDTO]
}

struct NoteResponse: Decodable {
    let data: NoteDTO
}

struct RunResponse: Decodable {
    let data: RunDTO
}

// MARK: - DTOs

struct NoteDTO: Decodable {
    let id: String
    let userId: String
    let title: String
    let folderId: String?
    let pinned: Bool
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
