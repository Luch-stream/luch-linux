// The receiver's QR code through this Mac's camera (PROTOCOL.md 1e): the person holds the laptop up to the TV.
// Frames from AVFoundation, codes read by Vision; the link is the one luchd and the phone app read.
// SPDX-License-Identifier: GPL-3.0-or-later
import AVFoundation
import CoreImage
import Foundation
import Vision

public enum QR {
    /// Every QR text in a picture. The TV draws its code light on dark, so a picture with none is tried inverted.
    public static func texts(_ image: CIImage) -> [String] {
        let plain = read(image)
        if !plain.isEmpty { return plain }
        return read(image.applyingFilter("CIColorInvert"))
    }

    private static func read(_ image: CIImage) -> [String] {
        let request = VNDetectBarcodesRequest()
        request.symbologies = [.qr]
        try? VNImageRequestHandler(ciImage: image, options: [:]).perform([request])
        return (request.results ?? []).compactMap(\.payloadStringValue)
    }

    /// The receivers' cards in a picture: QR codes that are Luch links.
    public static func cards(_ image: CIImage) -> [Card] {
        texts(image).filter { $0.hasPrefix("luch://") }.compactMap(Card.parseLink)
    }

    /// Watches the default camera for up to `seconds`; returns the first receivers whose code it sees.
    public static func watch(seconds: TimeInterval) throws -> [Card] {
        try allowed(.video, what: "camera")
        guard let device = AVCaptureDevice.default(for: .video) else { throw FindError("no camera") }
        let session = AVCaptureSession()
        session.sessionPreset = .high
        do {
            session.addInput(try AVCaptureDeviceInput(device: device))
        } catch {
            throw FindError("cannot open the camera: \(error.localizedDescription)")
        }
        let output = AVCaptureVideoDataOutput()
        output.alwaysDiscardsLateVideoFrames = true
        let watcher = Watcher()
        output.setSampleBufferDelegate(watcher, queue: DispatchQueue(label: "luch.qr"))
        guard session.canAddOutput(output) else { throw FindError("cannot read the camera") }
        session.addOutput(output)
        session.startRunning()
        _ = watcher.found.wait(timeout: .now() + seconds)
        session.stopRunning()
        return watcher.cards
    }

    private final class Watcher: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate {
        let found = DispatchSemaphore(value: 0)
        private let lock = NSLock()
        private var seen: [Card] = []
        private var last = Date.distantPast

        var cards: [Card] {
            lock.lock()
            defer { lock.unlock() }
            return seen
        }

        func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer,
                           from connection: AVCaptureConnection) {
            // A few looks a second are plenty, and leave the laptop cool.
            guard Date().timeIntervalSince(last) > 0.2, cards.isEmpty,
                  let pixels = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
            last = Date()
            let c = QR.cards(CIImage(cvPixelBuffer: pixels))
            guard !c.isEmpty else { return }
            lock.lock()
            seen = c
            lock.unlock()
            found.signal()
        }
    }
}
