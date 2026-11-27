// Joining a receiver's Wi-Fi Direct group as an ordinary Wi-Fi network (macOS has no Wi-Fi Direct of its own), and
// going back to the network this Mac was on before.
//
// What macOS asks for: the names of Wi-Fi networks (the current one's included) are Location data since macOS 14,
// so Luch asks for Location when a direct connection is first used; without it the network left is not known by
// name and macOS's own auto-join picks where to go back. Going back needs no password: the network is a known one,
// its password in the keychain, and CoreWLAN or `networksetup` join it as the person's own Wi-Fi menu would.
// SPDX-License-Identifier: GPL-3.0-or-later
import CoreLocation
import CoreWLAN
import Foundation
import LuchProto

/// What a direct connection needs of the Wi-Fi (CoreWiFi; tests use a fake).
public protocol WiFiClient: AnyObject {
    /// Whether network names can be read, asking the person the first time (Location); false when not given.
    func allowNames() -> Bool
    /// The network this Mac is on now, if known.
    var ssid: String? { get }
    /// The Wi-Fi interface ("en0"), if there is one.
    var interfaceName: String? { get }
    func join(ssid: String, password: String) throws
    func leave()
    /// Back onto a network this Mac knows (its password kept by macOS); nil: whichever macOS picks.
    func rejoin(ssid: String?) throws
}

/// Where a direct connection stands, and when to go back: the network left is remembered when the group is joined
/// and rejoined after the session there ends, when nothing is shown at all, or when the person says so.
public final class DirectWiFi {
    public struct Joined {
        public var ssid: String
        /// The network left for it, nil when it was not known by name (or there was none).
        public var previous: String?
        public var receiverID: String
        public var name: String
        var since: TimeInterval
        var sawSession = false
        var lastBusy: TimeInterval = 0
    }

    /// A pause in what is shown (a mirror stopped for a cast) does not take the Mac off at once.
    public static let endGrace: TimeInterval = 15
    /// Nothing shown this long after joining: the receiver drops its group 2 minutes after its last connection.
    public static let idleLimit: TimeInterval = 150

    private let wifi: WiFiClient
    private let now: () -> TimeInterval
    private let lock = NSLock()
    private var current: Joined?

    public init(wifi: WiFiClient, now: @escaping () -> TimeInterval) {
        self.wifi = wifi
        self.now = now
    }

    public var joined: Joined? {
        lock.lock()
        defer { lock.unlock() }
        return current
    }

    /// The question asked before anything changes: this Mac's Wi-Fi is what the person gives up.
    public func question(tv: String) -> String {
        let leaving = joined?.previous ?? wifi.ssid
        return "Connect directly to \(tv)? This Mac leaves "
            + (leaving.map { "'\($0)'" } ?? "its Wi-Fi network") + " while showing, and comes back after."
    }

    /// Joins the group. A second direct connection keeps the network the first one left as the one to go back to.
    public func join(_ a: DirectAnswer, receiverID: String, name: String) throws {
        let before = joined
        let previous = before.map(\.previous) ?? wifi.ssid
        do {
            try wifi.join(ssid: a.ssid, password: a.pass)
        } catch {
            // Half joined or not at all: back to where it was, as if never asked.
            if before == nil { try? wifi.rejoin(ssid: previous) }
            throw FindError("could not join '\(a.ssid)': \(error.localizedDescription)")
        }
        lock.lock()
        current = Joined(ssid: a.ssid, previous: previous, receiverID: receiverID, name: name, since: now())
        lock.unlock()
        log("direct: joined '\(a.ssid)' for \(name), back to \(previous.map { "'\($0)'" } ?? "macOS's choice") after")
    }

    /// Called every few seconds with whether a session with the receiver is open; goes back once it has ended (or
    /// none came) and returns what it said then, else nil.
    public func tick(busy: Bool) -> String? {
        let t = now()
        lock.lock()
        guard var j = current else {
            lock.unlock()
            return nil
        }
        if busy {
            j.sawSession = true
            j.lastBusy = t
        }
        current = j
        lock.unlock()
        if j.sawSession, !busy, t - j.lastBusy >= DirectWiFi.endGrace { return restore("showing ended") }
        if !j.sawSession, t - j.since >= DirectWiFi.idleLimit { return restore("nothing was shown") }
        return nil
    }

    /// `luchctl direct off`: leaves the group now.
    public func off() throws -> String {
        guard joined != nil else { throw FindError("not connected directly") }
        return restore("asked to")
    }

    /// Leaves the group and goes back; says how it went.
    @discardableResult
    func restore(_ why: String) -> String {
        lock.lock()
        let j = current
        current = nil
        lock.unlock()
        guard let j else { return "not connected directly" }
        wifi.leave()
        let words: String
        do {
            try wifi.rejoin(ssid: j.previous)
            words = "left '\(j.ssid)' (\(why)): " + (j.previous.map { "back on '\($0)'" } ?? "macOS picks the Wi-Fi network")
        } catch {
            words = "left '\(j.ssid)' (\(why)), but \(error.localizedDescription)"
        }
        log("direct: \(words)")
        return words
    }
}

/// The Mac's Wi-Fi through CoreWLAN.
public final class CoreWiFi: NSObject, WiFiClient, CLLocationManagerDelegate {
    private var location: CLLocationManager?
    private let decided = DispatchSemaphore(value: 0)

    public override init() {}

    private var iface: CWInterface? { CWWiFiClient.shared().interface() }

    public var ssid: String? { iface?.ssid() }
    public var interfaceName: String? { iface?.interfaceName }

    /// Must not be called on the main thread: the question is put there and waited for.
    public func allowNames() -> Bool {
        var status = CLLocationManager().authorizationStatus
        if status == .notDetermined {
            DispatchQueue.main.sync {
                let m = CLLocationManager()
                m.delegate = self
                location = m
                m.requestWhenInUseAuthorization()
            }
            _ = decided.wait(timeout: .now() + 60)
            status = CLLocationManager().authorizationStatus
            DispatchQueue.main.sync { location = nil }
        }
        return ![.notDetermined, .denied, .restricted].contains(status)
    }

    public func locationManagerDidChangeAuthorization(_ m: CLLocationManager) {
        if m.authorizationStatus != .notDetermined { decided.signal() }
    }

    public func join(ssid: String, password: String) throws {
        guard let i = iface else { throw FindError("this Mac has no Wi-Fi") }
        guard i.powerOn() else { throw FindError("Wi-Fi is off: turn it on to connect directly") }
        // The group has just come up: a scan or two may miss it.
        var found: CWNetwork?
        var lastError: Error?
        for attempt in 0..<4 where found == nil {
            if attempt > 0 { Thread.sleep(forTimeInterval: 1.5) }
            do {
                let nets = try i.scanForNetworks(withSSID: Data(ssid.utf8))
                // Without Location the names read back as nil: a single answer to the scan for this one name is it
                // then; several could be any network around (and an open one would take any passphrase).
                found = nets.first { $0.ssid == ssid } ?? (nets.count == 1 ? nets.first { $0.ssid == nil } : nil)
            } catch {
                lastError = error
            }
        }
        if let found {
            do {
                try i.associate(to: found, password: password)
                return
            } catch {
                lastError = error
            }
        }
        // What the Wi-Fi menu does, through the system's own tool.
        if Self.networksetup(["-setairportnetwork", i.interfaceName ?? "en0", ssid, password]) { return }
        throw FindError(lastError.map { $0.localizedDescription } ?? "'\(ssid)' is not in range of this Mac's Wi-Fi")
    }

    public func leave() {
        iface?.disassociate()
    }

    public func rejoin(ssid: String?) throws {
        guard let i = iface else { throw FindError("this Mac has no Wi-Fi") }
        guard let ssid else { return } // auto-join brings back the best known network
        // Auto-join often goes back by itself within seconds of leaving.
        if waitFor(ssid, on: i, seconds: 8) { return }
        if let net = (try? i.scanForNetworks(withSSID: Data(ssid.utf8)))?.first {
            try? i.associate(to: net, password: nil)
            if waitFor(ssid, on: i, seconds: 5) { return }
        }
        if Self.networksetup(["-setairportnetwork", i.interfaceName ?? "en0", ssid]), waitFor(ssid, on: i, seconds: 5) {
            return
        }
        throw FindError("could not go back to '\(ssid)': choose it in the Wi-Fi menu")
    }

    private func waitFor(_ ssid: String, on i: CWInterface, seconds: TimeInterval) -> Bool {
        let until = Date().addingTimeInterval(seconds)
        while Date() < until {
            if i.ssid() == ssid { return true }
            Thread.sleep(forTimeInterval: 0.5)
        }
        return false
    }

    /// True when it said nothing: networksetup exits 0 even when it fails, and says why.
    static func networksetup(_ args: [String]) -> Bool {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/sbin/networksetup")
        p.arguments = args
        let out = Pipe()
        p.standardOutput = out
        p.standardError = out
        do { try p.run() } catch { return false }
        let said = String(decoding: out.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        p.waitUntilExit()
        if !said.isEmpty { log("direct: networksetup \(args.first ?? ""): \(said)") }
        return p.terminationStatus == 0 && said.isEmpty
    }
}
