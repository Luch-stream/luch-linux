// Speaker notes: the fixtures of luchd's notes.rs, so both senders read the same notes from the same files.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import PDFKit
import XCTest
@testable import LuchPresent

final class NotesTests: XCTestCase {
    private func dir() -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("luch-notes-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    /// A zip of `files` made by the system's zip (deflated, or stored with `store`).
    private func zip(_ out: URL, _ files: [(String, String)], store: Bool = false) throws {
        let src = dir()
        for (name, body) in files {
            let u = src.appendingPathComponent(name)
            try FileManager.default.createDirectory(at: u.deletingLastPathComponent(), withIntermediateDirectories: true)
            try Data(body.utf8).write(to: u)
        }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/zip")
        p.currentDirectoryURL = src
        p.arguments = ["-q", "-X", store ? "-0" : "-9", out.path] + files.map(\.0)
        try p.run()
        p.waitUntilExit()
        XCTAssertEqual(p.terminationStatus, 0)
    }

    static let odp = """
    <?xml version="1.0"?>
    <office:document-content xmlns:office="o" xmlns:style="s" xmlns:draw="d" xmlns:presentation="p" xmlns:text="t">
     <office:automatic-styles>
      <style:style style:name="dp1" style:family="drawing-page"><style:drawing-page-properties presentation:display-footer="true"/></style:style>
      <style:style style:name="dp2" style:family="drawing-page"><style:drawing-page-properties presentation:visibility="hidden"/></style:style>
     </office:automatic-styles>
     <office:body><office:presentation>
      <draw:page draw:name="One" draw:style-name="dp1">
       <draw:frame presentation:class="title"><draw:text-box><text:p>Not a note</text:p></draw:text-box></draw:frame>
       <presentation:notes>
        <draw:page-thumbnail/>
        <draw:frame presentation:class="notes"><draw:text-box>
          <text:p>Say hello &amp; smile</text:p><text:p>Then<text:s text:c="2"/>the plan<text:line-break/>in two lines</text:p>
        </draw:text-box></draw:frame>
       </presentation:notes>
      </draw:page>
      <draw:page draw:name="Hidden" draw:style-name="dp2">
       <presentation:notes><draw:frame presentation:class="notes"><draw:text-box><text:p>secret</text:p></draw:text-box></draw:frame></presentation:notes>
      </draw:page>
      <draw:page draw:name="Three" draw:style-name="dp1"/>
      <draw:page draw:name="Four" draw:style-name="dp1">
       <presentation:notes><draw:frame presentation:class="notes"><draw:text-box><text:p>Questions?</text:p></draw:text-box></draw:frame></presentation:notes>
      </draw:page>
     </office:presentation></office:body>
    </office:document-content>
    """

    func testOdpNotesFollowTheShownSlides() throws {
        XCTAssertEqual(Notes.odpContent(Self.odp), ["Say hello & smile\nThen  the plan\nin two lines", "", "Questions?"])
        let f = dir().appendingPathComponent("talk.odp")
        try zip(f, [("mimetype", "application/vnd.oasis.opendocument.presentation"), ("content.xml", Self.odp)])
        XCTAssertEqual(Notes.speakerNotes(original: f, pdf: f.deletingPathExtension().appendingPathExtension("pdf"))[2],
                       "Questions?")
    }

    private func notesSlide(_ text: [String]) -> String {
        let paras = text.map { "<a:p><a:r><a:t>\($0)</a:t></a:r></a:p>" }.joined()
        return """
        <p:notes xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>
        <p:sp><p:nvSpPr><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp>
        <p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes"/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr>
        <p:txBody>\(paras)</p:txBody></p:sp>
        <p:sp><p:nvSpPr><p:nvPr><p:ph type="sldNum" idx="5"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>7</a:t></a:r></a:p></p:txBody></p:sp>
        </p:spTree></p:cSld></p:notes>
        """
    }

    func testPptxNotesInSlideOrderWithoutHiddenSlides() throws {
        let rel = { (id: String, kind: String, target: String) in
            "<Relationship Id=\"\(id)\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/\(kind)\" Target=\"\(target)\"/>"
        }
        let rels = { (body: String) in "<Relationships xmlns=\"r\">\(body)</Relationships>" }
        let pres = """
        <p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst>
        <p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/><p:sldId id="258" r:id="rId4"/></p:sldIdLst></p:presentation>
        """
        let presRels = rels(rel("rId2", "slide", "slides/slide1.xml") + rel("rId3", "slide", "slides/slide2.xml")
            + rel("rId4", "slide", "/ppt/slides/slide3.xml"))
        let slide = { (show: String) in "<p:sld xmlns:p=\"p\"\(show)><p:cSld/></p:sld>" }
        let slideRels = { (n: Int) in
            rels(rel("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml")
                + rel("rId2", "notesSlide", "../notesSlides/notesSlide\(n).xml"))
        }
        let files: [(String, String)] = [
            ("ppt/presentation.xml", pres),
            ("ppt/_rels/presentation.xml.rels", presRels),
            ("ppt/slides/slide1.xml", slide("")),
            ("ppt/slides/slide2.xml", slide("")),
            ("ppt/slides/slide3.xml", slide(" show=\"0\"")),
            ("ppt/slides/_rels/slide1.xml.rels", slideRels(1)),
            ("ppt/slides/_rels/slide2.xml.rels", slideRels(2)),
            ("ppt/slides/_rels/slide3.xml.rels", slideRels(1)),
            ("ppt/notesSlides/notesSlide1.xml", notesSlide(["First &lt;b&gt;", "line two"])),
            ("ppt/notesSlides/notesSlide2.xml", notesSlide(["Opening"])),
        ]
        // Deflated as PowerPoint writes them, and stored: both are read.
        for store in [false, true] {
            let f = dir().appendingPathComponent("deck.pptx")
            try zip(f, files, store: store)
            // The order of sldIdLst, not of the file names; the page number placeholder is not a note.
            XCTAssertEqual(Notes.speakerNotes(original: f, pdf: f.deletingPathExtension().appendingPathExtension("pdf")),
                           ["Opening", "First <b>\nline two"])
        }
    }

    func testPdfpcFilesInBothForms() throws {
        let json = #"{"pdfpcFormat":2,"pages":[{"idx":0,"label":"1","note":"Hi"},{"idx":2,"label":"2","note":" Demo "}]}"#
        XCTAssertEqual(Notes.pdfpc(json), ["Hi", "", "Demo"])
        let old = "[file]\ntalk.pdf\n[notes]\n### 1\nfirst\nmore\n### 3\nthird\n[end]\n"
        XCTAssertEqual(Notes.pdfpc(old), ["first\nmore", "", "third"])
        XCTAssertNil(Notes.pdfpc("nothing here"))
        let pdf = dir().appendingPathComponent("beamer.pdf")
        try Data("%PDF-1.4\n".utf8).write(to: pdf)
        try Data(json.utf8).write(to: pdf.deletingPathExtension().appendingPathExtension("pdfpc"))
        XCTAssertEqual(Notes.speakerNotes(original: pdf, pdf: pdf)[2], "Demo")
    }

    /// luchd's hand-written PDF: two pages, a sticky note on the second (in UTF-16), a link that is no note. With the
    /// /Rect every annotation must have: PDFKit drops one without it, luchd's own reader does not look.
    static func plainPDF() -> Data {
        var s = "%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n"
        s += "2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >> endobj\n"
        s += "3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >> endobj\n"
        s += "4 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots 7 0 R >> endobj\n"
        s += "5 0 obj << /Type /Annot /Subtype /Link /Rect [0 0 9 9] /Contents (not a note) >> endobj\n"
        s += "6 0 obj << /Type /Annot /Subtype /Text /Rect [0 0 9 9] /Contents <FEFF0041006E0064002004110435> >> endobj\n"
        s += "7 0 obj [6 0 R 8 0 R] endobj\n"
        s += "8 0 obj << /Subtype /Text /Rect [0 0 9 9] /Contents (Second \\(note\\)\\nline\\101) >> endobj\n"
        s += "trailer << /Root 1 0 R >>\n%%EOF\n"
        return Data(s.utf8)
    }

    func testPdfTextAnnotationsAreNotes() throws {
        let f = dir().appendingPathComponent("plain.pdf")
        try Self.plainPDF().write(to: f)
        XCTAssertEqual(Notes.speakerNotes(original: f, pdf: f), ["", "And Бе\n\nSecond (note)\nlineA"])

        // As a PDF writer of today saves them (PDFKit: object streams, a cross-reference stream).
        let doc = PDFDocument()
        for i in 0..<3 {
            let page = PDFPage()
            if i == 1 {
                let note = PDFAnnotation(bounds: CGRect(x: 10, y: 10, width: 20, height: 20), forType: .text, withProperties: nil)
                note.contents = "Slow down here"
                page.addAnnotation(note)
                let link = PDFAnnotation(bounds: CGRect(x: 40, y: 10, width: 20, height: 20), forType: .link, withProperties: nil)
                link.contents = "not a note"
                page.addAnnotation(link)
            }
            doc.insert(page, at: i)
        }
        let saved = dir().appendingPathComponent("saved.pdf")
        XCTAssertTrue(doc.write(to: saved))
        XCTAssertEqual(Notes.pdfNotes(saved), ["", "Slow down here", ""])
        XCTAssertEqual(Notes.pdfNotes(dir().appendingPathComponent("missing.pdf")), [])
    }

    func testPartsResolveRelativeToTheirFolder() {
        XCTAssertEqual(Notes.resolvePart("ppt/slides/slide1.xml", "../notesSlides/n1.xml"), "ppt/notesSlides/n1.xml")
        XCTAssertEqual(Notes.resolvePart("ppt/presentation.xml", "slides/slide1.xml"), "ppt/slides/slide1.xml")
        XCTAssertEqual(Notes.resolvePart("ppt/presentation.xml", "/ppt/slides/s.xml"), "ppt/slides/s.xml")
    }

    func testABrokenArchiveGivesNoNotes() throws {
        let f = dir().appendingPathComponent("broken.pptx")
        try Data("PK not really".utf8).write(to: f)
        XCTAssertNil(ZipArchive(url: f).flatMap(Notes.pptxNotes))
        XCTAssertEqual(Notes.speakerNotes(original: f, pdf: f.deletingPathExtension().appendingPathExtension("pdf")), [])
    }
}
