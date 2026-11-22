import SwiftUI

/// `architecture_describe` as cards, one group for each part of the answer.
struct ArchitectureView: View {
    let session: Session
    @State private var model = ToolModel("architecture_describe")

    var body: some View {
        Screen(errors: [model.error].compactMap { $0 }, refresh: { await model.load(session.client) }) {
            Eyebrow(text: "Architecture")
            if model.data != nil {
                JSONCards(value: model.data)
            } else if model.error == nil {
                Card { Text("Loading…") }
            }
        }
        .navigationTitle("Architecture")
        .navigationBarTitleDisplayMode(.inline)
        .task { await model.load(session.client) }
    }
}

/// `models_usage` for the last week: totals, then a card for each group.
struct CostView: View {
    let session: Session
    @State private var model = ToolModel("models_usage", ["hours": 168])

    var body: some View {
        Screen(errors: [model.error].compactMap { $0 }, refresh: { await model.load(session.client) }) {
            Eyebrow(text: "Cost, last 7 days")
            if let dict = model.data as? [String: Any] {
                Card {
                    CardTitle(text: "Total")
                    Text("Requests: \(jsonText(dict["requests"]))")
                    Text("Tokens in: \(jsonText(dict["tokens_in"]))")
                    Text("Tokens out: \(jsonText(dict["tokens_out"]))")
                    Text("Cost: \(jsonText(dict["cost"]))")
                }
                Eyebrow(text: "By model")
                JSONCards(value: dict["groups"])
            } else if model.data != nil {
                ErrorBanner(text: "models_usage failed: the answer was not an object.")
            } else if model.error == nil {
                Card { Text("Loading…") }
            }
        }
        .navigationTitle("Cost")
        .navigationBarTitleDisplayMode(.inline)
        .task { await model.load(session.client) }
    }
}
