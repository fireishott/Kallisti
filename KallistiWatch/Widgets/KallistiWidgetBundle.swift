//
//  KallistiWidgetBundle.swift
//  KallistiWatchWidgets
//
//  One widget in the extension. The bundle is the @main of this target; the app
//  keeps its own.
//

import SwiftUI
import WidgetKit

@main
struct KallistiWidgetBundle: WidgetBundle {
    var body: some Widget {
        KallistiComplication()
    }
}
