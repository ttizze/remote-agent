import SwiftUI
import UIKit

struct ProjectIcon: View {
    let png: Data?
    let monogram: String
    let colorRGB: UInt32

    var body: some View {
        let color = Color(
            red: Double((colorRGB >> 16) & 255) / 255,
            green: Double((colorRGB >> 8) & 255) / 255,
            blue: Double(colorRGB & 255) / 255
        )
        Group {
            if let png, let image = UIImage(data: png) {
                Image(uiImage: image).resizable().scaledToFit()
            } else {
                Text(monogram)
                    .font(.system(size: 10, weight: .bold, design: .rounded))
                    .foregroundStyle(color)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(color.opacity(0.15))
            }
        }
        .frame(width: 24, height: 24)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .accessibilityHidden(true)
    }
}
