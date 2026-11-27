// Rows printed as a table: aligned columns under a header on a terminal, tab-separated without one (a script reading
// luchctl's output gets the same lines as before, and as the Linux luchctl's).
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin

public enum Table {
    public static var toTerminal: Bool { isatty(STDOUT_FILENO) == 1 }

    public static func print(header: [String], rows: [[String]], terminal: Bool = Table.toTerminal) {
        for line in lines(header: header, rows: rows, terminal: terminal) { Swift.print(line) }
    }

    public static func lines(header: [String], rows: [[String]], terminal: Bool) -> [String] {
        guard terminal else { return rows.map { $0.joined(separator: "\t") } }
        let all = [header] + rows
        let columns = all.map(\.count).max() ?? 0
        let widths = (0..<columns).map { c in all.map { c < $0.count ? $0[c].count : 0 }.max() ?? 0 }
        func line(_ cells: [String]) -> String {
            cells.enumerated().map { c, cell in
                c == cells.count - 1 ? cell : cell + String(repeating: " ", count: widths[c] - cell.count)
            }.joined(separator: "  ")
        }
        let bold = "\u{1B}[1m", plain = "\u{1B}[0m"
        return [bold + line(header) + plain] + rows.map(line)
    }
}
