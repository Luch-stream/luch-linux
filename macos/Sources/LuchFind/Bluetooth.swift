// Bluetooth LE through CoreBluetooth: receivers' cards (PROTOCOL.md 1c) and a remote's power key (for waking a
// receiver, luchd's wake.rs), both heard as manufacturer data. CoreBluetooth hands that data out with its company id
// in front, little-endian, as the advertisement carries it.
//
// What macOS does not let an app do: advertise manufacturer data. CBPeripheralManager puts only a local name and
// service UUIDs on the air, so this Mac neither says "looking" (1c) nor sends the remote's signal itself; the
// IOBluetooth HCI calls that once could (BluetoothHCILESetAdvertisingData...) answer 0 and do nothing since the
// stack moved into bluetoothd. Waking by Bluetooth goes through another Luch device instead (WakeRelay.swift).
// SPDX-License-Identifier: GPL-3.0-or-later
import CoreBluetooth
import Foundation

public enum BLE {
    /// The card's company id: the one the Bluetooth SIG keeps for tests; the card's "Lu" magic tells it apart.
    public static let cardCompany: UInt16 = 0xffff
    /// MediaTek's: the key of the remote's manufacturer data.
    public static let mediatek: UInt16 = 0x0046

    /// The company id and the bytes after it, from CoreBluetooth's manufacturer data.
    public static func manufacturer(_ data: Data?) -> (company: UInt16, bytes: [UInt8])? {
        guard let data, data.count >= 2 else { return nil }
        let b = [UInt8](data)
        return (UInt16(b[0]) | UInt16(b[1]) << 8, Array(b[2...]))
    }

    /// A receiver's card in an advertisement, if it holds one.
    public static func card(_ data: Data?) -> Card? {
        guard let m = manufacturer(data), m.company == cardCompany, let c = Card.decode(m.bytes, bluetooth: true),
              c.kind == Card.receiver else { return nil }
        return c
    }

    /// A remote's power-key advertisement (its MediaTek data, at least 8 bytes), if this is one.
    public static func remote(_ data: Data?) -> [UInt8]? {
        guard let m = manufacturer(data), m.company == mediatek, m.bytes.count >= 8 else { return nil }
        return m.bytes
    }
}

/// A receiver heard over the air.
public struct Heard {
    public var card: Card
    /// dBm; higher is nearer.
    public var rssi: Int?
    public var device: UUID
}

/// Receivers by signal strength, nearest first; one per id (the strongest), unknown strength last.
public func nearestFirst(_ heard: [Heard]) -> [Heard] {
    var seen = Set<String>()
    return heard.sorted { ($0.rssi ?? Int.min) > ($1.rssi ?? Int.min) }.filter { seen.insert($0.card.id).inserted }
}

/// One scan: every advertisement goes to `onAdvert` until `seconds` pass or it returns true (enough).
public final class BLEScan: NSObject, CBCentralManagerDelegate {
    private let queue = DispatchQueue(label: "luch.ble")
    private var central: CBCentralManager?
    private let ready = DispatchSemaphore(value: 0)
    private let finished = DispatchSemaphore(value: 0)
    private var onAdvert: ((UUID, [String: Any], Int?) -> Bool)?
    private var stopped = false

    public override init() {}

    /// Blocks for the scan; throws when Bluetooth is off, not allowed, or missing.
    public func run(seconds: TimeInterval, onAdvert: @escaping (UUID, [String: Any], Int?) -> Bool) throws {
        self.onAdvert = onAdvert
        // The first use asks the person for Bluetooth (NSBluetoothAlwaysUsageDescription); the state comes after.
        central = CBCentralManager(delegate: self, queue: queue)
        _ = ready.wait(timeout: .now() + 30)
        let state = queue.sync { central?.state ?? .unknown }
        switch state {
        case .poweredOn: break
        case .poweredOff: throw FindError("Bluetooth is off")
        case .unauthorized: throw FindError("Luch is not allowed to use Bluetooth (System Settings, Privacy & Security, Bluetooth)")
        case .unsupported: throw FindError("no Bluetooth LE on this Mac")
        default: throw FindError("Bluetooth did not get ready")
        }
        queue.sync {
            // Every advertisement, not only the first per device: the RSSI follows the person moving the laptop.
            central?.scanForPeripherals(withServices: nil, options: [CBCentralManagerScanOptionAllowDuplicatesKey: true])
        }
        _ = finished.wait(timeout: .now() + seconds)
        queue.sync {
            stopped = true
            central?.stopScan()
            central?.delegate = nil
            central = nil
        }
    }

    public func centralManagerDidUpdateState(_ central: CBCentralManager) {
        if central.state != .unknown && central.state != .resetting { ready.signal() }
    }

    public func centralManager(_ central: CBCentralManager, didDiscover peripheral: CBPeripheral,
                               advertisementData: [String: Any], rssi RSSI: NSNumber) {
        guard !stopped, let onAdvert else { return }
        let rssi = RSSI.intValue == 127 ? nil : RSSI.intValue // 127: not known
        if onAdvert(peripheral.identifier, advertisementData, rssi) {
            stopped = true
            finished.signal()
        }
    }
}

public extension BLE {
    /// Listens for receivers' cards for `seconds`; returns them nearest first. Each new receiver also goes to
    /// `heard` the moment it is heard, for the list to grow while it listens.
    static func search(seconds: TimeInterval, heard: @escaping (Heard) -> Void) throws -> [Heard] {
        var cards: [UUID: Heard] = [:]
        try BLEScan().run(seconds: seconds) { device, adv, rssi in
            if let c = card(adv[CBAdvertisementDataManufacturerDataKey] as? Data) {
                let h = Heard(card: c, rssi: rssi, device: device)
                if cards[device] == nil { heard(h) }
                cards[device] = h
            } else if let rssi, cards[device] != nil {
                cards[device]?.rssi = rssi
            }
            return false
        }
        return nearestFirst(Array(cards.values))
    }

    /// Listens up to `seconds` for a remote's power-key advertisement and returns its MediaTek data.
    static func learn(seconds: TimeInterval) throws -> [UInt8] {
        var got: [UInt8]?
        try BLEScan().run(seconds: seconds) { _, adv, _ in
            got = remote(adv[CBAdvertisementDataManufacturerDataKey] as? Data)
            return got != nil
        }
        guard let got else {
            throw FindError("no remote heard in \(Int(seconds)) s: turn the receiver off, then press the remote's power key near this computer")
        }
        return got
    }
}
