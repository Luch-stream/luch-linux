// A receiver that asks for pairing (PROTOCOL.md 3a): the code it shows is typed on this terminal, then the command
// goes on, as luchctl does on Linux.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import LuchNet
import LuchProto

/// Runs `body`; when it finds the receiver asking for pairing and there is a terminal, asks for the code here, pairs
/// and runs it again. Any other failure ends the command.
@discardableResult
func pairingIfAsked<T>(_ body: () throws -> T) -> T {
    do {
        do {
            return try body()
        } catch let ConnectError.pairingNeeded(id, name, _) where isatty(STDIN_FILENO) != 0 {
            try pairOnTerminal(id, name: name)
            return try body()
        }
    } catch {
        log("start failed: \(error.localizedDescription)")
        exit(1)
    }
}

/// Waits for the TV's allow (when it asks its viewers first), then for the code typed here.
private func pairOnTerminal(_ id: String, name: String) throws {
    var told = false
    let until = Date().addingTimeInterval(70)
    while true {
        let st = Connector.shared.pairing(id)
        switch st["step"] as? String {
        case "code":
            print("Code shown on \(name): ", terminator: "")
            fflush(stdout)
            try Connector.shared.pair(id, code: readLine() ?? "")
            print("paired with \(name): from now on it lets this computer in")
            return
        case "allow" where !told:
            print("\(name) is asking its viewers first: allow the connection on the TV")
            told = true
        case "failed":
            throw ConnectError.failed(st["reason"] as? String ?? "pairing failed")
        case "paired":
            return
        default:
            break
        }
        if Date() > until { throw ConnectError.failed("nobody answered on the TV") }
        usleep(300_000)
    }
}
