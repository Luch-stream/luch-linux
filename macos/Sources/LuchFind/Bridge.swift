// A pair of streams (a Bluetooth L2CAP channel's) offered as a TCP port on 127.0.0.1, for the TLS that LuchNet runs
// over Network.framework connections only: the Connector connects there, and every byte goes through unchanged.
// The receiver's pin is checked by that TLS as for any receiver; the loopback port is only a pipe.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation

public final class StreamBridge: NSObject, StreamDelegate {
    /// Where to connect, on 127.0.0.1.
    public let port: UInt16
    /// The pipe ended (either side closed); called once, on the bridge's thread.
    public var onEnd: (() -> Void)?

    private let input: InputStream
    private let output: OutputStream
    private var listener: Int32
    private var fd: Int32 = -1
    private let lock = NSLock()
    /// From the TCP side, waiting for room in the stream.
    private var toStream = Data()
    private var runLoop: CFRunLoop?
    private var stopped = false
    private var ended = false
    private let served = DispatchSemaphore(value: 0)

    /// Listens at once; the first connection within `acceptWithin` is bridged, nobody else is let in.
    public init(input: InputStream, output: OutputStream, acceptWithin: TimeInterval = 30) throws {
        self.input = input
        self.output = output
        let s = socket(AF_INET, SOCK_STREAM, 0)
        guard s >= 0 else { throw FindError("no socket for the Bluetooth channel") }
        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_addr.s_addr = INADDR_LOOPBACK.bigEndian
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let ok = withUnsafeMutablePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(s, $0, len) == 0 && listen(s, 1) == 0 && getsockname(s, $0, &len) == 0
            }
        }
        guard ok else {
            Darwin.close(s)
            throw FindError("cannot listen on loopback for the Bluetooth channel")
        }
        listener = s
        port = UInt16(bigEndian: addr.sin_port)
        super.init()
        Thread.detachNewThread { [self] in serve(acceptWithin) }
    }

    /// Ends the pipe: the TCP side sees the connection close, the streams are closed.
    public func close() {
        lock.lock()
        stopped = true
        lock.unlock()
        perform { [self] in end() }
    }

    private var isStopped: Bool {
        lock.lock()
        defer { lock.unlock() }
        return stopped
    }

    private func serve(_ within: TimeInterval) {
        let deadline = Date().addingTimeInterval(within)
        var client: Int32 = -1
        while client < 0, !isStopped, Date() < deadline {
            var pfd = pollfd(fd: listener, events: Int16(POLLIN), revents: 0)
            if poll(&pfd, 1, 250) == 1 { client = accept(listener, nil, nil) }
        }
        Darwin.close(listener)
        guard client >= 0, !isStopped else {
            if client >= 0 { Darwin.close(client) }
            finish()
            return
        }
        var on: Int32 = 1
        setsockopt(client, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
        setsockopt(client, IPPROTO_TCP, TCP_NODELAY, &on, socklen_t(MemoryLayout<Int32>.size))
        lock.lock()
        fd = client
        runLoop = CFRunLoopGetCurrent()
        lock.unlock()
        for s in [input, output] as [Stream] {
            s.delegate = self
            s.schedule(in: .current, forMode: .default)
            s.open()
        }
        Thread.detachNewThread { [self] in readSocket(client) }
        while !isStopped { _ = RunLoop.current.run(mode: .default, before: Date(timeIntervalSinceNow: 0.25)) }
        for s in [input, output] as [Stream] {
            s.close()
            s.remove(from: .current, forMode: .default)
            s.delegate = nil
        }
        lock.lock()
        runLoop = nil
        lock.unlock()
        if client >= 0 { shutdown(client, SHUT_RDWR) }
        finish()
        served.signal()
    }

    /// TCP to the stream: what comes in is queued for the stream thread; the end of it ends the pipe.
    private func readSocket(_ s: Int32) {
        var buf = [UInt8](repeating: 0, count: 16384)
        while true {
            let n = recv(s, &buf, buf.count, 0)
            if n < 0, errno == EINTR { continue }
            guard n > 0 else { break }
            lock.lock()
            toStream.append(contentsOf: buf[0..<n])
            lock.unlock()
            perform { [self] in flush() }
        }
        lock.lock()
        stopped = true
        lock.unlock()
        perform { [self] in end() }
        // Closed only once the bridge's thread, which writes to it, is done.
        served.wait()
        Darwin.close(s)
    }

    /// Runs `block` on the bridge's thread.
    private func perform(_ block: @escaping () -> Void) {
        lock.lock()
        let rl = runLoop
        lock.unlock()
        guard let rl else { return }
        CFRunLoopPerformBlock(rl, CFRunLoopMode.defaultMode.rawValue, block)
        CFRunLoopWakeUp(rl)
    }

    private func flush() {
        while output.hasSpaceAvailable {
            lock.lock()
            let chunk = toStream.prefix(4096)
            lock.unlock()
            if chunk.isEmpty { return }
            let n = chunk.withUnsafeBytes { output.write($0.bindMemory(to: UInt8.self).baseAddress!, maxLength: chunk.count) }
            if n < 0 { return end() }
            if n == 0 { return }
            lock.lock()
            toStream.removeFirst(n)
            lock.unlock()
        }
    }

    public func stream(_ s: Stream, handle event: Stream.Event) {
        switch event {
        case .hasBytesAvailable where s === input:
            var buf = [UInt8](repeating: 0, count: 16384)
            while input.hasBytesAvailable {
                let n = input.read(&buf, maxLength: buf.count)
                guard n > 0 else { break }
                if !sendAll(Array(buf[0..<n])) { return end() }
            }
        case .hasSpaceAvailable:
            flush()
        case .endEncountered, .errorOccurred:
            end()
        default:
            break
        }
    }

    private func sendAll(_ bytes: [UInt8]) -> Bool {
        lock.lock()
        let s = fd
        lock.unlock()
        guard s >= 0 else { return false }
        var sent = 0
        while sent < bytes.count {
            let n = bytes.withUnsafeBytes { send(s, $0.baseAddress!.advanced(by: sent), bytes.count - sent, 0) }
            if n < 0, errno == EINTR { continue }
            guard n > 0 else { return false }
            sent += n
        }
        return true
    }

    /// On the bridge's thread: the TCP side is shut (its reader closes it), the run loop stops.
    private func end() {
        lock.lock()
        stopped = true
        let s = fd
        lock.unlock()
        if s >= 0 { shutdown(s, SHUT_RDWR) }
        finish()
    }

    private func finish() {
        lock.lock()
        let first = !ended
        ended = true
        lock.unlock()
        if first { onEnd?() }
    }
}
