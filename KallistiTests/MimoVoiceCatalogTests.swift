import Foundation
import Testing
@testable import Herald

@Suite("Mimo voice catalog covers every offered voice")
struct MimoVoiceCatalogTests {

    /// Exactly the voices offered by the Settings picker
    /// (SettingsScreen.swift:947) plus Mimo's cluster default.
    static let offered = ["Mia", "Chloe", "Milo", "Dean",
                          "冰糖", "茉莉", "苏打", "白桦", "mimo_default"]

    @Test("Every offered voice maps to a SpeechVoice case")
    func everyOfferedVoiceMaps() {
        for raw in Self.offered {
            #expect(SpeechVoice(rawValue: raw) != nil, "no SpeechVoice case for \(raw)")
        }
    }

    @Test("English voices keep their existing raw values")
    func englishVoicesUnchanged() {
        #expect(SpeechVoice.mia.rawValue == "Mia")
        #expect(SpeechVoice.chloe.rawValue == "Chloe")
        #expect(SpeechVoice.milo.rawValue == "Milo")
        #expect(SpeechVoice.dean.rawValue == "Dean")
    }

    @Test("mimo_default uses the documented wire value")
    func mimoDefaultRawValue() {
        #expect(SpeechVoice.mimoDefault.rawValue == "mimo_default")
    }
}
