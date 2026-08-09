import SafariServices
import SwiftUI

/// A SwiftUI representable that wraps SFSafariViewController for in-app browsing.
/// Used for TOS, Privacy Policy, and Support links so users can return to the app
/// without switching to Safari.
struct SafariView: UIViewControllerRepresentable {
    let url: URL

    func makeUIViewController(context: Context) -> SFSafariViewController {
        let config = SFSafariViewController.Configuration()
        config.entersReaderIfAvailable = false
        let controller = SFSafariViewController(url: url, configuration: config)
        controller.preferredControlTintColor = UIColor.systemOrange
        return controller
    }

    func updateUIViewController(_ uiViewController: SFSafariViewController, context: Context) {}
}
