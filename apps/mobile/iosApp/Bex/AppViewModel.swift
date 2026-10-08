import AgentCore
import Combine
import Foundation
import UIKit

@MainActor
final class BexAppViewModel: ObservableObject {
    struct ClientPreferencesSyncError: Error {}

    struct PendingClientPreferencesHandoff {
        let owner: AgentStore
        let data: Data
        let token: UInt64
        let minimumRevision: UInt64
    }

    struct PushActivitySource: Encodable {
        let environmentId: String
        let threadId: String
        let projectTitle: String
        let threadTitle: String
        let modelTitle: String
        let phase: String
        let headline: String
        let updatedAtMs: Int64
        let deepLink: String
    }

    struct PendingLoadBalancedNewThread {
        let projectId: String
        let sourceEnvironmentId: String
        let startedAt: Date
        let generation: UInt64
    }

    var clientPreferencesData: Data
    @Published var snapshot: AgentCore.Snapshot
    @Published var screen: AppScreen = .profiles
    @Published var isScanning = false
    @Published var isConnecting = false
    @Published var pairingError: String?
    @Published var pairingInvitation: Invitation?
    @Published var notice: String?
    @Published var notificationThreadRoute: String?
    @Published var profiles: [HostProfile] = []
    @Published var environments: [EnvironmentRow] = []
    /// Latest immutable core snapshot for each saved environment.
    @Published var environmentSnapshots: [String: AgentCore.Snapshot] = [:]
    @Published var selectedProfileId: String?
    @Published var composerText = ""
    /// Counts requests to focus the composer with the cursor at the end of the draft.
    @Published var composerFocusRequests = 0
    var draftEdits = DraftRevision()
    var composerKey = ""
    @Published var timelineRows: [TimelineRow] = []
    @Published var threadView: ThreadView?
    /// Set by a WidgetKit URL and consumed by the native Usage navigation.
    @Published var usageDeepLinkRequests = 0
    @Published var disclosure = TimelineDisclosure.empty {
        didSet {
            if disclosure != oldValue {
                schedulePresentation()
            }
        }
    }

    @Published var showScrollToEnd = false {
        didSet {
            if showScrollToEnd != oldValue {
                schedulePresentation()
            }
        }
    }

    var presentation: Task<Void, Never>?
    var presentationTick: Task<Void, Never>?

    var store: AgentStore?
    var backgroundOwners: [String: AgentStore] = [:]
    var backgroundTasks: [String: Task<Void, Never>] = [:]
    var backgroundTaskGenerations: [String: UInt64] = [:]
    var initialization: Task<Void, Never>?
    var observation: Task<Void, Never>?
    var persistence: Task<Void, Never>?
    var persistenceWrite: Task<Void, Never>?
    var clientPreferencesGeneration: UInt64 = 0
    var pendingSelectedClientPreferences: PendingClientPreferencesHandoff?
    var nextClientPreferencesHandoffToken: UInt64 = 0
    var selectedClientPreferencesRetry: Task<Void, Never>?
    var connection: Task<Void, Never>?
    var pending: [(Intent, (Result<Outcome, Error>) -> Void)] = []
    var operations: [UUID: Task<Void, Never>] = [:]
    var pushRegistrations: [String: AgentPushRegistration] = [:]
    var pushDeviceIdProvider: ((String) -> String?)?
    var pendingPushActive: [String: (deviceId: String, active: Bool)] = [:]
    var pushGenerations: [String: UInt64] = [:]
    /// The Host store that has accepted the current registration. A retained
    /// registration must be replayed when a profile switches stores, even if
    /// its APNs token did not change.
    var registeredPushOwners: [String: AgentStore] = [:]
    var registeredPushConfigurations: [String: PushDeviceRegistration] = [:]
    var pendingPushThread: (hostId: String, threadId: String)?
    var activityUpdater: (([String: AgentActivityAttributes.ContentState]) -> Void)?
    var incomingShareHandoffsInFlight: Set<URL> = []
    var pendingLoadBalancedNewThread: PendingLoadBalancedNewThread?
    var loadBalancingAttemptGeneration: UInt64 = 0
    var browserProfileRemovalGeneration: UInt64 = 0
    var automaticRouteProfileId: String?
    let usageWidget = UsageWidgetPublisher()

    init() {
        let defaults = SnapshotFiles.modelDefaults()
        clientPreferencesData = defaults
        snapshot = AgentCore.Snapshot.empty(clientPreferences: defaults)
        do { profiles = try HostProfile.load() } catch { notice = error.localizedDescription }
        let recoveryNotice = snapshot.error()
        if !clientPreferencesData.isEmpty,
           let canonical = try? snapshot.serializeModelPreferences(),
           canonical != clientPreferencesData {
            clientPreferencesData = canonical
            Task { [weak self] in
                do {
                    try await SnapshotFiles.saveModelPreferences(canonical)
                } catch {
                    self?.notice = error.localizedDescription
                }
            }
        }
        if let id = UserDefaults.standard.string(forKey: "bex.selected-host"),
           profiles.contains(where: { $0.id == id }) {
            selectProfile(id)
        } else {
            startBackgroundProfiles(nil)
        }
        if let recoveryNotice {
            notice = recoveryNotice
        }
    }
}
