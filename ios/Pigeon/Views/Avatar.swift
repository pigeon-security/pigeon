import SwiftUI

/// A circle with a person's initials, or their picture when a URL is given.
struct Avatar: View {
    let name: String
    var initials: String?
    var picture: String?
    var size: CGFloat = 44

    var body: some View {
        ZStack {
            Circle().fill(Tokens.accentSoft)
            Text(initials ?? initialsOf(name))
                .font(.system(size: size * 0.38, weight: .semibold))
                .foregroundStyle(Tokens.accent)
            if let picture, let url = URL(string: picture) {
                AsyncImage(url: url) { image in
                    image.resizable().scaledToFill()
                } placeholder: {
                    Color.clear
                }
                .clipShape(Circle())
            }
        }
        .frame(width: size, height: size)
    }
}
