import SwiftUI
import WidgetKit

/// Control Center widget bundle for Herald gateway management.
///
/// NOTE: iOS 26 beta SDK (26.5) removed `ControlWidgetBundle` from WidgetKit.
/// This is a minimal WidgetBundle placeholder; the Control Center widget types
/// remain in the target for re-enabling when the API stabilizes.
struct HeraldControlsBundle: WidgetBundle {
    @WidgetBundleBuilder
    var body: some Widget {
        // No widgets registered — will be re-enabled when iOS 26
        // Control Center API stabilizes.
    }
}
