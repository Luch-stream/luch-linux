// What a Core Audio device adds between the air and the host time a buffer carries (input), or between a buffer's host
// time and the air (output): the device's latency, its safety offset and its stream's latency. The last is the big
// one on a MacBook (its microphones' beamforming, 30 ms; its speakers' processing, 14 ms), and AVAudioEngine's
// presentationLatency leaves it out: measured with `luchctl calibrate self`, speaker to microphone, 47.1 ms against
// these figures' 48.1.
// SPDX-License-Identifier: GPL-3.0-or-later
import CoreAudio
import Foundation

public enum AudioLatency {
    /// The default input device's, in seconds (0 when Core Audio does not say).
    public static var input: Double { of(kAudioHardwarePropertyDefaultInputDevice, kAudioObjectPropertyScopeInput) }
    /// The default output device's, in seconds.
    public static var output: Double { of(kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyScopeOutput) }

    private static func of(_ which: AudioObjectPropertySelector, _ scope: AudioObjectPropertyScope) -> Double {
        let device: AudioObjectID = get(AudioObjectID(kAudioObjectSystemObject), which, kAudioObjectPropertyScopeGlobal,
                                        AudioObjectID(0))
        guard device != 0 else { return 0 }
        let rate: Float64 = get(device, kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, 0)
        guard rate > 0 else { return 0 }
        let latency: UInt32 = get(device, kAudioDevicePropertyLatency, scope, 0)
        let safety: UInt32 = get(device, kAudioDevicePropertySafetyOffset, scope, 0)
        var address = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyStreams, mScope: scope,
                                                 mElement: kAudioObjectPropertyElementMain)
        var size: UInt32 = 0
        AudioObjectGetPropertyDataSize(device, &address, 0, nil, &size)
        var streams = [AudioStreamID](repeating: 0, count: Int(size) / MemoryLayout<AudioStreamID>.size)
        AudioObjectGetPropertyData(device, &address, 0, nil, &size, &streams)
        // The first stream: one device, one direction, one stream on everything seen so far.
        let stream: UInt32 = streams.first.map { get($0, kAudioStreamPropertyLatency, kAudioObjectPropertyScopeGlobal, 0) } ?? 0
        return Double(latency + safety + stream) / rate
    }

    private static func get<T>(_ object: AudioObjectID, _ selector: AudioObjectPropertySelector,
                               _ scope: AudioObjectPropertyScope, _ fallback: T) -> T {
        var address = AudioObjectPropertyAddress(mSelector: selector, mScope: scope,
                                                 mElement: kAudioObjectPropertyElementMain)
        var value = fallback
        var size = UInt32(MemoryLayout<T>.size)
        return AudioObjectGetPropertyData(object, &address, 0, nil, &size, &value) == noErr ? value : fallback
    }
}
