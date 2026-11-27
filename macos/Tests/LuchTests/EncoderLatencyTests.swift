// SPDX-License-Identifier: GPL-3.0-or-later
import CoreMedia
import XCTest
@testable import LuchMedia

/// How long a frame spends in the encoder, per kind of encoder: the same synthetic frames at 60 fps, the time from
/// handing a frame in to its access unit coming out. About 1.5 minutes; prints the table. Opt-in:
/// LUCH_TEST_ENCODER_LATENCY=1 swift test --filter EncoderLatencyTests
final class EncoderLatencyTests: XCTestCase {
    struct Result {
        var mode: EncoderMode
        var median: Double
        var p95: Double
        var frames: Int
    }

    private func measure(_ codec: VideoCodec, _ w: Int, _ h: Int, _ modes: [EncoderMode]) -> Result? {
        guard let enc = VideoEncoder(codec: codec, width: w, height: h, fps: 60, kbps: 20_000, modes: modes) else {
            return nil
        }
        var sent: [Int64: CFAbsoluteTime] = [:]
        var took: [Double] = []
        let lock = NSLock()
        enc.onPacket = { _, _, pts in
            let now = CFAbsoluteTimeGetCurrent()
            lock.lock()
            if let t = sent[pts.value] { took.append((now - t) * 1000) }
            lock.unlock()
        }
        // Drawn before, not while timing: a frame drawn pixel by pixel in a debug build takes longer than 1/60 s.
        let drawn = (0..<8).compactMap { SyntheticFrame.make(width: w, height: h, index: $0 * 7) }
        guard !drawn.isEmpty else { return nil }
        let frames = 120
        let start = CFAbsoluteTimeGetCurrent()
        for i in 0..<frames {
            let buffer = drawn[i % drawn.count]
            let pts = CMTime(value: Int64(i), timescale: 60)
            lock.lock()
            sent[pts.value] = CFAbsoluteTimeGetCurrent()
            lock.unlock()
            enc.encode(buffer, pts: pts, forceIDR: i == 0)
            let next = start + Double(i + 1) / 60
            let wait = next - CFAbsoluteTimeGetCurrent()
            if wait > 0 { Thread.sleep(forTimeInterval: wait) }
        }
        Thread.sleep(forTimeInterval: 0.3)
        lock.lock()
        let all = took.dropFirst(5).sorted() // the first frames warm the encoder up
        lock.unlock()
        guard !all.isEmpty else { return nil }
        return Result(mode: enc.mode, median: all[all.count / 2], p95: all[min(all.count - 1, all.count * 95 / 100)],
                      frames: all.count)
    }

    /// Prints the table; checks only that the chain always ends in an encoder and that each mode is what opened.
    func testEachEncoderAndItsTimeInTheEncoder() throws {
        try XCTSkipUnless(ProcessInfo.processInfo.environment["LUCH_TEST_ENCODER_LATENCY"] == "1")
        var lines: [String] = []
        for codec in [VideoCodec.h264, .hevc] {
            for (w, h) in [(1920, 1080), (3840, 2160)] {
                for m in EncoderMode.allCases {
                    guard let r = measure(codec, w, h, [m]) else {
                        lines.append("\(codec.label) \(w)x\(h) \(m.rawValue): did not open")
                        continue
                    }
                    XCTAssertEqual(r.mode, m)
                    lines.append(String(format: "%@ %dx%d %@: median %.2f ms, p95 %.2f ms (%d frames)",
                                        codec.label, w, h, m.rawValue, r.median, r.p95, r.frames))
                }
                XCTAssertNotNil(measure(codec, w, h, EncoderMode.standard), "the chain ends in some encoder")
            }
        }
        print("ENCODER LATENCY\n" + lines.joined(separator: "\n"))
    }
}
