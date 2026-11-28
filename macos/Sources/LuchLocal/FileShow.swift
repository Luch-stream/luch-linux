// "Show on TV with Luch" from Finder (its Services, or files dropped on the menu bar icon): which files it takes, and
// what to offer for them. One receiver and one thing to do runs at once; otherwise a short menu of choices.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum ShowVerb: String {
    /// A cast: photos as one slideshow, a video or a sound, a document page by page.
    case show
    /// A PDF or a presentation on the TV, the presenter view here.
    case present
}

public struct ShowTarget: Equatable {
    public var id: String
    public var name: String
    /// A Luch receiver; TVs without Luch take casts only.
    public var presents: Bool
    public init(id: String, name: String, presents: Bool) {
        self.id = id
        self.name = name
        self.presents = presents
    }
}

public struct ShowChoice: Equatable {
    public var title: String
    public var verb: ShowVerb
    public var receiver: String
}

/// What to do with files given to Luch.
public enum ShowPlan: Equatable {
    /// Nobody found yet (the service may have just started the app): look again shortly.
    case wait
    case run(ShowChoice)
    /// A menu of these.
    case ask([ShowChoice])
    case tell(String)
}

public enum FileShow {
    static let media: Set<String> = ["jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "bmp", "tif", "tiff",
                                     "mp4", "m4v", "mov", "mkv", "webm", "ts", "m2ts", "3gp", "avi",
                                     "mp3", "m4a", "m4b", "aac", "ogg", "oga", "opus", "flac", "wav", "aif", "aiff"]
    /// What the cast turns into PDF first (LuchCast's Cast.office and PDFs).
    static let documents: Set<String> = ["pdf", "doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "ppt", "pptx",
                                         "odp", "key"]
    static let slides: Set<String> = ["pdf", "key", "pptx", "ppt", "odp"]

    static func ext(_ path: String) -> String { (path as NSString).pathExtension.lowercased() }

    /// Every file is one the receiver can be handed.
    public static func accepts(_ paths: [String]) -> Bool {
        !paths.isEmpty && paths.allSatisfy { media.contains(ext($0)) || documents.contains(ext($0)) }
    }

    /// One PDF or presentation: it can be presented as well as shown.
    public static func presentable(_ paths: [String]) -> Bool {
        paths.count == 1 && slides.contains(ext(paths[0]))
    }

    /// What to offer for `verbs` on `receivers`, in the menu's order: each receiver's show, then its present.
    public static func choices(_ verbs: [ShowVerb], files: [String], receivers: [ShowTarget]) -> [ShowChoice] {
        guard accepts(files) else { return [] }
        var out: [ShowChoice] = []
        for r in receivers {
            for v in verbs {
                switch v {
                case .show: out.append(ShowChoice(title: "Show on \(r.name)", verb: .show, receiver: r.id))
                case .present where r.presents && presentable(files):
                    out.append(ShowChoice(title: "Present on \(r.name)…", verb: .present, receiver: r.id))
                case .present: break
                }
            }
        }
        return out
    }

    /// `canWait`: whether an empty list may still fill (the app has just started).
    public static func plan(_ verbs: [ShowVerb], files: [String], receivers: [ShowTarget], canWait: Bool) -> ShowPlan {
        if receivers.isEmpty {
            return canWait ? .wait : .tell("No TV found: is Luch open on the TV, on this network?")
        }
        let c = choices(verbs, files: files, receivers: receivers)
        if c.count == 1 { return .run(c[0]) }
        if !c.isEmpty { return .ask(c) }
        let name = files.count == 1 ? (files[0] as NSString).lastPathComponent : "\(files.count) files"
        if !accepts(files) { return .tell("Luch shows photos, videos, sound, PDFs and office files, not \(name)") }
        if verbs == [.present], !presentable(files) { return .tell("Present takes one PDF or presentation") }
        return .tell("None of these TVs can present: they are not Luch receivers")
    }

    /// The local API's request for a choice (Caster.handle), the receiver by id.
    public static func request(_ c: ShowChoice, files: [String]) -> [String: Any] {
        switch c.verb {
        case .show: return ["cmd": "cast", "items": files, "id": c.receiver]
        case .present: return ["cmd": "present", "file": files[0], "id": c.receiver]
        }
    }
}
