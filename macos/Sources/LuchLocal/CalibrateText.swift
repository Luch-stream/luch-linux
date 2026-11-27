// What a calibration found, in words: `luchctl calibrate` prints it and the menu shows it.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum CalibrateText {
    /// Said before it starts, so the room is quiet and nobody is surprised by the whistles.
    public static func before(seconds: Double) -> String {
        "Each TV plays four short whistles in turn, about 3 seconds each (\(Int(seconds.rounded())) s in all), and " +
            "this Mac's microphone listens. Keep the room quiet until it is done."
    }

    /// One line per receiver: what was heard and the correction it plays with now, or why it was not measured.
    public static func lines(_ results: [[String: Any]]) -> [String] {
        func num(_ k: String, _ r: [String: Any]) -> Double? { (r[k] as? NSNumber)?.doubleValue }
        var out = results.map { r -> String in
            let name = r["name"] as? String ?? "?"
            let offset = String(format: "%+.1f ms", num("offset_ms", r) ?? 0)
            guard r["measured"] as? Bool == true else {
                let twice = r["retried"] as? Bool == true ? " (tried twice)" : ""
                return "\(name): not measured: \(r["why"] as? String ?? "?")\(twice); offset stays \(offset)"
            }
            var sure: [String] = []
            if let s = num("spread_ms", r) { sure.append(String(format: "sweeps within %.2f ms", s)) }
            if let p = num("psr_db", r) { sure.append(String(format: "%.0f dB over the noise", p)) }
            if r["retried"] as? Bool == true { sure.append("louder, the second time") }
            if r["src"] as? String == "head" { sure.append("its own timing is a guess (no DAC timestamps)") }
            return "\(name): heard \(String(format: "%.1f", num("delay_ms", r) ?? 0)) ms after it played; " +
                "offset now \(offset) (\(sure.joined(separator: ", ")))"
        }
        if results.contains(where: { $0["measured"] as? Bool == true }) {
            out.append("(heard − played includes this Mac's microphone delay, the same for every TV: only the " +
                       "differences matter)")
        }
        return out
    }
}
