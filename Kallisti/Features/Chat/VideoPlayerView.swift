import AVKit
import SwiftUI

/// Inline video player for direct MP4/MOV URLs.
struct VideoPlayerView: View {
    let url: URL
    @State private var player: AVPlayer?

    var body: some View {
        Group {
            if let player {
                VideoPlayer(player: player)
            } else {
                RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                    .fill(Design.Colors.surface)
                    .overlay {
                        ProgressView()
                            .tint(Design.Colors.secondaryForeground)
                    }
            }
        }
        .onAppear { player = AVPlayer(url: url) }
        .onDisappear {
            player?.pause()
            player = nil
        }
    }
}
