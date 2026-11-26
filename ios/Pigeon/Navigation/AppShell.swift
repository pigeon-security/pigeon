import SwiftUI
import UIKit
import UserNotifications

/// Which tab is selected and which of the shell's sheets is open.
@MainActor
@Observable
final class ShellState {
    enum Sheet: String, Identifiable {
        case switcher, account
        var id: String { rawValue }
    }

    /// The selected area's id.
    var selection = "inbox"
    var sheet: Sheet?
}

/// The signed-in app: one tab per area of the selected instance's `console_manifest`, in
/// manifest order (the system More tab takes the overflow), the Inbox badge from `inbox.count`,
/// and the shell's two sheets (instance switcher, account).
struct AppShell: View {
    let session: Session
    @Environment(\.scenePhase) private var scenePhase
    @State private var store = ManifestStore()
    @State private var toasts = ToastCenter()
    @State private var shell = ShellState()
    /// The last notification route followed (`NotificationRoute.serial`).
    @State private var routeSeen = 0

    private var api: ConsoleAPI { session.api }

    var body: some View {
        content
            .toasts(toasts)
            .sheet(item: $shell.sheet) { sheet in
                switch sheet {
                case .switcher:
                    InstanceSwitcherSheet { id in Task { await switchTo(id) } }
                case .account:
                    AccountSheet()
                }
            }
            .environment(session)
            .environment(api)
            .environment(store)
            .environment(toasts)
            .environment(shell)
            .environment(\.openArea) { id in
                if store.manifest?.area(id) != nil { shell.selection = id }
            }
            .task { await store.load(api) }
            // Asks to allow notifications and registers the device, at launch and after sign-in.
            .task { await Push.shared.start(session.client) }
            // The badge and the tabs follow the server when the app comes back.
            .onChange(of: scenePhase) { _, phase in
                if phase == .active { Task { await store.refresh(api) } }
            }
            .onChange(of: store.areas) { _, areas in keepSelectionValid(areas) }
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
        if store.manifest?.area("inbox") != nil { shell.selection = "inbox" }
    }

    @ViewBuilder
    private var content: some View {
        if let manifest = store.manifest {
            TabView(selection: $shell.selection) {
                ForEach(manifest.areas) { area in
                    Tab(area.title, systemImage: AreaSymbol.name(for: area.id), value: area.id) {
                        NavigationStack {
                            AreaRoot(area: area, instance: manifest.instance)
                        }
                        // A new instance starts every tab afresh.
                        .id("\(manifest.instance.id)/\(area.id)")
                    }
                    .badge(area.id == "inbox" ? store.inboxCount : 0)
                }
            }
            .modifier(MinimizeTabBar())
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
                    ToolbarItem(placement: .topBarTrailing) { AvatarButton() }
                }
            }
        }
    }

    /// Keeps the selected tab among the areas; Inbox (or the first area) otherwise.
    private func keepSelectionValid(_ areas: [Area]) {
        guard !areas.isEmpty, !areas.contains(where: { $0.id == shell.selection }) else { return }
        shell.selection = areas.first { $0.id == "inbox" }?.id ?? areas[0].id
    }

    /// Selects instance `id`. When it has no area for the open tab, Overview opens with a toast (M5).
    private func switchTo(_ id: String) async {
        let current = shell.selection
        let currentTitle = store.manifest?.area(current)?.title ?? current
        guard let m = await store.select(id, api) else {
            toasts.show("Couldn't open that instance. Try again.", tone: .bad)
            return
        }
        guard m.area(current) == nil else { return }
        let fallback = m.area("overview") ?? m.areas.first
        if let fallback { shell.selection = fallback.id }
        toasts.show("\(m.instance.title) has no \(currentTitle) · showing \(fallback?.title ?? "its first area")")
    }
}

/// Minimises the tab bar on scroll down, on iOS 26 and later.
private struct MinimizeTabBar: ViewModifier {
    func body(content: Content) -> some View {
        if #available(iOS 26.0, *) {
            content.tabBarMinimizeBehavior(.onScrollDown)
        } else {
            content
        }
    }
}

// MARK: - Bar items

/// The instance pill at the top left: the only instance picker. Opens the switcher sheet.
struct InstancePill: View {
    @Environment(ManifestStore.self) private var store
    @Environment(ShellState.self) private var shell

    var body: some View {
        Button {
            shell.sheet = .switcher
        } label: {
            HStack(spacing: 8) {
                InstanceInitial(text: store.selected?.initial ?? "?", size: 26)
                Text(store.manifest?.instance.title ?? store.selected?.title ?? "Instance")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)
                Image(systemName: "chevron.up.chevron.down")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(Theme.muted)
            }
            .padding(.leading, 2)
            .padding(.trailing, 6)
            .modifier(PillChrome())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Instance: \(store.selected?.title ?? "none"). Switch instance")
    }
}

/// On iOS 26 the bar draws glass around toolbar items itself; before, the pill carries the
/// material. Never two layers of glass.
private struct PillChrome: ViewModifier {
    func body(content: Content) -> some View {
        if #available(iOS 26.0, *) {
            content
        } else {
            content
                .padding(.vertical, 4)
                .padding(.horizontal, 4)
                .glass(in: Capsule(), interactive: true)
        }
    }
}

/// The avatar at the top right. Opens the account sheet.
struct AvatarButton: View {
    @Environment(ShellState.self) private var shell
    @AppStorage("profileInitials") private var initials = ""

    var body: some View {
        Button {
            shell.sheet = .account
        } label: {
            Text(initials.isEmpty ? "?" : initials)
                .font(.footnote.weight(.semibold))
                .foregroundStyle(Theme.ink)
                .frame(width: 32, height: 32)
                .background(Theme.hover, in: Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Account")
    }
}

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

// MARK: - Instance switcher

/// The instance switcher: every instance, how many requests wait on each, and the current one
/// ticked. The tabs follow the chosen instance's manifest.
struct InstanceSwitcherSheet: View {
    let onSelect: (String) -> Void
    @Environment(ManifestStore.self) private var store
    @Environment(ConsoleAPI.self) private var api
    @Environment(\.openArea) private var openArea
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(store.instances) { instance in
                        Button {
                            dismiss()
                            if instance.id != store.selected?.id { onSelect(instance.id) }
                        } label: {
                            row(instance)
                        }
                        .buttonStyle(.plain)
                    }
                }
                if store.manifest?.area("instances") != nil {
                    Section {
                        Button {
                            dismiss()
                            openArea("instances")
                        } label: {
                            ListRow(title: "All instances") {
                                Image(systemName: "chevron.right")
                                    .font(.footnote.weight(.semibold))
                                    .foregroundStyle(Theme.lineStrong)
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle("Switch instance")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .task { await store.loadOthers(api) }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
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
}

// MARK: - Account

/// The account sheet: who is signed in, notifications, theme, passkeys, the app's version, and
/// sign out.
struct AccountSheet: View {
    @Environment(Session.self) private var session
    @Environment(ConsoleAPI.self) private var api
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
                Section {
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
            .navigationTitle("Account")
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
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
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
