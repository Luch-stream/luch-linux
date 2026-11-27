// Answers a group receiver's clock requests (PROTOCOL.md 4k) on the media socket they come back on: a request is
// answered the moment it is read, with t2 and t3 on the pts clock, and nothing is kept; it runs until stop().
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import LuchProto

public final class ClockResponder {
    private let sender: UdpSender
    private let lock = NSLock()
    private var stopped = false

    /// `now`: the pts clock in µs (what every pts of the session is on), nil before it runs.
    public init(on sender: UdpSender, now: @escaping () -> UInt64?) {
        self.sender = sender
        // Wakes up now and then to see whether it was stopped; the socket is the sender's, so it is not closed here.
        var tv = timeval(tv_sec: 0, tv_usec: 200_000)
        setsockopt(sender.fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        let fd = sender.fd
        let thread = Thread { [self] in
            var buf = [UInt8](repeating: 0, count: 128)
            while !isStopped {
                // Timeouts, and the ICMP "unreachable" of a receiver not listening yet: go on.
                let n = recv(fd, &buf, buf.count, 0)
                guard n > 0, let reply = Sync.answer(Array(buf[0..<n]), now: now) else { continue }
                _ = reply.withUnsafeBytes { Darwin.send(fd, $0.baseAddress, $0.count, 0) }
            }
        }
        thread.name = "luch.clock.\(sender.kind == .video ? "video" : "audio")"
        thread.qualityOfService = .userInteractive
        thread.start()
    }

    private var isStopped: Bool {
        lock.lock()
        defer { lock.unlock() }
        return stopped
    }

    /// Ends the answering thread within 0.2 s.
    public func stop() {
        lock.lock()
        stopped = true
        lock.unlock()
    }
}
