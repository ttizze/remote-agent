import AgentCore
import UIKit

/// The upstream mobile catalog uses untinted Pierre PNG glyphs, including for chips.
func mobileFileImage(_ path: String) -> UIImage {
    let token = mobileFileIcon(path: path)
    let url = Bundle.main.url(forResource: "pierre_\(token)", withExtension: "png", subdirectory: "file-icons")!
    return UIImage(contentsOfFile: url.path)!
}
