import AgentCore
import Foundation

@main
struct MarkdownTests {
    static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        guard condition() else {
            fputs("FAIL: \(message)\n", stderr)
            exit(1)
        }
    }

    static func text(_ blocks: [ConversationMarkdownContent.Block]) -> String {
        blocks.flatMap(\.runs).map(\.text).joined()
    }

    static func main() throws {
        let table = try String(contentsOfFile: CommandLine.arguments[1], encoding: .utf8)
        let visual = ConversationMarkdownContent
            .parse("Before\n\nvisualize{\"path\":\"/fixture/options.html\"}\n\nAfter")
        check(
            visual.count == 3 && visual[1].visualizationPath == "/fixture/options.html",
            "visualize must reach its native view in document order"
        )
        check(text(visual[0].blocks) == "Before" && text(visual[2].blocks) == "After", "surrounding Markdown survives")
        reportedTable(table)
        formattedCells()
        surroundingContent(table)
        streaming(table)
        try sharedDocument(String(contentsOfFile: CommandLine.arguments[2], encoding: .utf8))
        print("Markdown regressions passed: cells, boundaries, alignment, formatting, images, links, streaming")
    }

    private static func reportedTable(_ table: String) {
        let parts = ConversationMarkdownContent.parse("Before\n\n" + table + "\nAfter")
        check(parts.count == 3, "table must be isolated from surrounding prose; got \(parts.count) part(s)")
        check(text(parts[0].blocks) == "Before" && text(parts[2].blocks) == "After", "surrounding prose")
        let rows = parts[1].tableRows.map { $0.map(text) }
        check(rows == [
            ["構成", "利点", "負担"],
            ["Codexハーネス＋Claude接続", "ツール実行・承認・履歴・委譲を一本化できる",
             "通信変換、モデルの挙動、サブスク認証との適合を検証する必要"],
            ["Codex／Claude Codeを並列接続", "それぞれの標準機能・認証を使える", "両者の機能差をBexが吸収する必要"]
        ], "reported Japanese table must retain every cell in its row and column")
    }

    private static func formattedCells() {
        let formatted = ConversationMarkdownContent.parse("""
        | Left | Center | Right |
        |:---|:---:|---:|
        | **bold** and [link](https://example.com) | | `code` |
        | | | |
        | last | escaped \\| pipe | |
        """)[0]
        check(formatted.tableRows[0].map { $0[0].style.alignment } == [.left, .center, .right], "column alignment")
        check(
            formatted.tableRows[0].flatMap(\.self).flatMap(\.runs).allSatisfy(\.strong),
            "shared table header emphasis"
        )
        check(formatted.tableRows.map { $0.map(text) } == [
            ["Left", "Center", "Right"], ["bold and link", "", "code"], ["", "", ""],
            ["last", "escaped | pipe", ""]
        ], "empty cells and empty interior rows must not shift columns")
        let runs = formatted.tableRows[1][0][0].runs
        check(runs.contains { $0.strong }, "bold")
        check(runs.contains { $0.link == "https://example.com" }, "link destination")
        check(formatted.tableRows[1][2][0].runs.first?.code == true,
              "inline code")
    }

    private static func surroundingContent(_ table: String) {
        let mixed = ConversationMarkdownContent.parse("# Heading\n\n- item\n\n```text\n" + table
            + "```\n\n![image](https://example.com/image.png)\n\n" + table + "\n" + table)
        check(mixed.filter { !$0.tableRows.isEmpty }.count == 2, "separate adjacent tables")
        check(mixed.contains { $0.isCode && text($0.blocks).contains("|---|---|---|") }, "fenced table stays code")
        check(mixed.contains { $0.image?.imageURL?.absoluteString == "https://example.com/image.png" }, "image")
        check(mixed[0].blocks[0].style.header == 1 && mixed[0].blocks[1].style.marker == "•", "heading and list")
        let references = ConversationMarkdownContent.parse("[before][ref]\n\n" + table
            + "\n[after][ref]\n\n[ref]: https://example.com")
        check(references[0].blocks[0].runs.first?.link == "https://example.com",
              "reference links before a table must keep whole-document context")
        check(references[2].blocks[0].runs.first?.link == "https://example.com",
              "reference links after a table")
    }

    private static func sharedDocument(_ source: String) {
        let parts = ConversationMarkdownContent.parse(source)
        let blocks = parts.flatMap(\.blocks)
        check(blocks.count == 10, "shared document paragraph count")
        check(blocks[0].style.header == 1, "shared heading")
        check(blocks[2].style.quoted, "shared quote")
        check(blocks[3 ... 6].map(\.style.marker) == ["3.", "4.", "☑", "☐"], "shared list markers")
        check(blocks[7].style.code && blocks[7].imageURL == nil, "code is not an image")
        check(blocks[8].imageURL?.relativeString == "images/example.png", "reference image")
        check(blocks[1].runs.contains {
            $0.strong && $0.emphasis
        }, "nested emphasis")
        for index in [1, 9] {
            check(blocks[index].runs.contains {
                $0.link == "https://example.com/reference"
            }, "whole document reference resolution")
        }
    }

    private static func streaming(_ table: String) {
        for end in table.indices {
            let partial = ConversationMarkdownContent.parse(String(table[..<end]))
            for part in partial where !part.tableRows.isEmpty {
                check(part.tableRows.allSatisfy { $0.count == part.tableRows[0].count }, "partial stream dimensions")
            }
        }
        check(ConversationMarkdownContent.parse("No table | here")[0].tableRows.isEmpty, "ordinary pipe")
    }
}
