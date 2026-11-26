import SwiftUI
import UIKit
import UserNotifications

/// Which tab is selected, what More has pushed, and whether the instance and account sheet is
/// open.
@MainActor
@Observable
final class ShellState {
    enum Sheet: String, Identifiable {
        case switcher
        var id: String { rawValue }
    }

    /// The selection value of the More tab (area ids never contain a colon).
    static let moreTab = "more:tab"

    /// The selected tab: a pinned area's id, or `moreTab`.
    var selection = "inbox"
    var sheet: Sheet?
    /// What the More tab has pushed (`MoreRoute`s, then whatever those screens push).
    var morePath = NavigationPath()
}

/// The signed-in app: the selected instance's `console_manifest` areas split by that instance's
/// `TabLayout` into the bar (up to three) and More, under the app's own bar (`ShellBar`), whose
/// leading switcher opens the instance and account sheet. The system tab bar is hidden: it has
/// no room for the switcher and cannot be customized on iPhone.
struct AppShell: View {
    let session: Session
    @Environment(\.scenePhase) private var scenePhase
    @State private var store = ManifestStore()
    @State private var toasts = ToastCenter()
    @State private var shell = ShellState()
    @State private var layouts = TabLayoutStore()
    /// The last notification route followed (`NotificationRoute.serial`).
    @State private var routeSeen = 0

    private var api: ConsoleAPI { session.api }

    /// The selected instance's tab layout, matched to its manifest.
    private var layout: TabLayout? {
        store.manifest.map { layouts.layout(for: $0.instance.id, areas: $0.areas) }
    }

    var body: some View {
        content
            .toasts(toasts)
            .sheet(item: $shell.sheet) { _ in
                ShellSheet(
                    onSelect: { id in Task { await switchTo(id) } },
                    onCustomize: { openMore(.editTabs) }
                )
            }
            .environment(session)
            .environment(api)
            .environment(store)
            .environment(toasts)
            .environment(shell)
            .environment(layouts)
            .environment(\.openArea) { id in openArea(id) }
            .task { await store.load(api) }
            // Asks to allow notifications and registers the device, at launch and after sign-in.
            .task { await Push.shared.start(session.client) }
            // The badge and the tabs follow the server when the app comes back.
            .onChange(of: scenePhase) { _, phase in
                if phase == .active { Task { await store.refresh(api) } }
            }
            .onChange(of: store.areas) { _, _ in keepSelectionValid() }
            .onChange(of: layout) { _, _ in keepSelectionValid() }
            // A tapped notification opens the Inbox, also when it launched the app (A6).
            .onChange(of: NotificationRoute.shared.serial) { _, _ in
                Task { await followNotification() }
            }
            .onChange(of: store.phase) { _, _ in Task { await followNotification() } }
    }

    /// Opens the Inbox for a tapped notification, on the home instance, whose Inbox lists every
    /// request. The Inbox takes `NotificationRoute.shared.pendingApprovalID` from there.
    private func followNotification() async {
        let route = NotificationRoute.shared
        guard route.serial != routeSeen, store.manifest != nil else { return }
        routeSeen = route.serial
        shell.sheet = nil
        if store.selected?.isHome != true, let home = store.instances.first(where: { $0.isHome }) {
            await store.select(home.id, api)
        }
        openArea("inbox")
    }

    @ViewBuilder
    private var content: some View {
        if let manifest = store.manifest, let layout {
            let pinned = layout.pinned.compactMap(manifest.area)
            TabView(selection: $shell.selection) {
                ForEach(pinned) { area in
                    Tab(area.title, systemImage: AreaSymbol.name(for: area.id), value: area.id) {
                        NavigationStack {
                            AreaRoot(area: area, instance: manifest.instance)
                        }
                        // A new instance starts every tab afresh.
                        .id("\(manifest.instance.id)/\(area.id)")
                        .modifier(ShellTabChrome())
                    }
                }
                Tab("More", systemImage: "ellipsis", value: ShellState.moreTab) {
                    MoreScreen(manifest: manifest, layout: layout)
                        .id(manifest.instance.id)
                        .modifier(ShellTabChrome())
                }
            }
            .overlay(alignment: .bottom) { ShellBar(pinned: pinned) }
        } else {
            NavigationStack {
                Group {
                    switch store.phase {
                    case .failed(let message):
                        ErrorState(title: "Couldn't load your instances", message: message) {
                            await store.load(api)
                        }
                    default:
                        ProgressView("Loading your instances…")
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Theme.sunk)
                .toolbar {
                    ToolbarItem(placement: .topBarLeading) { SwitcherButton(compact: true) }
                }
            }
        }
    }

    /// Opens area `id` of the selected instance: its tab when it is in the bar, else pushed on
    /// More. Does nothing when the instance has no such area.
    private func openArea(_ id: String) {
        guard let manifest = store.manifest, manifest.area(id) != nil, let layout else { return }
        if layout.pinned.contains(id) {
            shell.selection = id
        } else {
            openMore(.area(id))
        }
    }

    /// Selects More with `route` pushed on it.
    private func openMore(_ route: MoreRoute) {
        shell.selection = ShellState.moreTab
        shell.morePath = NavigationPath([route])
    }

    /// Keeps the selection on a tab that exists: an area that left the bar opens in More; one
    /// the instance no longer has gives way to the first tab.
    private func keepSelectionValid() {
        guard let manifest = store.manifest, let layout else { return }
        let id = shell.selection
        guard id != ShellState.moreTab, !layout.pinned.contains(id) else { return }
        if manifest.area(id) != nil {
            openArea(id)
        } else {
            shell.selection = layout.pinned.first ?? ShellState.moreTab
        }
    }

    /// Selects instance `id`, keeping the open area when it has one (in the bar or in More).
    /// When it has no such area, Overview opens with a toast (M5).
    private func switchTo(_ id: String) async {
        let current = shell.selection
        let currentTitle = store.manifest?.area(current)?.title ?? current
        guard let m = await store.select(id, api) else {
            toasts.show("Couldn't open that instance. Try again.", tone: .bad)
            return
        }
        shell.morePath = NavigationPath()
        if current == ShellState.moreTab {
            shell.selection = ShellState.moreTab
            return
        }
        if m.area(current) != nil {
            openArea(current)
            return
        }
        let fallback = m.area("overview") ?? m.areas.first
        if let fallback { openArea(fallback.id) }
        toasts.show("\(m.instance.title) has no \(currentTitle) · showing \(fallback?.title ?? "its first area")")
    }
}

/// Hides the system tab bar in a tab and keeps the tab's scroll content clear of `ShellBar`.
private struct ShellTabChrome: ViewModifier {
    func body(content: Content) -> some View {
        content
            .toolbar(.hidden, for: .tabBar)
            .contentMargins(.bottom, ShellBar.reserved, for: .scrollContent)
    }
}

// MARK: - Marks

/// An instance's letter on an accent-soft circle.
struct InstanceInitial: View {
    let text: String
    var size: CGFloat = 32

    var body: some View {
        Text(text)
            .font(.system(size: size * 0.46, weight: .semibold))
            .foregroundStyle(Theme.accent)
            .frame(width: size, height: size)
            .background(Theme.accentSoft, in: Circle())
    }
}

// MARK: - Instance and account

/// The one sheet the switcher opens: who is signed in, every instance (how many requests wait
/// on each, the current one ticked), settings (notifications, the tab bar, theme, passkeys, the
/// app's version) and sign out.
struct ShellSheet: View {
    let onSelect: (String) -> Void
    let onCustomize: () -> Void
    @Environment(Session.self) private var session
    @Environment(ConsoleAPI.self) private var api
    @Environment(ManifestStore.self) private var store
    @Environment(\.openArea) private var openArea
    @Environment(\.dismiss) private var dismiss
    @AppStorage("theme") private var theme = AppearanceChoice.system.rawValue
    @AppStorage("profileInitials") private var storedInitials = ""
    @State private var name: String?
    @State private var passkeys: Int?
    @State private var notifications: UNAuthorizationStatus?
    @State private var confirmSignOut = false

    private var version: String {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        return "\(short) (\(build))"
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    HStack(spacing: 12) {
                        Text(storedInitials.isEmpty ? "?" : storedInitials)
                            .font(.headline)
                            .foregroundStyle(Theme.accent)
                            .frame(width: 44, height: 44)
                            .background(Theme.accentSoft, in: Circle())
                        VStack(alignment: .leading, spacing: 2) {
                            Text(name ?? "Signed in").font(.headline).foregroundStyle(Theme.ink)
                            Text("Signed in with a passkey on this iPhone.")
                                .font(Theme.Fonts.meta).foregroundStyle(Theme.muted)
                        }
                    }
                }
                if !store.instances.isEmpty {
                    Section("Instances") {
                        ForEach(store.instances) { instance in
                            Button {
                                dismiss()
                                if instance.id != store.selected?.id { onSelect(instance.id) }
                            } label: {
                                row(instance)
                            }
                            .buttonStyle(.plain)
                        }
                        if store.manifest?.area("instances") != nil {
                            Button {
                                dismiss()
                                openArea("instances")
                            } label: {
                                ListRow(title: "All instances") { chevron }
                                    .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
                Section("Settings") {
                    Button {
                        if let url = URL(string: UIApplication.openNotificationSettingsURLString) {
                            UIApplication.shared.open(url)
                        }
                    } label: {
                        ListRow(title: "Notifications", subtitle: "Approvals and landings") {
                            notificationPill
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    if let instance = store.manifest?.instance {
                        Button {
                            dismiss()
                            onCustomize()
                        } label: {
                            ListRow(title: "Tab bar", subtitle: "Pin and order tabs for \(instance.title)") {
                                chevron
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                    }
                    Picker("Theme", selection: $theme) {
                        ForEach(AppearanceChoice.allCases) { Text($0.title).tag($0.rawValue) }
                    }
                    .font(Theme.Fonts.body)
                    Button {
                        dismiss()
                        openArea("access")
                    } label: {
                        ListRow(title: "Passkeys") {
                            Text(passkeys.map { String($0) } ?? "–")
                                .font(Theme.Fonts.body).foregroundStyle(Theme.muted)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    KeyValueRow("App version", version, mono: true)
                }
                Section {
                    Button("Sign out", role: .destructive) { confirmSignOut = true }
                        .frame(maxWidth: .infinity)
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle("Instance & account")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .confirmationDialog(
                "Sign out of pigeon?", isPresented: $confirmSignOut, titleVisibility: .visible
            ) {
                Button("Sign out", role: .destructive) {
                    Task { await session.userSignOut() }
                }
                Button("Cancel", role: .cancel) {}
            }
            .task { await load() }
            .task { await store.loadOthers(api) }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }

    private var chevron: some View {
        Image(systemName: "chevron.right")
            .font(.footnote.weight(.semibold))
            .foregroundStyle(Theme.lineStrong)
    }

    private func row(_ instance: InstanceSummary) -> some View {
        HStack(spacing: 10) {
            InstanceInitial(text: instance.initial, size: 30)
            VStack(alignment: .leading, spacing: 1) {
                Text(instance.title).font(Theme.Fonts.body).foregroundStyle(Theme.ink)
                if let host = instance.host {
                    Text(host).idStyle().foregroundStyle(Theme.muted)
                }
            }
            Spacer(minLength: 8)
            if let waiting = store.manifests[instance.id]?.inbox?.count, waiting > 0 {
                StatusPill("\(waiting) waiting", tone: .warn)
            }
            if instance.id == store.selected?.id {
                Image(systemName: "checkmark")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Theme.accent)
                    .accessibilityLabel("Current")
            }
        }
        .frame(minHeight: 40)
        .contentShape(Rectangle())
    }

    @ViewBuilder
    private var notificationPill: some View {
        switch notifications {
        case .authorized, .provisional, .ephemeral: StatusPill("On", tone: .ok)
        case .denied: StatusPill("Off", tone: .warn)
        case .notDetermined: StatusPill("Not asked", tone: .neutral)
        default: EmptyView()
        }
    }

    /// Reads who is signed in (`users_whoami`), their passkeys (`app_people`) and the
    /// notification setting. Each part that fails stays blank.
    private func load() async {
        notifications = await UNUserNotificationCenter.current().notificationSettings()
            .authorizationStatus
        if let who = try? await api.op("users_whoami") {
            let person = who["acting_for"]["name"].string ?? who["user"]["name"].string
            name = person
            if let person {
                storedInitials = who["initials"].string ?? initialsOf(person)
            }
        }
        if let people = try? await api.op("app_people"), let me = people["me"].string {
            let mine = people["people"].items.first(where: { $0["id"].string == me })
            passkeys = mine?["passkeys"].array?.count
        }
    }
}
