// Speaker notes of a presentation, one entry per page of the PDF the receiver shows; the rules of luch-linux's
// luchd/src/notes.rs, so both senders show the same notes.
//
// LibreOffice turns .odp and .pptx into PDF without notes, so they are read from the original file: both are zip
// archives of XML with the notes as plain text. Hidden slides are left out, as LibreOffice leaves them out of the PDF.
// A PDF has its notes in a .pdfpc file next to it (pdfpc, Beamer) or in its text annotations, read with PDFKit.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import PDFKit

public enum Notes {
    /// The notes of each page of `pdf`, made from `original` (the same file when it is a PDF). Missing notes are "".
    public static func speakerNotes(original: URL, pdf: URL) -> [String] {
        let notes: [String]?
        switch original.pathExtension.lowercased() {
        case "odp": notes = ZipArchive(url: original).flatMap { $0.text("content.xml") }.map(odpContent)
        case "pptx": notes = ZipArchive(url: original).flatMap(pptxNotes)
        default: notes = nil
        }
        return notes
            ?? (try? String(contentsOf: pdf.deletingPathExtension().appendingPathExtension("pdfpc"), encoding: .utf8))
                .flatMap(pdfpc)
            ?? pdfNotes(pdf)
    }

    /// Lines of text joined, the blank ones at both ends cut.
    static func tidy(_ lines: [String]) -> String {
        lines.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
    }

    // MARK: - OpenDocument (content.xml)

    /// The notes of each shown slide of an ODP's content.xml.
    public static func odpContent(_ xml: String) -> [String] {
        var hiddenStyles = Set<String>()
        var style: String?
        var slides: [(hidden: Bool, lines: [String])] = []
        // Where the reader is: in a slide's notes, in their text frame, in a paragraph.
        var inNotes = false, inFrame = false
        var para: String?
        XML.walk(xml, start: { name, a in
            switch name {
            case "style:style":
                style = a["style:family"] == "drawing-page" ? a["style:name"] : nil
            case "style:drawing-page-properties":
                if let s = style, a["presentation:visibility"] == "hidden" { hiddenStyles.insert(s) }
            case "draw:page":
                slides.append((a["draw:style-name"].map(hiddenStyles.contains) ?? false, []))
            case "presentation:notes":
                inNotes = true
            case "draw:frame" where inNotes:
                inFrame = a["presentation:class"] == "notes"
            case "text:p" where inFrame, "text:h" where inFrame:
                para = ""
            case "text:s" where inFrame:
                para?.append(String(repeating: " ", count: min(Int(a["text:c"] ?? "") ?? 1, 64)))
            case "text:tab" where inFrame:
                para?.append("\t")
            case "text:line-break" where inFrame:
                para?.append("\n")
            default:
                break
            }
            return true
        }, end: { name in
            switch name {
            case "style:style": style = nil
            case "presentation:notes": inNotes = false
            case "draw:frame": inFrame = false
            case "text:p", "text:h":
                if let p = para, !slides.isEmpty { slides[slides.count - 1].lines.append(p) }
                para = nil
            default: break
            }
        }, text: { t in
            if inFrame { para?.append(t) }
        })
        return slides.filter { !$0.hidden }.map { tidy($0.lines) }
    }

    // MARK: - PowerPoint (ppt/...)

    /// `target` of a relationship, relative to the folder of the part `from` ("ppt/slides/slide1.xml").
    static func resolvePart(_ from: String, _ target: String) -> String {
        if target.hasPrefix("/") { return String(target.dropFirst()) }
        var parts = from.split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        parts.removeLast()
        for seg in target.split(separator: "/", omittingEmptySubsequences: false) {
            switch seg {
            case "..": if !parts.isEmpty { parts.removeLast() }
            case ".", "": break
            default: parts.append(String(seg))
            }
        }
        return parts.joined(separator: "/")
    }

    /// The relationships of a part: id -> (type, target part).
    static func relationships(_ a: ZipArchive, _ part: String) -> [String: (kind: String, part: String)] {
        let (dir, file) = part.lastIndex(of: "/").map { (String(part[..<$0]), String(part[part.index(after: $0)...])) }
            ?? ("", part)
        guard let xml = a.text("\(dir)/_rels/\(file).rels") else { return [:] }
        var out: [String: (String, String)] = [:]
        XML.walk(xml, start: { name, attrs in
            if name.split(separator: ":").last == "Relationship", let id = attrs["Id"], let t = attrs["Target"] {
                out[id] = (attrs["Type"] ?? "", resolvePart(part, t))
            }
            return true
        })
        return out
    }

    static func pptxNotes(_ a: ZipArchive) -> [String]? {
        let pres = "ppt/presentation.xml"
        guard let xml = a.text(pres) else { return nil }
        let rels = relationships(a, pres)
        var order: [String] = []
        XML.walk(xml, start: { name, attrs in
            if name == "p:sldId", let part = attrs["r:id"].flatMap({ rels[$0] }) { order.append(part.part) }
            return true
        })
        return order.compactMap { slide in
            guard let xml = a.text(slide), !pptxHidden(xml) else { return nil }
            return relationships(a, slide).values.first { $0.kind.hasSuffix("/notesSlide") }
                .flatMap { a.text($0.part) }.map(pptxNotesText) ?? ""
        }
    }

    /// Whether a slide is hidden (`<p:sld show="0">`).
    static func pptxHidden(_ slide: String) -> Bool {
        var hidden = false
        XML.walk(slide, start: { name, a in
            guard name == "p:sld" else { return true }
            hidden = a["show"] == "0" || a["show"] == "false"
            return false
        })
        return hidden
    }

    /// The text of a notes slide's body placeholder: the notes, not the slide picture or the page number.
    public static func pptxNotesText(_ xml: String) -> String {
        var shape = 0, body = false, inT = false
        var lines: [String] = [], para = ""
        XML.walk(xml, start: { name, a in
            switch name {
            case "p:sp":
                shape += 1
                body = false
            case "a:t": inT = true
            case "a:p": para = ""
            case "p:ph": body = a["type"] == "body"
            case "a:br" where body && shape > 0: para.append("\n")
            default: break
            }
            return true
        }, end: { name in
            switch name {
            case "p:sp":
                shape = max(0, shape - 1)
                body = false
            case "a:t": inT = false
            case "a:p" where body && shape > 0:
                lines.append(para)
                para = ""
            default: break
            }
        }, text: { t in
            if inT && body && shape > 0 { para.append(t) }
        })
        return tidy(lines)
    }

    // MARK: - PDF

    /// pdfpc's file (`talk.pdfpc` next to `talk.pdf`): its JSON form (`pages[].idx`, `note`) or the older text form
    /// (`[notes]`, `### <page>` headings).
    public static func pdfpc(_ text: String) -> [String]? {
        var notes: [String] = []
        func put(_ page: Int, _ note: String) {
            guard page >= 0, page < 10_000 else { return }
            if notes.count <= page { notes += Array(repeating: "", count: page + 1 - notes.count) }
            notes[page] = note.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        if let v = try? JSONSerialization.jsonObject(with: Data(text.utf8)) {
            guard let pages = (v as? [String: Any])?["pages"] as? [[String: Any]] else { return nil }
            for p in pages {
                if let i = (p["idx"] as? NSNumber)?.intValue, let n = p["note"] as? String { put(i, n) }
            }
            return notes
        }
        guard let start = text.range(of: "[notes]") else { return nil }
        var body = String(text[start.upperBound...])
        if let end = body.range(of: "\n[") { body = String(body[..<end.lowerBound]) }
        for block in body.components(separatedBy: "### ").dropFirst() {
            let (head, note) = block.firstIndex(of: "\n").map { (String(block[..<$0]), String(block[block.index(after: $0)...])) }
                ?? (block, "")
            if let n = Int(head.trimmingCharacters(in: .whitespaces)) { put(max(n - 1, 0), note) }
        }
        return notes
    }

    /// The text annotations ("sticky notes") of each page, joined. A locked PDF gives none.
    public static func pdfNotes(_ pdf: URL) -> [String] {
        guard let doc = PDFDocument(url: pdf), !doc.isLocked else { return [] }
        return (0..<doc.pageCount).map { i in
            (doc.page(at: i)?.annotations ?? [])
                .filter { $0.type == "Text" }
                .compactMap { $0.contents?.trimmingCharacters(in: .whitespacesAndNewlines) }
                .filter { !$0.isEmpty }
                .joined(separator: "\n\n")
        }
    }
}

/// XMLParser with closures: qualified names ("draw:page") and attributes as written, as luchd's quick-xml sees them.
enum XML {
    /// `start` returns false to stop reading.
    static func walk(_ xml: String, start: @escaping (String, [String: String]) -> Bool,
                     end: @escaping (String) -> Void = { _ in }, text: @escaping (String) -> Void = { _ in }) {
        let d = Delegate(start: start, end: end, text: text)
        let p = XMLParser(data: Data(xml.utf8))
        p.shouldProcessNamespaces = false
        p.shouldResolveExternalEntities = false
        p.delegate = d
        p.parse()
    }

    private final class Delegate: NSObject, XMLParserDelegate {
        let start: (String, [String: String]) -> Bool
        let end: (String) -> Void
        let text: (String) -> Void

        init(start: @escaping (String, [String: String]) -> Bool, end: @escaping (String) -> Void,
             text: @escaping (String) -> Void) {
            self.start = start
            self.end = end
            self.text = text
        }

        func parser(_ parser: XMLParser, didStartElement name: String, namespaceURI: String?, qualifiedName: String?,
                    attributes: [String: String] = [:]) {
            if !start(name, attributes) { parser.abortParsing() }
        }

        func parser(_ parser: XMLParser, didEndElement name: String, namespaceURI: String?, qualifiedName: String?) {
            end(name)
        }

        func parser(_ parser: XMLParser, foundCharacters string: String) {
            text(string)
        }
    }
}
