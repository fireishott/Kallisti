import SwiftUI

// MARK: - Device Class

/// Detects whether the current device is an iPad or iPhone.
enum DeviceClass {
    case phone
    case pad

    /// The device class for the current device.
    @MainActor
    static let current: DeviceClass = {
        switch UIDevice.current.userInterfaceIdiom {
        case .pad:  return .pad
        case .phone: return .phone
        default:    return .phone
        }
    }()

    /// `true` when running on an iPad.
    @MainActor static var isPad: Bool { current == .pad }

    /// `true` when running on an iPhone (or iPod touch).
    @MainActor static var isPhone: Bool { current == .phone }
}
