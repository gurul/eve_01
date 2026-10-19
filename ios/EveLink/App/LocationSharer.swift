// Shares the phone's location with eve on the Mac (Shared/PhoneLocation.swift has the wire and the rules; the
// Settings sheet has the switch). With the app open: standard updates, ~100 m accuracy, every 50 m. In the
// background: significant-change monitoring only (cell-tower moves of ~500 m, the cheapest service iOS has), which
// also relaunches the app after iOS ended it, when the owner chose "Always". Nothing else runs in the background: no
// standard updates, no timers.
//
// Visits (owner, 2026-10-18: "make a location service that tracks location ... patterns habits where things are"):
// with Always, iOS's visit monitoring also says when the owner arrives at and leaves a place (CLVisit: no GPS of its
// own, it rides the significant-change wakes). Each one is named (the street line, and the nearest point of interest
// within 60 m from Apple Maps, which already names the place) and sent to the Mac's places (POST /api/link/visit). A
// visit that cannot be sent waits in a small queue on this phone (VisitQueue, 50 at most) and goes with the next send.
//
// Threading: the manager is made on the main thread, so its delegate calls arrive there; each one turns the
// CLLocation into a Sendable LocationFix first and then runs on the main actor. Diagnostics never carry a coordinate
// or a place name, only accuracy, age and why a fix was or was not sent.
import CoreLocation
import MapKit
import SwiftUI
import UIKit

@MainActor
final class LocationSharer: NSObject, ObservableObject {
    static let shared = LocationSharer()

    /// The owner's switch. On by default: the app asks iOS once it is paired, and shares once iOS says yes.
    @Published private(set) var enabled: Bool
    @Published private(set) var permission: Permission = .notAsked
    @Published private(set) var lastSent: Date?
    @Published private(set) var problem: String?

    enum Permission: Equatable, Sendable {
        case notAsked, whileUsing, always, denied, restricted

        var title: String {
            switch self {
            case .notAsked: "Not asked yet"
            case .whileUsing: "While using the app"
            case .always: "Always"
            case .denied: "Not allowed"
            case .restricted: "Restricted on this phone"
            }
        }
        var granted: Bool { self == .whileUsing || self == .always }
    }

    /// Approximate location (the owner turned Precise Location off): fixes are a few km wide, places are cities.
    @Published private(set) var approximate = false

    /// What the Mac's places hold (GET /api/link/places), for Settings; nil until read, or when the Mac has none.
    @Published private(set) var places: PlacesSummary?
    @Published private(set) var placesProblem: String?
    @Published private(set) var forgetting = false

    private var manager: CLLocationManager?
    private var endpoint: @MainActor () -> ChatEndpoint? = { nil }
    private var refresh: @MainActor () async -> Bool = { false }
    private var active = false
    private var monitoringSignificant = false
    private var monitoringVisits = false
    private var sendingVisits = false
    private var updating = false
    private var heartbeat: Timer?
    private var sending = false
    private var waiting: LocationFix?         // the newest fix that came while a send was out
    private var lastLookup: Date?
    private let session: URLSession = {
        let config = URLSessionConfiguration.ephemeral
        config.waitsForConnectivity = false
        config.httpCookieStorage = nil
        config.urlCache = nil
        config.timeoutIntervalForRequest = 20
        config.timeoutIntervalForResource = 25
        return URLSession(configuration: config)
    }()

    private enum Key {
        static let enabled = "location.share"
        static let askedAlways = "location.askedAlways"
        static let lastFix = "location.lastFix"       // the fix sent last, for the throttle (on this phone only)
        static let lastSentAt = "location.lastSentAt"
        static let place = "location.place"           // the last place name and where it was named
        static let visits = "location.visitQueue"     // visits not sent yet (VisitQueue), on this phone only
    }

    private override init() {
        enabled = UserDefaults.standard.object(forKey: Key.enabled) as? Bool ?? true
        lastSent = UserDefaults.standard.object(forKey: Key.lastSentAt) as? Date
        super.init()
    }

    // ---- lifecycle ----------------------------------------------------------------------------------------

    /// At launch (the app delegate), before anything else: iOS hands a significant-change fix only to a manager with
    /// its delegate set and the monitoring restarted, when it relaunched the app in the background for one.
    func launch(endpoint: @escaping @MainActor () -> ChatEndpoint?, refresh: @escaping @MainActor () async -> Bool,
                forLocation: Bool) {
        self.endpoint = endpoint
        self.refresh = refresh
        if forLocation { diag("location: relaunched in the background for a move") }
        let m = CLLocationManager()
        m.delegate = self
        m.desiredAccuracy = kCLLocationAccuracyHundredMeters
        m.distanceFilter = 50
        m.activityType = .other
        m.pausesLocationUpdatesAutomatically = true
        manager = m
        readPermission()
        // The app's own active / background notifications as well as the scene's phase: SwiftUI's onChange does
        // not report the phase a scene starts in, and a launch straight to .active must start the updates.
        let center = NotificationCenter.default
        center.addObserver(forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main) { _ in
            MainActor.assumeIsolated { LocationSharer.shared.scene(.active) }
        }
        center.addObserver(forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main) { _ in
            MainActor.assumeIsolated { LocationSharer.shared.scene(.background) }
        }
        if UIApplication.shared.applicationState == .active { active = true }
        apply()
    }

    /// The scene's phase (EveLinkApp): standard updates only while the app is on screen.
    /// .inactive (Control Center, the app switcher, a call banner) changes nothing: stopping and restarting the
    /// updates for it would cost a fresh fix each time.
    func scene(_ phase: ScenePhase) {
        if phase == .inactive { return }
        let now = phase == .active
        guard now != active else { return }
        active = now
        if now { askIfNeeded() }
        apply()
    }

    func setEnabled(_ on: Bool) {
        guard on != enabled else { return }
        enabled = on
        UserDefaults.standard.set(on, forKey: Key.enabled)
        diag("location: sharing \(on ? "on" : "off")")
        if on {
            // what the Mac had may be hours old now: the first fix after turning it back on is sent
            UserDefaults.standard.removeObject(forKey: Key.lastFix)
            askIfNeeded()
        } else {
            problem = nil
        }
        apply()
    }

    func openSettings() {
        if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
    }

    /// Ask iOS, in order: while using the app first, then (once) always. iOS shows the second as "Change to Always
    /// Allow"; the owner can refuse it and still share while the app is open. Only asked with the app on screen and
    /// paired, so the question comes when the owner can see why.
    func askIfNeeded() {
        guard enabled, active, let manager, endpoint() != nil else { return }
        switch manager.authorizationStatus {
        case .notDetermined:
            diag("location: asking for while-using")
            manager.requestWhenInUseAuthorization()
        case .authorizedWhenInUse where !UserDefaults.standard.bool(forKey: Key.askedAlways):
            UserDefaults.standard.set(true, forKey: Key.askedAlways)
            diag("location: asking for always")
            manager.requestAlwaysAuthorization()
        default: break
        }
    }

    private func readPermission() {
        guard let manager else { return }
        let was = permission
        permission = switch manager.authorizationStatus {
        case .authorizedAlways: .always
        case .authorizedWhenInUse: .whileUsing
        case .denied: .denied
        case .restricted: .restricted
        default: .notAsked
        }
        approximate = permission.granted && manager.accuracyAuthorization == .reducedAccuracy
        if was != permission { diag("location: permission \(permission.title.lowercased())\(approximate ? ", approximate" : "")") }
    }

    /// Start or stop each service to match the switch, the permission and the scene. Idempotent.
    private func apply() {
        guard let manager else { return }
        let share = enabled && permission.granted
        // standard updates: the app on screen only
        let wantUpdates = share && active
        if wantUpdates != updating {
            updating = wantUpdates
            if wantUpdates {
                manager.startUpdatingLocation()
                startHeartbeat()
            } else {
                manager.stopUpdatingLocation()
                heartbeat?.invalidate()
                heartbeat = nil
            }
        }
        // significant change: with Always only (while-using gets nothing from it in the background)
        let wantSignificant = share && permission == .always && CLLocationManager.significantLocationChangeMonitoringAvailable()
        if wantSignificant != monitoringSignificant {
            monitoringSignificant = wantSignificant
            if wantSignificant {
                manager.startMonitoringSignificantLocationChanges()
            } else {
                manager.stopMonitoringSignificantLocationChanges()
            }
            // a fresh process starts with it false, so a background relaunch restarts the monitoring here, as
            // iOS requires before it hands over the fix that woke the app
            diag("location: background moves \(wantSignificant ? "on" : "off")")
        }
        // visits: with Always too (they ride the same wakes); a fresh process restarts them here, as for moves
        let wantVisits = wantSignificant
        if wantVisits != monitoringVisits {
            monitoringVisits = wantVisits
            if wantVisits {
                manager.startMonitoringVisits()
            } else {
                manager.stopMonitoringVisits()
            }
            diag("location: visits \(wantVisits ? "on" : "off")")
        }
    }

    /// With the app open and the phone still, standard updates say nothing; every 10 minutes the updates restart,
    /// which hands over a fresh fix, so eve's copy stays current (the throttle decides whether it is sent).
    private func startHeartbeat() {
        heartbeat?.invalidate()
        let t = Timer(timeInterval: LocationThrottle.refreshForeground, repeats: true) { _ in
            MainActor.assumeIsolated {
                let s = LocationSharer.shared
                guard s.updating, let m = s.manager else { return }
                m.stopUpdatingLocation()
                m.startUpdatingLocation()
            }
        }
        t.tolerance = 60     // let iOS batch the wake with others
        RunLoop.main.add(t, forMode: .common)
        heartbeat = t
    }

    // ---- fixes --------------------------------------------------------------------------------------------

    fileprivate func received(_ fix: LocationFix) {
        guard enabled, permission.granted, endpoint() != nil else { return }
        if sending {
            waiting = fix
            return
        }
        let last = Self.loadLastFix()
        let decision = LocationThrottle.decide(fix, last: last, now: Date(), foreground: active)
        guard case .send(let reason) = decision else { return }
        sending = true
        Task { await send(fix, reason: reason) }
    }

    private func send(_ fix: LocationFix, reason: LocationThrottle.Reason) async {
        // A fix that woke the app in the background has seconds of run time; ask for the usual ~30 s to name the
        // place and post it.
        let background = BackgroundRun(name: "eve location")
        defer { background.end() }
        let place = await placeName(for: fix)
        guard enabled else {           // switched off while the place was being named
            sending = false
            waiting = nil
            return
        }
        let body = PhoneLocationBody(fix, place: place)
        let reply = await post(body)
        let age = Int(max(0, Date().timeIntervalSince(fix.at)))
        if reply == .ok {
            Self.saveLastFix(fix)
            let now = Date()
            lastSent = now
            UserDefaults.standard.set(now, forKey: Key.lastSentAt)
            problem = nil
            diag("location: sent (\(reason.rawValue), ±\(Int(fix.accuracyM)) m, \(age) s old\(active ? "" : ", background"))")
        } else {
            problem = reply.problem
            diag("location: not sent (\(reply.problem ?? "?"))")
        }
        sending = false
        if reply == .ok { flushVisits() }     // the Mac is reachable now: what waited goes too
        if let next = waiting {
            waiting = nil
            received(next)
        }
    }

    /// One POST, retried once at eve's new address when the tunnel is gone.
    private func post(_ body: PhoneLocationBody) async -> LocationReply {
        var retried = false
        while true {
            guard let ep = endpoint() else { return .failed("Eve Link isn't paired.") }
            let reply = await Self.fetch(LocationRequests.post(ep, body), session: session)
            if reply.addressMayHaveMoved, !retried, await refresh() {
                retried = true
                continue
            }
            return reply
        }
    }

    @concurrent
    private static func fetch(_ request: URLRequest, session: URLSession) async -> LocationReply {
        do {
            let (data, response) = try await session.data(for: request)
            return LocationReply.from(status: (response as? HTTPURLResponse)?.statusCode ?? 0, body: data)
        } catch {
            let f = ChatFailure.from(error)
            return f.addressMayHaveMoved ? .unreachable : .failed(f.message)
        }
    }

    // ---- visits -------------------------------------------------------------------------------------------

    fileprivate func received(_ visit: VisitEvent) {
        guard enabled, permission.granted, endpoint() != nil else { return }
        Task { await queue(visit) }
    }

    /// Name the visit, put it on the queue (a departure replaces its own arrival), and send what waits.
    private func queue(_ visit: VisitEvent) async {
        let background = BackgroundRun(name: "eve visit")
        defer { background.end() }
        var place: String?
        var poi: (name: String?, category: String?) = (nil, nil)
        if let fix = visit.fix {
            place = await placeName(for: fix)
            poi = await Self.nearestPOI(fix)
        }
        guard enabled else { return }
        let body = PhoneVisitBody(visit, place: place, poi: poi.name, category: PhoneVisitBody.category(poi.category))
        var q = Self.loadVisits()
        q.add(body)
        Self.saveVisits(q)
        diag("location: visit \(visit.departed == nil ? "arrival" : "departure") (±\(Int(visit.accuracyM)) m\(active ? "" : ", background"))")
        await sendVisits()
    }

    private func flushVisits() {
        guard !Self.loadVisits().isEmpty else { return }
        Task { await sendVisits() }
    }

    /// The queue, oldest first, until it is empty or a send must wait for later. One sender at a time.
    private func sendVisits() async {
        guard !sendingVisits else { return }
        sendingVisits = true
        defer { sendingVisits = false }
        let background = BackgroundRun(name: "eve visits")
        defer { background.end() }
        while enabled, let body = Self.loadVisits().first {
            guard let ep = endpoint() else { return }
            var reply = await Self.fetch(LocationRequests.visit(ep, body), session: session)
            if reply.addressMayHaveMoved, await refresh(), let ep2 = endpoint() {
                reply = await Self.fetch(LocationRequests.visit(ep2, body), session: session)
            }
            switch VisitOutcome.from(reply) {
            case .sent, .drop:
                if reply == .outdated { diag("location: this eve has no places yet; visit dropped") }
                else if reply != .ok { diag("location: visit dropped (\(reply.problem ?? "?"))") }
                var q = Self.loadVisits()
                if q.first == body { q.dropFirst() }
                Self.saveVisits(q)
            case .retry:
                diag("location: visit kept for later (\(reply.problem ?? "?"))")
                return
            }
        }
    }

    /// The nearest point of interest within 60 m (Apple Maps, as the place name), 6 s at most; nils when none.
    private static func nearestPOI(_ fix: LocationFix) async -> (name: String?, category: String?) {
        let center = CLLocationCoordinate2D(latitude: fix.lat, longitude: fix.lon)
        let here = CLLocation(latitude: fix.lat, longitude: fix.lon)
        let search = UncheckedBox(MKLocalSearch(request: MKLocalPointsOfInterestRequest(center: center, radius: 60)))
        let lookup = Task { () -> (String?, String?) in
            let response = await withTaskCancellationHandler {
                try? await search.value.start()
            } onCancel: {
                search.value.cancel()
            }
            let nearest = response?.mapItems.min { a, b in
                itemLocation(a).distance(from: here) < itemLocation(b).distance(from: here)
            }
            guard let nearest else { return (nil, nil) }
            return (nearest.name, nearest.pointOfInterestCategory?.rawValue)
        }
        let timer = Task {
            try? await Task.sleep(for: .seconds(6))
            lookup.cancel()
        }
        defer { timer.cancel() }
        let found = await lookup.value
        return (found.0, found.1)
    }

    private static func itemLocation(_ item: MKMapItem) -> CLLocation {
        if #available(iOS 26.0, *) { return item.location }
        return legacyLocation(item)
    }

    @available(iOS, deprecated: 26.0, message: "the fallback for iOS 18 to 25; MKMapItem.location on 26 and later")
    private static func legacyLocation(_ item: MKMapItem) -> CLLocation {
        item.placemark.location ?? CLLocation(latitude: item.placemark.coordinate.latitude,
                                              longitude: item.placemark.coordinate.longitude)
    }

    // ---- the Mac's places (Settings) ------------------------------------------------------------------------

    /// GET /api/link/places: how many places the Mac knows, for the Settings row.
    func refreshPlaces() async {
        guard let ep = endpoint() else { return }
        let (status, data) = await Self.raw(LocationRequests.places(ep), session: session)
        places = PlacesSummary.from(status: status, body: data)
        placesProblem = switch status {
        case 200: nil
        case 403: "Places are off on the Mac."
        case 404: "This eve can't keep places yet: update it on the Mac."
        default: nil
        }
    }

    /// POST /api/link/places/forget: the Mac forgets the history, the places, the things and the latest fix; the
    /// visits waiting on this phone go too. Settings asks first.
    func forgetPlaces() async {
        guard let ep = endpoint(), !forgetting else { return }
        forgetting = true
        defer { forgetting = false }
        Self.saveVisits(VisitQueue())
        let (status, _) = await Self.raw(LocationRequests.forgetPlaces(ep), session: session)
        if status == 200 {
            diag("location: the Mac forgot the location history")
            placesProblem = nil
            await refreshPlaces()
        } else {
            placesProblem = status == 404 ? "This eve keeps no history to forget." : "The Mac didn't forget it (\(status)). Try again."
        }
    }

    @concurrent
    private static func raw(_ request: URLRequest, session: URLSession) async -> (Int, Data) {
        guard let (data, response) = try? await session.data(for: request) else { return (0, Data()) }
        return ((response as? HTTPURLResponse)?.statusCode ?? 0, data)
    }

    // ---- the place name -----------------------------------------------------------------------------------

    /// The cached name within 200 m, else a reverse geocode (at most one a minute, 6 s at most), else none.
    private func placeName(for fix: LocationFix) async -> String? {
        let cached = Self.loadPlace()
        if let reused = PlaceRule.reuse(cached, for: fix) { return reused }
        let now = Date()
        guard PlaceRule.mayLookUp(last: lastLookup, now: now) else { return nil }
        lastLookup = now
        let named = await Self.lookUp(fix)
        guard let line = PlaceRule.line(name: named?.name, city: named?.city, accuracyM: fix.accuracyM) else { return nil }
        Self.savePlace(PlaceRule.Cached(lat: fix.lat, lon: fix.lon, place: line, at: fix.at, accuracyM: fix.accuracyM))
        return line
    }

    private struct Named: Sendable { var name: String?; var city: String? }

    private static func lookUp(_ fix: LocationFix) async -> Named? {
        let location = CLLocation(latitude: fix.lat, longitude: fix.lon)
        let lookup = Task { () -> Named? in
            if #available(iOS 26.0, *) { return await mapKitLookUp(location) }
            return await geocoderLookUp(location)
        }
        let timer = Task {
            try? await Task.sleep(for: .seconds(6))
            lookup.cancel()
        }
        defer { timer.cancel() }
        return await lookup.value
    }

    @available(iOS 26.0, *)
    private static func mapKitLookUp(_ location: CLLocation) async -> Named? {
        guard let request = MKReverseGeocodingRequest(location: location) else { return nil }
        let box = UncheckedBox(request)
        let items = await withTaskCancellationHandler {
            try? await box.value.mapItems
        } onCancel: {
            box.value.cancel()
        }
        guard let item = items?.first else { return nil }
        return Named(name: item.name, city: item.addressRepresentations?.cityWithContext)
    }

    @available(iOS, deprecated: 26.0, message: "the fallback for iOS 18 to 25; mapKitLookUp on 26 and later")
    private static func geocoderLookUp(_ location: CLLocation) async -> Named? {
        let geocoder = UncheckedBox(CLGeocoder())
        let marks = await withTaskCancellationHandler {
            try? await geocoder.value.reverseGeocodeLocation(location)
        } onCancel: {
            geocoder.value.cancelGeocode()
        }
        guard let mark = marks?.first else { return nil }
        let city = [mark.locality, mark.administrativeArea].compactMap { $0 }.joined(separator: ", ")
        return Named(name: mark.name, city: city.isEmpty ? nil : city)
    }

    // ---- what the phone keeps (UserDefaults, this phone only) ---------------------------------------------

    private static func loadLastFix() -> LocationFix? {
        guard let d = UserDefaults.standard.dictionary(forKey: Key.lastFix),
              let lat = d["lat"] as? Double, let lon = d["lon"] as? Double, let acc = d["acc"] as? Double,
              let at = d["at"] as? Date else { return nil }
        return LocationFix(lat: lat, lon: lon, accuracyM: acc, at: at)
    }

    private static func saveLastFix(_ f: LocationFix) {
        UserDefaults.standard.set(["lat": f.lat, "lon": f.lon, "acc": f.accuracyM, "at": f.at], forKey: Key.lastFix)
    }

    private static func loadPlace() -> PlaceRule.Cached? {
        UserDefaults.standard.data(forKey: Key.place).flatMap { try? JSONDecoder().decode(PlaceRule.Cached.self, from: $0) }
    }

    private static func savePlace(_ c: PlaceRule.Cached) {
        if let data = try? JSONEncoder().encode(c) { UserDefaults.standard.set(data, forKey: Key.place) }
    }

    private static func loadVisits() -> VisitQueue {
        UserDefaults.standard.data(forKey: Key.visits).flatMap { try? JSONDecoder().decode(VisitQueue.self, from: $0) } ?? VisitQueue()
    }

    private static func saveVisits(_ q: VisitQueue) {
        if q.isEmpty {
            UserDefaults.standard.removeObject(forKey: Key.visits)
        } else if let data = try? JSONEncoder().encode(q) {
            UserDefaults.standard.set(data, forKey: Key.visits)
        }
    }
}

/// beginBackgroundTask's run time for one send, ended once: by the send, or by iOS's expiry, whichever comes first.
@MainActor
private final class BackgroundRun {
    private var id = UIBackgroundTaskIdentifier.invalid

    init(name: String) {
        id = UIApplication.shared.beginBackgroundTask(withName: name) { [weak self] in
            MainActor.assumeIsolated { self?.end() }
        }
    }

    func end() {
        guard id != .invalid else { return }
        UIApplication.shared.endBackgroundTask(id)
        id = .invalid
    }
}

/// A non-Sendable Core Location / MapKit object handed to a cancellation handler: used from one place at a time.
private final class UncheckedBox<T>: @unchecked Sendable {
    let value: T
    init(_ value: T) { self.value = value }
}

extension LocationSharer: CLLocationManagerDelegate {
    nonisolated func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        // the newest valid fix of the batch, as a Sendable value, before leaving Core Location's call
        let fix = locations.reversed().lazy.compactMap { l in
            LocationFix(lat: l.coordinate.latitude, lon: l.coordinate.longitude, accuracyM: l.horizontalAccuracy,
                        at: l.timestamp, speedMps: l.speed)
        }.first
        guard let fix else { return }
        MainActor.assumeIsolated { self.received(fix) }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didVisit visit: CLVisit) {
        // a Sendable value before leaving Core Location's call; iOS's sentinel dates become nil in VisitEvent
        let event = VisitEvent(lat: visit.coordinate.latitude, lon: visit.coordinate.longitude,
                               accuracyM: visit.horizontalAccuracy, arrival: visit.arrivalDate,
                               departure: visit.departureDate)
        guard let event else { return }
        MainActor.assumeIsolated { self.received(event) }
    }

    nonisolated func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        MainActor.assumeIsolated {
            self.readPermission()
            if self.permission == .whileUsing { self.askIfNeeded() }     // the second question, once
            if !self.permission.granted, self.permission != .notAsked { self.problem = nil }
            self.apply()
        }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didFailWithError error: any Error) {
        let code = (error as? CLError)?.code
        MainActor.assumeIsolated {
            switch code {
            case .locationUnknown?: break              // transient: Core Location keeps trying
            case .denied?:
                self.readPermission()
                self.apply()
            default: diag("location: Core Location error \(code.map { String($0.rawValue) } ?? "?")")
            }
        }
    }
}
