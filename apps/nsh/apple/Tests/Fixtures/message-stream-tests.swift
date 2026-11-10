import UIKit

@main
struct MessageStreamTests {
    static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        guard condition() else {
            fputs("FAIL: \(message)\n", stderr)
            exit(1)
        }
    }

    static func document(_ text: String, width: Double = 320, fillsWidth: Bool = true) -> MessageDocument {
        MessageDocument(blocks: [
            MessageParagraph(header: nil, marker: nil, code: false, quoted: false, depth: 0,
                             alignment: "left", runs: [
                                 MessageRun(text: text, strong: false, emphasis: false, strikethrough: false,
                                            code: false, link: nil, darkColor: nil, lightColor: nil)
                             ])
        ], secondary: false, role: "assistant", target: "stream", sideChat: false, maxWidth: width,
        menu: MessageMenuTitles(quote: "", sideChat: ""), fillsWidth: fillsWidth)
    }

    @MainActor static func main() {
        let view = MessageTextContainer()
        var sizes: [CGSize] = []
        view.send = {
            if let width = $0["measuredWidth"] as? CGFloat, let height = $0["measuredHeight"] as? CGFloat {
                sizes.append(CGSize(width: width, height: height))
            }
        }
        // A recycled view still has its previous row's frame when new text arrives.
        view.frame = CGRect(x: 0, y: 0, width: 70, height: 30)
        let paragraph = String(repeating: "Wrapped 日本語 👋 message. ", count: 40)
        view.update(document(paragraph))
        check(sizes.count == 1 && sizes[0].height > 200 && sizes[0].width <= 320,
              "The first size must already include wrapping at the new conversation's width")
        let wrapped = sizes[0]
        view.layoutSubviews()
        check(view.text.frame.size == wrapped, "The displayed text must use its published size, not the stale frame")
        check(sizes == [wrapped], "The stale native frame must not publish a second provisional height")
        view.frame.size = wrapped
        view.layoutSubviews()
        check(sizes == [wrapped], "Applying the measured frame must not change the row's height")

        sizes.removeAll()
        view.update(document(paragraph + String(repeating: "\nMore streamed text.", count: 10)))
        check(sizes.count == 1 && sizes[0].height > wrapped.height, "Streaming must publish one final, grown size")
        view.layoutSubviews()
        check(view.text.frame.size == sizes[0], "The streamed text's frame must match its published wrapped height")
        sizes.removeAll()
        view.update(document(paragraph, width: 120))
        check(sizes.count == 1 && sizes[0].height > wrapped.height,
              "A changed width must reflow before its first size is published")
        let narrow = sizes
        view.layoutSubviews()
        check(view.text.frame.size == narrow[0], "The new width must apply while the outer frame is still stale")
        check(sizes == narrow, "Reflow must not use the previous conversation's wider frame")
        sizes.removeAll()
        view.update(document("Short text", fillsWidth: false))
        check(sizes.count == 1 && sizes[0].width > 0 && sizes[0].width < 320,
              "Shrink-to-fit bubbles and table cells must retain their natural width")
        print("NSH message sizing tests passed")
    }
}
