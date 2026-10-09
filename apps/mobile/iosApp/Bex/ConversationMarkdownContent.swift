import AgentCore
import Foundation

enum ConversationMarkdownContent {
    struct Block: Identifiable, Sendable, Equatable {
        let id: Int
        let runs: [MarkdownRun]
        let style: MarkdownStyle
        let imageURL: URL?
    }

    struct Part: Identifiable, Sendable {
        let id: Int
        let blocks: [Block]
        let tableRows: [[[Block]]]
        var visualizationPath: String?
        var tableMarkdown: String?
        var tableCSV: String?

        var image: Block? {
            blocks.first.flatMap { $0.imageURL == nil ? nil : $0 }
        }

        var isCode: Bool {
            blocks.first?.style.code == true
        }
    }

    nonisolated static func parse(_ text: String) -> [Part] {
        parts(markdownBlocks(source: text))
    }

    nonisolated static func parts(_ sourceBlocks: [MarkdownBlock]) -> [Part] {
        var result: [Part] = []
        var paragraphs: [Block] = []
        for block in sourceBlocks {
            switch block {
            case let .paragraph(runs, style):
                paragraphs.append(contentsOf: blocks(runs, style: style, startingID: paragraphs.count))
            case let .visualization(path):
                appendParts(paragraphs, to: &result)
                paragraphs.removeAll(keepingCapacity: true)
                result.append(Part(id: result.count, blocks: [], tableRows: [], visualizationPath: path))
            case let .table(source, columns, rows):
                appendParts(paragraphs, to: &result)
                paragraphs.removeAll(keepingCapacity: true)
                let cells = rows.map { cells in
                    cells.enumerated().map { column, cell in
                        blocks(cell.runs, style: MarkdownStyle(alignment: columns[column],
                                                               header: nil, marker: nil, code: false, quoted: false,
                                                               listDepth: 0, language: nil, filename: nil, rule: false))
                    }
                }
                result.append(Part(id: result.count, blocks: [], tableRows: cells,
                                   tableMarkdown: source,
                                   tableCSV: markdownTableCsv(rows: rows
                                       .map { $0.map { $0.runs.map(\.text).joined() } })))
            }
        }
        appendParts(paragraphs, to: &result)
        return result
    }

    private nonisolated static func blocks(_ runs: [MarkdownRun], style: MarkdownStyle,
                                           startingID: Int = 0) -> [Block] {
        var result: [Block] = []
        var content: [MarkdownRun] = []
        var imageURL: URL?
        for run in runs {
            let nextImage = run.image.flatMap(URL.init(string:))
            if nextImage != imageURL, !content.isEmpty || imageURL != nil {
                result.append(Block(id: startingID + result.count, runs: content, style: style, imageURL: imageURL))
                content.removeAll(keepingCapacity: true)
            }
            imageURL = nextImage
            content.append(run)
        }
        // Empty cells still participate in table layout.
        result.append(Block(id: startingID + result.count, runs: content, style: style, imageURL: imageURL))
        return result
    }

    private nonisolated static func appendParts(_ blocks: [Block], to parts: inout [Part]) {
        var paragraphs: [Block] = []
        for block in blocks {
            if block.imageURL != nil || block.style.code {
                if !paragraphs.isEmpty {
                    parts.append(Part(id: parts.count, blocks: paragraphs, tableRows: []))
                    paragraphs.removeAll(keepingCapacity: true)
                }
                parts.append(Part(id: parts.count, blocks: [block], tableRows: []))
            } else {
                paragraphs.append(block)
            }
        }
        if !paragraphs.isEmpty {
            parts.append(Part(id: parts.count, blocks: paragraphs, tableRows: []))
        }
    }
}
