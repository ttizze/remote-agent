import UIKit

@main
struct MessageLayoutTests {
    static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        guard condition() else {
            fputs("FAIL: \(message)\n", stderr)
            exit(1)
        }
    }

    static func document(width: CGFloat, repetitions: Int = 8) -> MessageDocument {
        let body = String(repeating: "日本語の長い本文は、幅に合わせて折り返し、次の段落と重ならず表示される必要があります。", count: repetitions)
        let blocks = [
            MessageParagraph(header: 2, marker: nil, code: false, quoted: false, depth: 0, alignment: "left",
                             runs: [run("入力行を消した後の再送防止")]),
            MessageParagraph(header: nil, marker: nil, code: false, quoted: false, depth: 0, alignment: "left",
                             runs: [run(body)]),
            MessageParagraph(header: nil, marker: "•", code: false, quoted: false, depth: 1, alignment: "left",
                             runs: [run(body)]),
            MessageParagraph(header: nil, marker: nil, code: false, quoted: false, depth: 0, alignment: "left",
                             runs: [run("最後の段落まで読めます。")])
        ]
        return MessageDocument(blocks: blocks, secondary: false, role: "assistant", target: "fixture",
                               sideChat: false, maxWidth: width, menu: MessageMenuTitles(quote: "", sideChat: ""))
    }

    static func run(_ text: String) -> MessageRun {
        MessageRun(text: text, strong: false, emphasis: false, strikethrough: false, code: false,
                   link: nil, darkColor: nil, lightColor: nil)
    }

    @MainActor static func referenceHeight(_ content: NSAttributedString, width: CGFloat) -> CGFloat {
        let reference = UITextView()
        reference.isScrollEnabled = false
        reference.textContainerInset = .zero
        reference.textContainer.lineFragmentPadding = 0
        reference.attributedText = content
        return ceil(reference.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height)
    }

    @MainActor static func main() {
        let view = MessageTextContainer()
        var measurements: [CGSize] = []
        view.send = { event in
            if let width = event["measuredWidth"] as? CGFloat, let height = event["measuredHeight"] as? CGFloat {
                measurements.append(CGSize(width: width, height: height))
            }
        }
        let wide = document(width: 360)
        view.update(wide)
        let expected = referenceHeight(view.text.attributedText, width: 360)
        check(measurements.count == 1 && measurements[0].height == expected,
              "The first reported height must fit every wrapped paragraph before NSH assigns a frame")
        check(measurements[0].width > 360, "Natural width must remain available for horizontally scrolled code")

        // A recycled row still has its previous message's native frame.
        view.frame = CGRect(x: 0, y: 0, width: 90, height: 20)
        view.layoutIfNeeded()
        view.text.layoutIfNeeded()
        check(measurements.count == 1, "A stale native frame must not publish another message's width or height")
        check(view.text.frame.height == expected, "The drawn text must use the height reported to the list")

        view.frame.size = CGSize(width: 360, height: expected)
        view.layoutIfNeeded()
        view.text.layoutIfNeeded()
        check(measurements.count == 1, "Assigning the measured frame must keep the row height stable")
        check(view.text.contentOffset == .zero, "The message must not scroll inside its conversation row")

        let selection = (view.text.text as NSString).range(of: "日本語の長い本文")
        view.text.selectedRange = selection
        view.update(document(width: 360, repetitions: 10))
        check(view.text.selectedRange == selection, "Streaming must preserve unchanged selected text")
        let streamed = referenceHeight(view.text.attributedText, width: 360)
        check(measurements.last?.height == streamed && streamed > expected,
              "Streaming must immediately publish the new wrapped height")

        view.update(document(width: 180, repetitions: 10))
        let narrow = referenceHeight(view.text.attributedText, width: 180)
        check(measurements.last?.height == narrow && narrow > streamed,
              "A changed lane width must be measured before the old frame is replaced")
        view.frame.size = CGSize(width: 180, height: narrow)
        view.layoutIfNeeded()
        view.text.layoutIfNeeded()
        check(measurements.last?.height == narrow && view.text.frame.height == narrow,
              "A narrower message must keep its reported and drawn heights in agreement")

        view.update(document(width: 360, repetitions: 1))
        let short = referenceHeight(view.text.attributedText, width: 360)
        check(measurements.last?.height == short && short < expected,
              "Reusing a tall row for a shorter message must discard the old height")
        checkShortMessage()
        checkDecodeFailureRecovery()
        print("NSH message layout tests passed")
    }

    @MainActor static func checkShortMessage() {
        let view = MessageTextContainer()
        var measured = CGSize.zero
        view.send = { event in
            if let width = event["measuredWidth"] as? CGFloat, let height = event["measuredHeight"] as? CGFloat {
                measured = CGSize(width: width, height: height)
            }
        }
        let short = MessageDocument(
            blocks: [MessageParagraph(header: nil, marker: nil, code: false, quoted: false, depth: 0,
                                      alignment: "left", runs: [run("短い本文です。")])],
            secondary: false, role: "user", target: "short", sideChat: false, maxWidth: 360,
            menu: MessageMenuTitles(quote: "", sideChat: "")
        )
        view.update(short)
        let naturalWidth = ceil(view.text.attributedText.size().width)
        check(naturalWidth > 0 && naturalWidth < 360 && measured.width == naturalWidth,
              "A short bubble must keep its natural width rather than fill the lane")
        let height = referenceHeight(view.text.attributedText, width: naturalWidth)
        check(measured.height == height, "A short bubble must fit its text at its natural width")
        view.frame.size = CGSize(width: naturalWidth, height: height)
        view.layoutIfNeeded()
        view.text.layoutIfNeeded()
        check(view.text.frame.size == CGSize(width: naturalWidth, height: height),
              "A short bubble must draw at the size it reports")
    }

    @MainActor static func checkDecodeFailureRecovery() {
        let owner = MessageTextOwner(context: nil, callback: { _, _, _ in })
        var measured = CGSize.zero
        owner.view.send = { event in
            if let width = event["measuredWidth"] as? CGFloat, let height = event["measuredHeight"] as? CGFloat {
                measured = CGSize(width: width, height: height)
            }
        }
        owner.update(Data("{}".utf8))
        check(measured.width > 0 && measured.height > 0 && !owner.view.text.text.isEmpty,
              "A first-update decode failure must report a visible diagnostic")
        owner.view.frame.size = measured
        owner.view.layoutIfNeeded()
        check(owner.view.text.frame.height == measured.height, "The diagnostic must receive its measured height")

        let valid = Data("""
        {"blocks":[],"secondary":false,"role":"assistant","target":"fixture","sideChat":false,
         "maxWidth":360,"menu":{"quote":"","sideChat":""}}
        """.utf8)
        owner.update(valid)
        check(owner.view.text.text.isEmpty, "Valid content must replace a decode failure")
        owner.update(Data("{}".utf8))
        check(measured.height > 0, "A later decode failure must replace the previous measurement")
        owner.update(valid)
        check(owner.view.text.text.isEmpty, "Replaying the same valid document must recover after a decode failure")
    }
}
