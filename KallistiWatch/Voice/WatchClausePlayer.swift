//
//  WatchClausePlayer.swift
//  KallistiWatch
//
//  Plays the answer in the order it was spoken: one WAV per clause, queued, so
//  the first words start while the rest is still arriving.
//

import AVFoundation
import Foundation

@MainActor
@Observable
final class WatchClausePlayer {
    private var player: AVAudioPlayer?
    private var queue: [Data] = []
    private var playing = false
    private(set) var lastErrorWord: String?

    /// Queue one clause. It plays when the clause before it finishes.
    func enqueue(_ wav: Data) {
        guard !wav.isEmpty else { return }
        queue.append(wav)
        pump()
    }

    /// Stop now, and drop what has not started. A barge-in keeps no backlog: the
    /// person has already heard what they are going to hear.
    func stop() {
        queue.removeAll()
        player?.stop()
        player = nil
        playing = false
    }

    private func pump() {
        guard !playing, let next = queue.first else { return }
        queue.removeFirst()
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.playback, mode: .spokenAudio)
            try session.setActive(true)
            let player = try AVAudioPlayer(data: next)
            player.prepareToPlay()
            self.player = player
            playing = true
            player.play()
            lastErrorWord = nil
            // AVAudioPlayer has no completion callback on watchOS that survives
            // the class being replaced, so the next clause is polled in.
            Task { [weak self] in
                while let self, self.playing {
                    if self.player?.isPlaying == false {
                        self.playing = false
                        self.player = nil
                        self.pump()
                        return
                    }
                    try? await Task.sleep(for: .milliseconds(100))
                }
            }
        } catch {
            playing = false
            lastErrorWord = "audio_unavailable"
        }
    }
}
