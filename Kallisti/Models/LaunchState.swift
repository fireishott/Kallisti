import Foundation

/// Explicit launch outcome states replacing implicit Boolean behavior.
enum LaunchState: Equatable {
    /// App is currently initializing.
    case initializing
    /// App is ready to use.
    case ready
    /// Device is not paired.
    case unpaired
    /// Authentication failed but can be recovered by re-pairing.
    case authFailure
    /// Network/server error that can be retried.
    case networkFailure(String)
}
