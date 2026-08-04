import PhotosUI
import SwiftUI

/// Chat wallpaper picker, pushed from `SettingsScreen`'s Appearance section.
///
/// Presets render via `ChatWallpaperBackground` — the same rendering primitive
/// `ChatScreen` uses for the live background — so each tile is a live,
/// accurate thumbnail rather than a separately-maintained preview asset.
struct WallpaperPickerSheet: View {
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(ThemeManager.self) private var themeManager

    @State private var selectedItem: PhotosPickerItem?
    @State private var isImporting = false
    @State private var importError = false

    private let presets: [ChatWallpaper] = [
        .default, .gradient1, .gradient2, .gradient3, .gradient4,
        .texture1, .texture2, .solid
    ]

    private let columns = [
        GridItem(.flexible(), spacing: Design.Spacing.sm),
        GridItem(.flexible(), spacing: Design.Spacing.sm),
        GridItem(.flexible(), spacing: Design.Spacing.sm)
    ]

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            ScrollView {
                VStack(alignment: .leading, spacing: Design.Spacing.lg) {
                    LazyVGrid(columns: columns, spacing: Design.Spacing.md) {
                        ForEach(presets) { wallpaper in
                            wallpaperTile(wallpaper)
                        }
                    }

                    Divider()
                        .overlay(Design.Colors.divider)

                    photoPickerRow
                }
                .padding(Design.Spacing.md)
            }
        }
        .navigationTitle("Chat Wallpaper")
        .navigationBarTitleDisplayMode(.inline)
        .onChange(of: selectedItem) { _, newItem in
            guard let newItem else { return }
            isImporting = true
            Task {
                defer {
                    isImporting = false
                    selectedItem = nil
                }
                guard let data = try? await newItem.loadTransferable(type: Data.self) else {
                    importError = true
                    return
                }
                guard let wallpaperData = Self.prepareWallpaperData(data) else {
                    importError = true
                    return
                }
                settingsStore.settings.chatWallpaper = .custom(wallpaperData)
            }
        }
        .alert("Couldn't Load Photo", isPresented: $importError) {
            Button("OK", role: .cancel) {}
        } message: {
            Text("That image couldn't be used as a wallpaper. Try a different photo.")
        }
    }

    // MARK: - Preset tile

    private func wallpaperTile(_ wallpaper: ChatWallpaper) -> some View {
        let isSelected = settingsStore.settings.chatWallpaper == wallpaper

        return Button {
            withAnimation(Design.Motion.quickResponse) {
                settingsStore.settings.chatWallpaper = wallpaper
            }
        } label: {
            VStack(spacing: Design.Spacing.xxs) {
                ChatWallpaperBackground(wallpaper: wallpaper, tint: themeManager.preset.accent)
                    .frame(height: 90)
                    .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
                    .overlay(
                        RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                            .strokeBorder(
                                isSelected ? Design.Brand.accent : Color.clear,
                                lineWidth: 3
                            )
                    )
                    .overlay(alignment: .topTrailing) {
                        if isSelected {
                            Image(systemName: "checkmark.circle.fill")
                                .font(.system(size: 16, weight: .semibold))
                                .foregroundStyle(.white, Design.Brand.accent)
                                .padding(6)
                                .shadow(radius: 2)
                        }
                    }

                Text(wallpaper.label)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .lineLimit(1)
            }
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(isSelected ? [.isSelected] : [])
    }

    // MARK: - Photo picker

    private var photoPickerRow: some View {
        PhotosPicker(selection: $selectedItem, matching: .images) {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: "photo.on.rectangle")
                    .font(.system(size: 14))
                    .foregroundStyle(Design.Brand.accent)
                    .frame(width: 20, alignment: .center)

                Text("Choose Photo")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)

                Spacer()

                if isImporting {
                    ProgressView()
                } else if case .custom = settingsStore.settings.chatWallpaper {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(Design.Brand.accent)
                }
            }
            .frame(minHeight: Design.Size.minTapTarget)
            .padding(.horizontal, Design.Spacing.md)
            .background(Design.Colors.surface, in: RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
        }
        .buttonStyle(.plain)
        .disabled(isImporting)
    }

    // MARK: - Image preparation

    /// Downscales and JPEG-compresses picked photos before they're persisted.
    ///
    /// `UserSettings` round-trips through `UserDefaultsAppPersistenceStore` as a
    /// single JSON blob under one `UserDefaults` key (see
    /// `Services/Support/UserDefaultsAppPersistenceStore.swift`), and `Data`
    /// encodes as base64 text in that JSON — roughly +33% over the raw byte
    /// count. A full-resolution photo (often several MB, base64-inflated to
    /// 5-10+ MB) would bloat that blob well past what `UserDefaults` is
    /// designed to hold on every read/write of settings. Presets already stay
    /// tiny since they're drawn procedurally; only `.custom` carries raw bytes,
    /// so this is the only path that needs capping. 1600px matches typical
    /// device screen height, which is plenty for a full-screen background.
    private static let maxDimension: CGFloat = 1600
    private static let maxFileSize = 900 * 1024

    private static func prepareWallpaperData(_ data: Data) -> Data? {
        guard let image = UIImage(data: data) else { return nil }

        var targetImage = image
        let longestSide = max(image.size.width, image.size.height)
        if longestSide > maxDimension {
            let scale = maxDimension / longestSide
            let newSize = CGSize(width: image.size.width * scale, height: image.size.height * scale)
            let renderer = UIGraphicsImageRenderer(size: newSize)
            targetImage = renderer.image { _ in
                image.draw(in: CGRect(origin: .zero, size: newSize))
            }
        }

        var quality: CGFloat = 0.7
        guard var jpegData = targetImage.jpegData(compressionQuality: quality) else { return nil }

        while jpegData.count > maxFileSize && quality > 0.1 {
            quality -= 0.15
            guard let reduced = targetImage.jpegData(compressionQuality: max(quality, 0.1)) else { break }
            jpegData = reduced
        }

        return jpegData
    }
}

#Preview {
    NavigationStack {
        WallpaperPickerSheet()
    }
    .environment(SettingsStore(persistence: UserDefaultsAppPersistenceStore()))
    .environment(ThemeManager.shared)
}
