// The charts: BarChart, LineChart and PieChart, drawn with Swift Charts. Axis labels come from the block; a
// legend is shown when there is more than one series, and always for a pie, whose slices have no axis to name them.
// Behaviour: docs/app-chat.md, "The card engine".
import SwiftUI
import Charts
import JSONRenderCore

/// One value of one series at one label.
struct ChartPoint: Identifiable, Equatable {
    let id: Int
    let category: String
    let series: String
    let value: Double
}

/// A chart's labels and series as points: one per label per named series, 0 where a value is missing or not a
/// number (the web page's chartRows).
func chartPoints(labels: [JSONValue], series: [JSONValue]) -> (points: [ChartPoint], series: [String]) {
    var names: [String] = []
    var named: [(name: String, values: [JSONValue])] = []
    for one in series {
        let name = CardElement.text(one["name"])
        guard !name.isEmpty else { continue }
        if !names.contains(name) { names.append(name) }
        named.append((name, one["values"]?.arrayValue ?? []))
    }
    var points: [ChartPoint] = []
    for (i, label) in labels.enumerated() {
        for one in named {
            let value = i < one.values.count ? one.values[i].doubleValue ?? 0 : 0
            points.append(ChartPoint(id: points.count, category: CardElement.text(label), series: one.name, value: value.isFinite ? value : 0))
        }
    }
    return (points, names)
}

/// A pie's slices: one per label, 0 where a value is missing, not a number, or below zero.
func pieSlices(labels: [JSONValue], values: [JSONValue]) -> [ChartPoint] {
    labels.enumerated().map { i, label in
        let value = i < values.count ? values[i].doubleValue ?? 0 : 0
        return ChartPoint(id: i, category: CardElement.text(label), series: "", value: value.isFinite ? max(0, value) : 0)
    }
}

/// The series colours, in order. System colours, so they hold in light and in dark.
let chartPalette: [Color] = [.blue, .green, .orange, .purple, .pink, .teal, .yellow, .red, .indigo, .mint]

private func palette(_ count: Int) -> [Color] {
    (0..<max(count, 1)).map { chartPalette[$0 % chartPalette.count] }
}

private extension View {
    /// What every chart in a card shares: its height, quiet axes, and axis labels when the block gave them.
    func cardChart(x: String, y: String, legend: Bool) -> some View {
        chartLegend(legend ? .visible : .hidden)
            .chartLegend(position: .bottom, alignment: .leading, spacing: 10)
            .chartXAxisLabel(position: .bottom, alignment: .center) {
                if !x.isEmpty { Text(x).font(.caption).foregroundStyle(.secondary) }
            }
            .chartYAxisLabel(position: .top, alignment: .leading) {
                if !y.isEmpty { Text(y).font(.caption).foregroundStyle(.secondary) }
            }
            .frame(height: 200)
            .frame(maxWidth: .infinity)
    }
}

/// Vertical bars comparing a few categories, grouped side by side or stacked.
struct BarChartBlock: View {
    let element: CardElement

    var body: some View {
        let data = chartPoints(labels: element.list("labels"), series: element.list("series"))
        let stacked = element.text("variant") == "stacked"
        if !data.points.isEmpty {
            Chart(data.points) { point in
                if stacked {
                    BarMark(x: .value("Category", point.category), y: .value("Value", point.value))
                        .foregroundStyle(by: .value("Series", point.series))
                } else {
                    BarMark(x: .value("Category", point.category), y: .value("Value", point.value))
                        .foregroundStyle(by: .value("Series", point.series))
                        .position(by: .value("Series", point.series))
                        .cornerRadius(3)
                }
            }
            .chartForegroundStyleScale(domain: data.series, range: palette(data.series.count))
            .cardChart(x: element.text("xLabel"), y: element.text("yLabel"), legend: data.series.count > 1)
        }
    }
}

/// Lines showing a trend.
struct LineChartBlock: View {
    let element: CardElement

    var body: some View {
        let data = chartPoints(labels: element.list("labels"), series: element.list("series"))
        if !data.points.isEmpty {
            Chart(data.points) { point in
                LineMark(x: .value("Category", point.category), y: .value("Value", point.value))
                    .foregroundStyle(by: .value("Series", point.series))
                    .interpolationMethod(.monotone)
                    .lineStyle(StrokeStyle(lineWidth: 2.5, lineCap: .round, lineJoin: .round))
                PointMark(x: .value("Category", point.category), y: .value("Value", point.value))
                    .foregroundStyle(by: .value("Series", point.series))
                    .symbolSize(28)
            }
            .chartForegroundStyleScale(domain: data.series, range: palette(data.series.count))
            .cardChart(x: element.text("xLabel"), y: element.text("yLabel"), legend: data.series.count > 1)
        }
    }
}

/// Parts of one whole, as a pie or a ring.
struct PieChartBlock: View {
    let element: CardElement

    var body: some View {
        let slices = pieSlices(labels: element.list("labels"), values: element.list("values"))
        let names = distinct(slices.map(\.category))
        if !slices.isEmpty {
            Chart(slices) { slice in
                SectorMark(
                    angle: .value("Value", slice.value),
                    innerRadius: .ratio(element.text("variant") == "donut" ? 0.62 : 0),
                    angularInset: 1
                )
                .cornerRadius(3)
                .foregroundStyle(by: .value("Category", slice.category))
            }
            .chartForegroundStyleScale(domain: names, range: palette(names.count))
            .chartLegend(position: .bottom, alignment: .center, spacing: 12)
            .frame(height: 220)
            .frame(maxWidth: .infinity)
        }
    }
}
