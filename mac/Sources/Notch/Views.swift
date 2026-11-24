import NotchCore
import SwiftUI

enum Metrics {
    static let panelWidth: CGFloat = 400
    static let rowHeight: CGFloat = 42
    static let ear: CGFloat = 8
    /// Width of each wing beside a hardware notch (or half the pill on other screens).
    static let wing: CGFloat = 84
}

enum Palette {
    static let blue = Color(red: 0.23, green: 0.61, blue: 1.0)
    static let amber = Color(red: 1.0, green: 0.69, blue: 0.13)
    static let green = Color(red: 0.2, green: 0.78, blue: 0.35)
    static let red = Color(red: 1.0, green: 0.27, blue: 0.23)
    static let violet = Color(red: 0.65, green: 0.55, blue: 0.98)
    static let dim = Color(white: 0.66)
    static let line = Color.white.opacity(0.09)
    static let chip = Color.white.opacity(0.08)

    static func state(_ s: SessionState) -> Color {
        switch s {
        case .working: return blue
        case .needsInput: return amber
        case .done: return green
        case .error: return red
        }
    }
}

extension Font {
    /// DM Sans ships inside the app; SwiftUI falls back to the system font if it is missing.
    static func ui(_ size: CGFloat, _ weight: Font.Weight = .regular) -> Font {
        .custom("DM Sans", size: size).weight(weight)
    }
}

/// The notch outline: flush with the top of the screen, small concave fillets where it
/// meets the edge, rounded bottom corners.
struct NotchShape: Shape {
    var radius: CGFloat
    var ear: CGFloat = Metrics.ear

    var animatableData: CGFloat {
        get { radius }
        set { radius = newValue }
    }

    func path(in rect: CGRect) -> Path {
        let e = min(ear, rect.height / 2)
        let r = min(radius, (rect.width - 2 * e) / 2, rect.height - e)
        let (l, rt, t, b) = (rect.minX + e, rect.maxX - e, rect.minY, rect.maxY)
        var p = Path()
        p.move(to: CGPoint(x: rect.minX, y: t))
        p.addQuadCurve(to: CGPoint(x: l, y: t + e), control: CGPoint(x: l, y: t))
        p.addLine(to: CGPoint(x: l, y: b - r))
        p.addQuadCurve(to: CGPoint(x: l + r, y: b), control: CGPoint(x: l, y: b))
        p.addLine(to: CGPoint(x: rt - r, y: b))
        p.addQuadCurve(to: CGPoint(x: rt, y: b - r), control: CGPoint(x: rt, y: b))
        p.addLine(to: CGPoint(x: rt, y: t + e))
        p.addQuadCurve(to: CGPoint(x: rect.maxX, y: t), control: CGPoint(x: rt, y: t))
        p.closeSubpath()
        return p
    }
}

/// A status dot. Working is steady; the states that want you pulse.
struct StatusDot: View {
    let state: SessionState
    let pulsing: Bool
    var size: CGFloat = 8
    @State private var phase = false

    var body: some View {
        let c = Palette.state(state)
        ZStack {
            if pulsing {
                Circle().stroke(c.opacity(phase ? 0 : 0.6), lineWidth: 1).frame(width: size + (phase ? 7 : 1), height: size + (phase ? 7 : 1))
            }
            Circle().fill(c).frame(width: size, height: size).opacity(pulsing ? (phase ? 1 : 0.75) : 1)
        }
        .frame(width: size + 8, height: size + 8)
        .onAppear { animate() }
        .onChange(of: pulsing) { _ in animate() }
    }

    private func animate() {
        phase = false
        guard pulsing else { return }
        withAnimation(.easeInOut(duration: 0.8).repeatForever(autoreverses: true)) { phase = true }
    }
}

struct AgentBadge: View {
    let agent: Agent
    var body: some View {
        let (text, color): (String, Color) = {
            switch agent {
            case .claude: return ("C", Color(red: 0.85, green: 0.47, blue: 0.34))
            case .codex: return ("X", Color(red: 0.06, green: 0.64, blue: 0.5))
            case .t3code: return ("T3", Palette.violet)
            }
        }()
        RoundedRectangle(cornerRadius: 8).fill(color.opacity(0.22))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(color.opacity(0.5), lineWidth: 1))
            .overlay(Text(text).font(.ui(11, .bold)).foregroundColor(.white))
            .frame(width: 26, height: 26)
    }
}

func formatDuration(_ secs: UInt64) -> String {
    if secs < 60 { return "\(secs)s" }
    if secs < 3600 { return "\(secs / 60)m" }
    if secs < 86400 { return "\(secs / 3600)h \(String(format: "%02d", (secs % 3600) / 60))m" }
    return "\(secs / 86400)d \((secs % 86400) / 3600)h"
}

struct SessionRow: View {
    @ObservedObject var state: AppState
    let session: Session
    let actions: Actions
    @State private var hovering = false

    var body: some View {
        let now = nowUnix()
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .top, spacing: 10) {
                AgentBadge(agent: session.agent)
                VStack(alignment: .leading, spacing: 1) {
                    Text(session.name).font(.ui(13, .semibold)).foregroundColor(.white).lineLimit(1)
                    Text(session.step).font(.ui(11)).foregroundColor(Palette.dim).lineLimit(1)
                }
                Spacer(minLength: 6)
                VStack(alignment: .trailing, spacing: 1) {
                    if hovering {
                        Button(action: { actions.dismiss(session.id) }) {
                            Image(systemName: "xmark").font(.system(size: 9, weight: .bold)).foregroundColor(Palette.dim)
                                .frame(width: 18, height: 18).background(Circle().fill(Palette.chip))
                        }.buttonStyle(.plain).help("Dismiss")
                    } else {
                        Text(elapsed(now)).font(.ui(11)).foregroundColor(Palette.dim).frame(height: 18)
                    }
                    HStack(spacing: 2) {
                        StatusDot(state: session.state, pulsing: session.pulsing(now), size: 6)
                        Text(label).font(.ui(11)).foregroundColor(Palette.state(session.state))
                    }
                }
            }
            ForEach(session.subs) { sub in
                HStack(spacing: 6) {
                    Circle().fill(Palette.blue).frame(width: 5, height: 5)
                    Text(sub.step.isEmpty ? (sub.kind.isEmpty ? "subagent" : sub.kind) : "\(sub.kind.isEmpty ? "subagent" : sub.kind) · \(sub.step)")
                        .font(.ui(11)).foregroundColor(Palette.dim).lineLimit(1)
                }.padding(.leading, 38)
            }
            if session.pending != nil {
                HStack(spacing: 8) {
                    decideButton("Approve", Palette.green) { actions.decide(session.id, true) }
                    decideButton("Deny", Palette.red) { actions.decide(session.id, false) }
                }.padding(.leading, 36).padding(.top, 2)
            }
        }
        .padding(.horizontal, 10).padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: 12).fill(hovering ? Color.white.opacity(0.07) : .clear))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .onTapGesture { actions.open(session) }
    }

    private var label: String {
        switch session.state {
        case .working: return "working"
        case .needsInput: return "needs input"
        case .done: return "done"
        case .error: return "error"
        }
    }

    private func elapsed(_ now: UInt64) -> String {
        if let f = session.finished, !session.active { return "\(formatDuration(now > f ? now - f : 0)) ago" }
        return formatDuration(now > session.started ? now - session.started : 0)
    }

    private func decideButton(_ title: String, _ color: Color, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title).font(.ui(12, .semibold)).foregroundColor(.white).padding(.horizontal, 14).padding(.vertical, 4)
                .background(Capsule().fill(color.opacity(0.26))).overlay(Capsule().stroke(color.opacity(0.7), lineWidth: 1))
        }.buttonStyle(.plain)
    }
}

/// What the views can ask the app to do.
struct Actions {
    var open: (Session) -> Void = { _ in }
    var dismiss: (String) -> Void = { _ in }
    var decide: (String, Bool) -> Void = { _, _ in }
    var toggleRemote: () -> Void = {}
    var toggleAwake: () -> Void = {}
    var toggleTravel: () -> Void = {}
    var toggleHooks: () -> Void = {}
    var refreshUsage: () -> Void = {}
    var setLoginItem: (Bool) -> Void = { _ in }
    var quit: () -> Void = {}
}

struct ToggleButton: View {
    let icon: String
    let title: String
    let detail: String
    let on: Bool
    let accent: Color
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: icon).font(.system(size: 13, weight: .medium)).foregroundColor(on ? accent : Palette.dim).frame(width: 18)
                VStack(alignment: .leading, spacing: 0) {
                    Text(title).font(.ui(13, .semibold)).foregroundColor(.white).lineLimit(1)
                    Text(detail).font(.ui(11)).foregroundColor(on ? accent : Palette.dim).lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12).frame(height: 40)
            .background(RoundedRectangle(cornerRadius: 12).fill(on ? accent.opacity(0.18) : Color.white.opacity(hovering ? 0.1 : 0.06)))
            .overlay(RoundedRectangle(cornerRadius: 12).stroke(on ? accent.opacity(0.55) : Palette.line, lineWidth: 1))
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
    }
}

struct UsageRow: View {
    let label: String
    let window: UsageWindow?

    var body: some View {
        HStack(spacing: 10) {
            Text(label).font(.ui(11)).foregroundColor(Palette.dim).frame(width: 34, alignment: .leading)
            if let w = window {
                let color: Color = w.pct > 90 ? Palette.red : (w.pct >= 60 ? Palette.amber : .white.opacity(0.85))
                GeometryReader { geo in
                    ZStack(alignment: .leading) {
                        Capsule().fill(Color.white.opacity(0.14))
                        Capsule().fill(color).frame(width: max(4, geo.size.width * w.pct / 100))
                    }
                }.frame(width: 110, height: 4)
                Text("\(Int(w.pct.rounded()))% used").font(.ui(13, .semibold)).foregroundColor(w.pct >= 60 ? color : .white)
                Spacer(minLength: 4)
                if let r = w.resetsAt {
                    let now = nowUnix()
                    Text(r > now ? "resets in \(formatDuration(r - now))" : "resetting").font(.ui(11)).foregroundColor(Palette.dim)
                }
            } else {
                Text("n/a").font(.ui(11)).foregroundColor(Palette.dim)
                Spacer()
            }
        }.frame(height: 20)
    }
}

/// The open panel.
struct PanelView: View {
    @ObservedObject var state: AppState
    let actions: Actions

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            if state.showSettings { settings } else { main }
        }
        .padding(.horizontal, 8).padding(.bottom, 12)
        .frame(width: Metrics.panelWidth, alignment: .top)
    }

    private var title: String {
        if state.showSettings { return "Settings" }
        let (n, active) = (state.store.sessions.count, state.store.activeCount)
        if n == 0 { return "Notch" }
        if active == 0 { return n == 1 ? "1 session, idle" : "\(n) sessions, idle" }
        return active == 1 ? "1 active session" : "\(active) active sessions"
    }

    private var header: some View {
        HStack {
            if state.showSettings {
                Button(action: { state.showSettings = false }) { Image(systemName: "chevron.left").font(.system(size: 12, weight: .semibold)).foregroundColor(Palette.dim).frame(width: 26, height: 24) }.buttonStyle(.plain)
            }
            Text(title).font(.ui(13, .semibold)).foregroundColor(.white)
            Spacer()
            if !state.showSettings {
                Button(action: { state.showSettings = true }) { Image(systemName: "gearshape").font(.system(size: 13)).foregroundColor(Palette.dim).frame(width: 26, height: 24) }.buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 10).frame(height: max(state.collapsedHeight, 34))
    }

    private var main: some View {
        VStack(alignment: .leading, spacing: 0) {
            if state.store.sessions.isEmpty {
                Text("No agents running right now.").font(.ui(12)).foregroundColor(Palette.dim).padding(.horizontal, 10).frame(height: Metrics.rowHeight)
            } else {
                // a short list is laid out directly (it also renders in snapshots, which a
                // scroll view does not); a long one scrolls
                if state.store.sessions.count <= 5 {
                    VStack(spacing: 0) {
                        ForEach(state.store.sessions) { s in SessionRow(state: state, session: s, actions: actions) }
                    }
                } else {
                    ScrollView(.vertical, showsIndicators: false) {
                        VStack(spacing: 0) {
                            ForEach(state.store.sessions) { s in SessionRow(state: state, session: s, actions: actions) }
                        }
                    }.frame(height: 300)
                }
            }
            Rectangle().fill(Palette.line).frame(height: 1).padding(.horizontal, 10).padding(.vertical, 6)
            usage.padding(.horizontal, 10)
            VStack(spacing: 8) {
                HStack(spacing: 8) {
                    ToggleButton(icon: "iphone", title: "Remote Control", detail: remoteDetail, on: state.remote != .off, accent: Palette.blue, action: actions.toggleRemote)
                    ToggleButton(icon: "sun.max", title: "Keep Awake", detail: state.awake ? "On" : "Off", on: state.awake, accent: Palette.amber, action: actions.toggleAwake)
                }
                ToggleButton(icon: "airplane", title: "Travel Mode", detail: state.config.travelMode ? "On · click to switch off" : "Off · Remote Control, Keep Awake, hotspot reminder", on: state.config.travelMode, accent: Palette.violet, action: actions.toggleTravel)
            }.padding(.horizontal, 6).padding(.top, 8)
        }
    }

    private var remoteDetail: String {
        switch state.remote {
        case .off: return "Off"
        case .starting: return "Starting…"
        case .ours: return "On · click to stop"
        case .external: return "On · already running"
        }
    }

    @ViewBuilder private var usage: some View {
        if !state.config.showUsage {
            Button(action: {
                state.config.showUsage = true
                state.saveConfig()
                actions.refreshUsage()
            }) {
                VStack(alignment: .leading, spacing: 1) {
                    Text("Show Claude usage limits").font(.ui(12, .semibold)).foregroundColor(.white)
                    Text("macOS will ask once to let Notch read your Claude sign-in").font(.ui(11)).foregroundColor(Palette.dim)
                }.frame(maxWidth: .infinity, alignment: .leading).frame(height: 40)
            }.buttonStyle(.plain)
        } else if state.usage.hasNumbers {
            VStack(spacing: 0) {
                UsageRow(label: "5h", window: state.usage.fiveHour)
                UsageRow(label: "Week", window: state.usage.week)
            }.contentShape(Rectangle()).onTapGesture { actions.refreshUsage() }
        } else {
            VStack(alignment: .leading, spacing: 1) {
                Text(state.usageLoading ? "Usage limits: loading…" : "Usage limits: not available right now").font(.ui(11)).foregroundColor(.white)
                Text(state.usage.note.isEmpty ? "Click to retry" : "\(state.usage.note) · click to retry").font(.ui(11)).foregroundColor(Palette.dim).lineLimit(1)
            }.frame(maxWidth: .infinity, alignment: .leading).frame(height: 40).contentShape(Rectangle()).onTapGesture { actions.refreshUsage() }
        }
    }

    private func settingRow(_ label: String, _ value: Binding<Bool>) -> some View {
        HStack {
            Text(label).font(.ui(12)).foregroundColor(.white)
            Spacer()
            Toggle("", isOn: Binding(get: { value.wrappedValue }, set: { value.wrappedValue = $0; state.saveConfig() })).labelsHidden().toggleStyle(.switch).controlSize(.mini)
        }.padding(.horizontal, 10).frame(height: 26)
    }

    private func adapter(_ key: String) -> Binding<Bool> {
        Binding(get: { state.config.adapters[key] ?? true }, set: { state.config.adapters[key] = $0 })
    }

    private var settings: some View {
        VStack(spacing: 0) {
            settingRow("Expand on hover", $state.config.hoverExpand)
            settingRow("Expand briefly on events", $state.config.autoExpand)
            settingRow("Notifications", $state.config.notifications)
            settingRow("Notification sound", $state.config.sound)
            settingRow("Approve / deny from the notch", $state.config.approveFromNotch)
            settingRow("Hide / show with Ctrl+\\", $state.config.hotkey)
            settingRow("Show Claude usage limits", $state.config.showUsage)
            settingRow("Claude Code adapter", adapter("claude"))
            settingRow("Codex adapter", adapter("codex"))
            settingRow("T3 Code adapter", adapter("t3code"))
            HStack {
                Text("Claude Code hooks").font(.ui(12)).foregroundColor(.white)
                Spacer()
                Button(action: actions.toggleHooks) {
                    Text(state.hooksInstalled ? "Installed · remove" : "Install").font(.ui(11, .semibold)).foregroundColor(.white).padding(.horizontal, 10).padding(.vertical, 3)
                        .background(Capsule().fill(state.hooksInstalled ? Palette.blue.opacity(0.3) : Palette.chip))
                }.buttonStyle(.plain)
            }.padding(.horizontal, 10).frame(height: 28)
            HStack {
                Spacer()
                Button(action: actions.quit) { Text("Quit Notch").font(.ui(11)).foregroundColor(Palette.dim) }.buttonStyle(.plain)
            }.padding(.horizontal, 10).frame(height: 26)
        }
    }
}

/// The collapsed strip: a count and dots on the left wing, usage on the right. On a screen
/// with a camera housing the two wings sit either side of it.
struct CollapsedView: View {
    @ObservedObject var state: AppState

    var body: some View {
        let now = nowUnix()
        HStack(spacing: 0) {
            HStack(spacing: 3) {
                if state.store.sessions.isEmpty {
                    Circle().fill(Palette.dim.opacity(0.6)).frame(width: 6, height: 6)
                } else {
                    Text("\(state.store.activeCount)").font(.ui(12, .semibold)).foregroundColor(.white).fixedSize()
                    ForEach(state.store.sessions.prefix(3)) { s in StatusDot(state: s.state, pulsing: s.pulsing(now), size: 7) }
                    if state.store.sessions.count > 3 { Image(systemName: "chevron.down").font(.system(size: 7, weight: .bold)).foregroundColor(Palette.dim) }
                }
                Spacer(minLength: 0)
            }.padding(.leading, 12).frame(width: Metrics.wing)
            Spacer(minLength: state.hardwareNotchWidth)
            HStack(spacing: 6) {
                Spacer(minLength: 0)
                if state.remote != .off { Image(systemName: "iphone").font(.system(size: 10)).foregroundColor(Palette.blue) }
                if state.awake { Image(systemName: "sun.max.fill").font(.system(size: 10)).foregroundColor(Palette.amber) }
                if let w = state.usage.fiveHour {
                    ZStack(alignment: .leading) {
                        Capsule().fill(Color.white.opacity(0.16)).frame(width: 30, height: 4)
                        Capsule().fill(w.pct > 90 ? Palette.red : (w.pct >= 60 ? Palette.amber : .white.opacity(0.8))).frame(width: max(3, 30 * w.pct / 100), height: 4)
                    }
                }
            }.padding(.trailing, 14).frame(width: Metrics.wing)
        }
        .frame(height: state.collapsedHeight)
    }
}

/// The whole notch: the black shape, with the strip or the panel inside it.
struct NotchView: View {
    @ObservedObject var state: AppState
    let actions: Actions

    var collapsedWidth: CGFloat { Metrics.wing * 2 + state.hardwareNotchWidth + Metrics.ear * 2 }

    var body: some View {
        VStack(spacing: 0) {
            ZStack(alignment: .top) {
                if state.expanded {
                    PanelView(state: state, actions: actions).transition(.opacity)
                } else {
                    CollapsedView(state: state).frame(width: collapsedWidth - Metrics.ear * 2).transition(.opacity)
                }
            }
            .padding(.horizontal, Metrics.ear)
            .background(NotchShape(radius: state.expanded ? 22 : 12).fill(Color.black))
            .clipShape(NotchShape(radius: state.expanded ? 22 : 12))
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .animation(.spring(response: 0.36, dampingFraction: 0.84), value: state.expanded)
        .animation(.spring(response: 0.36, dampingFraction: 0.84), value: state.showSettings)
        .environment(\.colorScheme, .dark)
    }
}
