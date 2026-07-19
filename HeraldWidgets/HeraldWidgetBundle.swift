import SwiftUI
import WidgetKit

@main
struct HeraldWidgetBundle: WidgetBundle {
    var body: some Widget {
        HeraldLiveActivity()
        HeraldStatusWidget()
        HeraldHealthWidget()
    }
}
