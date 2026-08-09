import SwiftUI
import WidgetKit

@main
struct KallistiWidgetBundle: WidgetBundle {
    var body: some Widget {
        KallistiLiveActivity()
        KallistiStatusWidget()
        KallistiHealthWidget()
    }
}
