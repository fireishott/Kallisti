//
//  WatchTurnView.swift
//  KallistiWatch
//
//  One line of the conversation. The wrist has room for a sentence, so a turn is
//  drawn as a bubble with the role's tint and nothing else.
//

import SwiftUI

struct WatchTurnView: View {
    let turn: WatchTurn

    var body: some View {
        Text(turn.text)
            .font(.footnote)
            .foregroundStyle(turn.role == .user ? Color.primary : Color.teal)
            .frame(maxWidth: .infinity, alignment: turn.role == .user ? .trailing : .leading)
            .accessibilityLabel(turn.role == .user ? "You said" : "Kallisti said")
            .accessibilityValue(turn.text)
    }
}
