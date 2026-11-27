// The Bluetooth LE channel to a receiver that offers a direct connection: connect to its advertisement, open an
// L2CAP connection-oriented channel on the PSM its card names (CBPeripheral.openL2CAPChannel). Its two streams then
// carry the receiver's TLS control protocol byte for byte (StreamBridge).
// SPDX-License-Identifier: GPL-3.0-or-later
import CoreBluetooth
import Foundation

/// An open channel: its streams, and how to end it.
public struct DirectChannel {
    public let input: InputStream
    public let output: OutputStream
    public let close: () -> Void

    public init(input: InputStream, output: OutputStream, close: @escaping () -> Void) {
        self.input = input
        self.output = output
        self.close = close
    }
}

/// Opens channels to receivers heard over the air (tests hand out stream pairs instead).
public protocol DirectChannelOpener {
    func open(_ heard: Heard, timeout: TimeInterval) throws -> DirectChannel
}

public struct BLEChannels: DirectChannelOpener {
    public init() {}

    public func open(_ heard: Heard, timeout: TimeInterval) throws -> DirectChannel {
        guard heard.card.psm != 0 else { throw FindError("receiver \(heard.card.id) offers no direct connection") }
        let session = L2CAPSession(heard)
        let channel = try session.open(timeout: timeout)
        // The session holds the central, the peripheral and the channel: all three must live while it is used.
        return DirectChannel(input: channel.inputStream, output: channel.outputStream, close: { session.close() })
    }
}

/// One channel: its own central, so that nothing else (a scan) gets its delegate calls.
final class L2CAPSession: NSObject, CBCentralManagerDelegate, CBPeripheralDelegate {
    private let heard: Heard
    private let queue = DispatchQueue(label: "luch.l2cap")
    private var central: CBCentralManager?
    private var peripheral: CBPeripheral?
    private var channel: CBL2CAPChannel?
    private var failure: String?
    private let ready = DispatchSemaphore(value: 0)
    private let event = DispatchSemaphore(value: 0)

    init(_ heard: Heard) { self.heard = heard }

    func open(timeout: TimeInterval) throws -> CBL2CAPChannel {
        central = CBCentralManager(delegate: self, queue: queue)
        _ = ready.wait(timeout: .now() + 10)
        let state = queue.sync { central?.state ?? .unknown }
        guard state == .poweredOn else {
            close()
            switch state {
            case .poweredOff: throw FindError("Bluetooth is off")
            case .unauthorized: throw FindError("Luch is not allowed to use Bluetooth (System Settings, Privacy & Security, Bluetooth)")
            default: throw FindError("Bluetooth did not get ready")
            }
        }
        queue.sync {
            // Known to the system since the scan that heard it; heard again otherwise (its address may have changed).
            if let p = central?.retrievePeripherals(withIdentifiers: [heard.device]).first {
                connect(p)
            } else {
                central?.scanForPeripherals(withServices: nil)
            }
        }
        let inTime = event.wait(timeout: .now() + timeout) == .success
        let (got, why) = queue.sync { (channel, failure) }
        guard inTime, let got, why == nil else {
            close()
            throw FindError(why ?? "receiver \(heard.card.id) did not open its Bluetooth channel in time (is it still near?)")
        }
        return got
    }

    func close() {
        queue.sync {
            channel?.inputStream?.close()
            channel?.outputStream?.close()
            channel = nil
            central?.stopScan()
            if let peripheral { central?.cancelPeripheralConnection(peripheral) }
            central?.delegate = nil
            peripheral?.delegate = nil
        }
    }

    private func connect(_ p: CBPeripheral) {
        peripheral = p
        p.delegate = self
        central?.connect(p)
    }

    private func fail(_ why: String) {
        guard channel == nil, failure == nil else { return }
        failure = why
        event.signal()
    }

    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        if central.state != .unknown && central.state != .resetting { ready.signal() }
    }

    func centralManager(_ central: CBCentralManager, didDiscover p: CBPeripheral, advertisementData: [String: Any],
                        rssi RSSI: NSNumber) {
        guard peripheral == nil, BLE.card(advertisementData[CBAdvertisementDataManufacturerDataKey] as? Data)?.id == heard.card.id
        else { return }
        central.stopScan()
        connect(p)
    }

    func centralManager(_ central: CBCentralManager, didConnect p: CBPeripheral) {
        p.openL2CAPChannel(CBL2CAPPSM(heard.card.psm))
    }

    func centralManager(_ central: CBCentralManager, didFailToConnect p: CBPeripheral, error: Error?) {
        fail("could not connect to receiver \(heard.card.id) over Bluetooth\(error.map { ": \($0.localizedDescription)" } ?? "")")
    }

    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral p: CBPeripheral, error: Error?) {
        fail("receiver \(heard.card.id) dropped the Bluetooth connection")
    }

    func peripheral(_ p: CBPeripheral, didOpen channel: CBL2CAPChannel?, error: Error?) {
        guard let channel, error == nil else {
            return fail("receiver \(heard.card.id) did not open its channel\(error.map { ": \($0.localizedDescription)" } ?? "")")
        }
        self.channel = channel
        event.signal()
    }
}
