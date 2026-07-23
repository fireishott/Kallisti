import CarPlay
import UIKit
import os

/// Manages the CarPlay scene lifecycle. When the vehicle connects,
/// we set up a `CPTabBarTemplate` with Talk and Chats tabs.
/// The voice template is presented modally when Talk Mode is active.
final class CarPlaySceneDelegate: UIResponder, CPTemplateApplicationSceneDelegate, CPInterfaceControllerDelegate {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "CarPlay")
    private var interfaceController: CPInterfaceController?
    private var voiceManager: CarPlayVoiceManager?
    private var conversationManager: CarPlayConversationListManager?

    // MARK: - CPTemplateApplicationSceneDelegate

    func templateApplicationScene(
        _ templateApplicationScene: CPTemplateApplicationScene,
        didConnect interfaceController: CPInterfaceController
    ) {
        self.interfaceController = interfaceController
        interfaceController.delegate = self

        // Voice tab: CPVoiceControlTemplate for Talk Mode
        let voiceManager = CarPlayVoiceManager(interfaceController: interfaceController)
        self.voiceManager = voiceManager
        let voiceTemplate = CPVoiceControlTemplate(
            voiceControlStates: voiceManager.currentVoiceControlStates()
        )
        voiceTemplate.activateVoiceControlState(withIdentifier: "idle")
        voiceManager.setVoiceTemplate(voiceTemplate)
        voiceManager.configure()

        // Conversations tab: CPMessageListItem list (Siri-managed voice I/O)
        let conversationManager = CarPlayConversationListManager(interfaceController: interfaceController)
        self.conversationManager = conversationManager
        conversationManager.configure()
        let chatsTemplate = conversationManager.listTemplate ?? CPListTemplate(title: "Chats", sections: [])

        // CPTabBarTemplate takes an array of CPTemplate objects directly
        let tabBar = CPTabBarTemplate(templates: [voiceTemplate, chatsTemplate])
        interfaceController.setRootTemplate(tabBar, animated: false)
    }

    func templateApplicationScene(
        _ templateApplicationScene: CPTemplateApplicationScene,
        didDisconnectInterfaceController interfaceController: CPInterfaceController
    ) {
        // Do NOT end the voice session — it continues on the phone.
        // Just release CarPlay-specific references.
        voiceManager?.tearDown()
        voiceManager = nil
        conversationManager?.tearDown()
        conversationManager = nil
        self.interfaceController = nil
    }

    // MARK: - CPInterfaceControllerDelegate

    func templateDidAppear(_ aTemplate: CPTemplate, animated: Bool) {
        Self.logger.debug("CarPlay template appeared: \(type(of: aTemplate))")
    }

    func templateDidDisappear(_ aTemplate: CPTemplate, animated: Bool) {
        Self.logger.debug("CarPlay template disappeared: \(type(of: aTemplate))")
    }
}
