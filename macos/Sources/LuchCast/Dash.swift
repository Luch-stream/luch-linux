// A DASH manifest relayed through this Mac (PROTOCOL.md 4b, via "sender"): its links made to point back here. A
// manifest names its pieces relative to a base (its own address, or a BaseURL) with templates the player fills in
// ($Number$, $RepresentationID$), so the links cannot be shared one by one as an HLS playlist's are: a folder is
// shared instead, and everything the player asks under it is fetched from the same folder at the origin. The same
// rules as luch-linux's luchd/src/cast.rs (rewrite_mpd).
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum Dash {
    public static func isManifest(url: String, contentType: String) -> Bool {
        if contentType.lowercased().contains("dash+xml") { return true }
        let path = url.split(whereSeparator: { $0 == "?" || $0 == "#" }).first.map(String.init) ?? url
        return path.lowercased().hasSuffix(".mpd")
    }

    /// `url` cut where a folder ends: before the last "/" ahead of any template ("$Number$"), which the player fills in
    /// and must stay out of a shared folder's fixed part. Returns (folder ending in "/", the rest).
    static func split(_ url: String) -> (String, String) {
        let head = url.firstIndex(where: { $0 == "$" || $0 == "?" }).map { String(url[..<$0]) } ?? url
        guard let slash = head.lastIndex(of: "/") else { return (url, "") }
        let cut = url.index(after: slash)
        return (String(url[..<cut]), String(url[cut...]))
    }

    /// Every link of a manifest at `base` made ours: absolute BaseURLs and absolute piece links (`media`,
    /// `initialization`, `sourceURL`, `index`) through `folder` (an origin folder → our link for it, ending in "/"),
    /// and a BaseURL of the manifest's own folder added first for everything relative.
    public static func rewrite(_ text: String, base: String, folder: (String) -> String) -> String {
        var out = text
        func ours(_ link: String) -> String {
            let (dir, rest) = split(Playlist.resolve(base: base, link))
            return folder(dir) + rest
        }
        let period = out.range(of: "<Period")?.lowerBound
        var topLevelBase = false
        // <BaseURL>…</BaseURL> and <Location>…</Location>: absolute ones, and a relative one at the manifest's top
        // (which would resolve against our own link), are made ours; nested relative ones follow the one above them.
        for tag in ["BaseURL", "Location"] {
            var from = out.startIndex
            while let open = out.range(of: "<\(tag)", range: from..<out.endIndex),
                  let gt = out[open.upperBound...].firstIndex(of: ">"),
                  let close = out.range(of: "</\(tag)>", range: out.index(after: gt)..<out.endIndex) {
                let valueRange = out.index(after: gt)..<close.lowerBound
                let value = out[valueRange].trimmingCharacters(in: .whitespacesAndNewlines)
                let top = period.map { open.lowerBound < $0 } ?? true
                if tag == "BaseURL" && top { topLevelBase = true }
                let absolute = value.contains("://") || value.hasPrefix("//")
                var next = close.upperBound
                if !value.isEmpty && (absolute || top) {
                    let new = ours(value)
                    out.replaceSubrange(valueRange, with: new)
                    next = out.index(valueRange.lowerBound, offsetBy: new.count)
                }
                from = next
            }
        }
        for attr in ["media", "initialization", "sourceURL", "index"] {
            var from = out.startIndex
            while let r = out.range(of: " \(attr)=\"", range: from..<out.endIndex),
                  let close = out[r.upperBound...].firstIndex(of: "\"") {
                let value = String(out[r.upperBound..<close])
                var next = out.index(after: close)
                if value.contains("://") || value.hasPrefix("//") {
                    let new = ours(value)
                    out.replaceSubrange(r.upperBound..<close, with: new)
                    next = out.index(r.upperBound, offsetBy: new.count + 1)
                }
                from = next
            }
        }
        if !topLevelBase, let mpd = out.range(of: "<MPD"), let gt = out[mpd.upperBound...].firstIndex(of: ">") {
            out.insert(contentsOf: "<BaseURL>\(folder(split(base).0))</BaseURL>", at: out.index(after: gt))
        }
        return out
    }
}
