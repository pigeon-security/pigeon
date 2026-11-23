import SwiftUI

/// A chip-coloured circle with a person's initials, or their picture when a URL is given.
struct Avatar: View {
    let name: String
    var initials: String?
    var picture: String?
    var size: CGFloat = 44

    /// The initials font: the shared 28 pt one at size 76, else 0.37 of the size.
    private var initialsFont: Font {
        size == 76 ? AppFont.avatarInitials : .system(size: size * 0.37, weight: .semibold)
    }

    var body: some View {
        ZStack {
            Circle().fill(Tokens.chip)
            Text(initials ?? initialsOf(name))
                .font(initialsFont)
                .foregroundStyle(Tokens.ink)
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
