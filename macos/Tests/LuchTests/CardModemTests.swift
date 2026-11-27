// The card, Reed-Solomon and the sound modem, with luchd's vectors (find/card.rs, rs.rs, modem.rs): both senders and
// the receiver must agree byte for byte and tone for tone.
// SPDX-License-Identifier: GPL-3.0-or-later
import AVFoundation
@testable import LuchFind
import XCTest

/// A small, repeatable source of test noise.
struct Lcg {
    var state: UInt64
    mutating func next() -> UInt64 {
        state = state &* 6364136223846793005 &+ 1442695040888963407
        return state >> 33
    }

    mutating func uniform() -> Float {
        state = state &* 6364136223846793005 &+ 1442695040888963407
        return (Float(state >> 40) + 0.5) / Float(1 << 24)
    }

    /// Box-Muller.
    mutating func gauss() -> Float {
        let (u, v) = (uniform(), uniform())
        return (-2 * log(u)).squareRoot() * cos(2 * Float.pi * v)
    }
}

let tv192 = (UInt32(192) << 24 | 168 << 16 | 1 << 8 | 20)

final class CardTests: XCTestCase {
    private func tv() -> Card { Card(kind: Card.receiver, version: 0, id: "3f0c2a8e", addr: (tv192, 47800), token: "0badf00d") }

    func testCardsRoundTripAndFitAnAdvertisement() {
        let b = tv().encode()
        XCTAssertEqual(hexString(b), "4c75013f0c2a8ec0a80114bab80badf00d")
        XCTAssertEqual(Card.decode(b), tv())
        var ble = tv()
        ble.token = nil
        XCTAssertEqual(ble.encode().count, 13, "over Bluetooth there is no token")
        XCTAssertEqual(Card.decode(ble.encode()), ble)
        let me = Card.looking("9a1b2c3d-0000-4000-8000-000000000000")
        XCTAssertEqual(me.encode(), [0x4c, 0x75, 0x02, 0x9a, 0x1b, 0x2c, 0x3d])
        XCTAssertEqual(Card.decode(me.encode()), me)
    }

    func testStrangersAreNotCards() {
        XCTAssertNil(Card.decode(Array("Lu".utf8)))
        XCTAssertNil(Card.decode([0x4c, 0x76, 1, 0, 0, 0, 0]))
        XCTAssertNil(Card.decode([0x4c, 0x75, 0x11, 1, 2, 3, 4, 10, 0, 0, 1, 0, 80]), "a future version")
        XCTAssertNil(Card.decode([0x4c, 0x75, 0x07, 1, 2, 3, 4]), "an unknown kind")
        XCTAssertNil(Card.decode([0x4c, 0x75, 0x01, 1, 2, 3, 4]), "a receiver with no address")
    }

    func testLinksFromTheQRCodeAndFromAPerson() {
        XCTAssertEqual(Card.parseLink("luch://connect?host=192.168.1.20&port=47800&id=3F0C2A8E&t=0badf00d&new=1"), tv())
        let old = Card.parseLink("luch://connect?host=192.168.1.20&port=47801")!
        XCTAssertEqual(old.addr?.ip, tv192)
        XCTAssertEqual(old.addr?.port, 47801)
        XCTAssertNil(old.token)
        XCTAssertEqual(old.id, "")
        XCTAssertEqual(Card.parseLink("192.168.1.21")?.addr?.port, 47800)
        XCTAssertEqual(Card.parseLink(" 192.168.1.21:5000 ")?.addr?.port, 5000)
        XCTAssertEqual(Card.parseLink(" 192.168.1.21:5000 ")?.addr?.ip, tv192 + 1)
        for bad in ["", "luch://connect?port=1", "tv.local", "192.168.1.300", "192.168.1.2:0", "http://x/"] {
            XCTAssertNil(Card.parseLink(bad), bad)
        }
    }
}

final class ReedSolomonTests: XCTestCase {
    func testKnownCodeword() {
        // Shared with the receiver's ChirpTest and luchd: the very same parity.
        XCTAssertEqual(hexString(ReedSolomon.encode(Array("Luch".utf8), 8)), "4c7563681c7c9b839d89732d")
    }

    func testCorrectsUpToHalfTheParityAndRefusesMore() {
        var rng = Lcg(state: 7)
        for round in 0..<300 {
            let len = 1 + Int(rng.next() % 40)
            let msg = (0..<len).map { _ in UInt8(truncatingIfNeeded: rng.next()) }
            let good = ReedSolomon.encode(msg, 8)
            var bad = good
            let errors = round % 5
            var hit = Set<Int>()
            while hit.count < errors { hit.insert(Int(rng.next() % UInt64(bad.count))) }
            for i in hit { bad[i] ^= UInt8(1 + rng.next() % 255) }
            XCTAssertEqual(ReedSolomon.correct(&bad, 8), errors, "round \(round)")
            XCTAssertEqual(bad, good)
        }
        var refused = 0
        for _ in 0..<200 {
            let good = ReedSolomon.encode((0..<16).map { _ in UInt8(truncatingIfNeeded: rng.next()) }, 8)
            var bad = good
            for i in 0..<6 { bad[i * 3] ^= 0x5a }
            if ReedSolomon.correct(&bad, 8) == nil { refused += 1 } else { XCTAssertNotEqual(bad, good) }
        }
        XCTAssertGreaterThan(refused, 190)
    }
}

final class ModemTests: XCTestCase {
    /// The card a receiver says: kind 1, id 3f0c2a8e, 192.168.1.20:47800, token 0badf00d.
    let card: [UInt8] = [0x4c, 0x75, 0x01, 0x3f, 0x0c, 0x2a, 0x8e, 192, 168, 1, 20, 0xba, 0xb8, 0x0b, 0xad, 0xf0, 0x0d]

    /// What a room does: silence before, `gain`, white noise of `noise` RMS, a sample clock `ppm` off.
    func room(_ signal: [Float], lead: Int, gain: Float, noise: Float, ppm: Double, seed: UInt64) -> [Float] {
        let ratio = 1 + ppm * 1e-6
        let n = Int(Double(signal.count) * ratio)
        var rng = Lcg(state: seed)
        var out = (0..<lead).map { _ in noise * rng.gauss() }
        for i in 0..<(n + Modem.rate / 2) {
            let pos = Double(i) / ratio
            let k = Int(pos)
            let frac = Float(pos - pos.rounded(.down))
            let s: Float
            if k + 1 < signal.count { s = signal[k] + (signal[k + 1] - signal[k]) * frac } else if k < signal.count { s = signal[k] } else { s = 0 }
            out.append(gain * s + noise * rng.gauss())
        }
        return out
    }

    func testTheSymbolsAreTheReceiversToo() {
        let s = Modem.symbols(card)
        XCTAssertEqual(s.count, Modem.preamble.count + 2 * (1 + card.count + Modem.parity))
        XCTAssertEqual(s.map { String($0, radix: 16) }.joined(), "0f3c691e114c75013f0c2a8ec0a80114bab80badf00d1f8320c0e4ed937c")
    }

    func testAQuietFrameInANoisyRoomWithASkewedClock() {
        let x = Modem.synth(card, band: Modem.bands[0], amp: 0.5)
        XCTAssertEqual(x.count, Modem.symbols(card).count * Modem.symbol)
        XCTAssertEqual(Modem.demodulate(room(x, lead: 5000, gain: 1, noise: 0, ppm: 0, seed: 1)), [card])
        // 40 dB down and noise louder than the signal itself; clocks 500 ppm apart either way.
        for (seed, ppm) in [(UInt64(2), 500.0), (3, -500.0)] {
            XCTAssertEqual(Modem.demodulate(room(x, lead: 12_345, gain: 0.01, noise: 0.01, ppm: ppm, seed: seed)), [card], "ppm \(ppm)")
        }
    }

    func testTheAudibleBandWithAHumAndABurst() {
        var heard = room(Modem.synth(Array("hi".utf8), band: Modem.bands[1], amp: 0.3), lead: 777, gain: 0.5, noise: 0.02, ppm: 100, seed: 5)
        // A steady whistle right on one of the tones, a little quieter than the frame.
        for i in heard.indices { heard[i] += 0.1 * sin(2 * Float.pi * (Modem.bands[1] + 7 * Modem.spacing) * Float(i) / Float(Modem.rate)) }
        XCTAssertEqual(Modem.demodulate(heard), [Array("hi".utf8)])

        // A clap: 120 ms (three symbols) of loud broadband noise in the middle of the data.
        var clap = room(Modem.synth(card, band: Modem.bands[0], amp: 0.5), lead: 4000, gain: 0.2, noise: 0.002, ppm: 0, seed: 6)
        var rng = Lcg(state: 9)
        let from = 4000 + (Modem.preamble.count + 10) * Modem.symbol
        for i in from..<from + 3 * Modem.symbol { clap[i] += 0.8 * rng.gauss() }
        XCTAssertEqual(Modem.demodulate(clap), [card])
    }

    func testNoiseAloneSaysNothingAndTwoFramesAreBothHeard() {
        var rng = Lcg(state: 11)
        XCTAssertTrue(Modem.demodulate((0..<Modem.rate * 2).map { _ in 0.1 * rng.gauss() }).isEmpty)
        XCTAssertTrue(Modem.demodulate([]).isEmpty)
        var x = Modem.synth(Array("first".utf8), band: Modem.bands[0], amp: 0.5)
        x += [Float](repeating: 0, count: 3000)
        x += Modem.synth(Array("second".utf8), band: Modem.bands[0], amp: 0.5)
        let got = Modem.demodulate(room(x, lead: 100, gain: 0.1, noise: 0.005, ppm: 50, seed: 12))
        XCTAssertTrue(got.contains(Array("first".utf8)) && got.contains(Array("second".utf8)), "\(got)")
    }

    /// The microphone path without a microphone: a frame made at 44.1 kHz in two channels (what many Macs' inputs
    /// run at), through AVAudioConverter to the modem's 48 kHz mono, in buffers as the input tap gives them.
    func testAFrameThroughTheMicrophonePathsConverter() {
        let y = [Float](repeating: 0, count: 4410) + Modem.synth(card, band: Modem.bands[0], amp: 0.5, rate: 44_100)
            + [Float](repeating: 0, count: 22050)
        let format = AVAudioFormat(standardFormatWithSampleRate: 44_100, channels: 2)!
        let decoder = SoundDecoder()
        var heard: [[UInt8]] = []
        for start in stride(from: 0, to: y.count, by: 4410) {
            let n = min(4410, y.count - start)
            let b = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(n))!
            b.frameLength = AVAudioFrameCount(n)
            for c in 0..<2 { for i in 0..<n { b.floatChannelData![c][i] = y[start + i] } }
            let got = decoder.feed(b)
            if !got.isEmpty { heard = got }
        }
        if heard.isEmpty { heard = decoder.flush() }
        XCTAssertEqual(heard, [card])
    }
}
