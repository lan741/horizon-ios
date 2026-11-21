//
//  PlaySurface.swift
//  HorizonPad
//
//  The whole play surface: Metal view underneath, touch controls and a
//  crosshair on top.
//
//  Input scheme (iPad, two thumbs):
//    left third   drag = movement joystick
//    right two thirds  drag = look
//    single tap   break the block under the crosshair
//    two-finger tap    place the selected block
//  Buttons on the right switch mode if you prefer not to remember gestures.
//

import SwiftUI

enum BuildMode: String, CaseIterable, Identifiable {
    case breakBlock = "挖"
    case placeBlock = "放"
    case none = "看"

    var id: String { rawValue }

    var icon: String {
        switch self {
        case .breakBlock: "hammer"
        case .placeBlock: "square.on.square"
        case .none: "hand.point.up.left"
        }
    }
}

struct PlaySurface: View {
    let session: GameSession

    @State private var dragOrigin: CGPoint?
    @State private var joystickOrigin: CGPoint = .zero
    @State private var joystickValue: CGSize = .zero
    @State private var lastLook: CGSize = .zero
    @State private var mode: BuildMode = .breakBlock
    @State private var selectedBlock: UInt32 = 0
    @State private var daylight: Double = 1.0

    var body: some View {
        ZStack {
            MetalGameView(session: session)
                .ignoresSafeArea()

            touchLayer

            Crosshair()
                .opacity(mode == .none ? 0.35 : 0.9)
                .allowsHitTesting(false)

            if dragOrigin != nil {
                joystick
            }

            controls
        }
        .statusBarHidden()
        .onAppear { selectedBlock = session.suggestedPlaceBlock() }
    }

    // MARK: Layers

    private var touchLayer: some View {
        GeometryReader { geo in
            Color.clear
                .contentShape(Rectangle())
                .gesture(
                    DragGesture(minimumDistance: 0)
                        .onChanged { value in
                            if dragOrigin == nil {
                                dragOrigin = value.startLocation
                                joystickOrigin = value.startLocation
                                lastLook = .zero
                            }
                            if value.startLocation.x < geo.size.width * 0.32 {
                                let dx = value.location.x - joystickOrigin.x
                                let dy = value.location.y - joystickOrigin.y
                                let limit: CGFloat = 56
                                joystickValue = CGSize(
                                    width: max(-limit, min(limit, dx)) / limit,
                                    height: max(-limit, min(limit, dy)) / limit
                                )
                                session.moveAxis = SIMD2(
                                    Float(joystickValue.width),
                                    Float(-joystickValue.height)
                                )
                            } else {
                                session.lookDelta = SIMD2(
                                    Float(value.translation.width - lastLook.width),
                                    Float(value.translation.height - lastLook.height)
                                )
                                lastLook = value.translation
                            }
                        }
                        .onEnded { value in
                            let travelled = hypot(value.translation.width, value.translation.height)
                            let wasJoystick = value.startLocation.x < geo.size.width * 0.32
                            dragOrigin = nil
                            joystickValue = .zero
                            lastLook = .zero
                            session.moveAxis = .zero
                            // A tap (not a drag) acts on the crosshair.
                            if travelled < 12 && !wasJoystick {
                                performAction()
                            }
                        }
                )
        }
    }

    private var joystick: some View {
        Circle()
            .fill(.white.opacity(0.16))
            .overlay(Circle().stroke(.white.opacity(0.35), lineWidth: 2))
            .frame(width: 112, height: 112)
            .overlay(
                Circle()
                    .fill(.white.opacity(0.55))
                    .frame(width: 44, height: 44)
                    .offset(x: joystickValue.width * 34, y: joystickValue.height * 34)
            )
            .position(x: joystickOrigin.x, y: joystickOrigin.y)
            .allowsHitTesting(false)
    }

    private var controls: some View {
        VStack {
            HStack(alignment: .top) {
                if let hit = session.crosshairHit {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("\(hit.block.x) \(hit.block.y) \(hit.block.z)")
                            .font(.caption2.monospacedDigit())
                        if let light = session.crosshairLight {
                            Text("sky \(light.sky) · block \(light.block) · \(light.brightness)")
                                .font(.caption2.monospacedDigit())
                                .foregroundStyle(.secondary)
                        }
                    }
                    .padding(8)
                    .background(.ultraThinMaterial, in: Capsule())
                    .padding(.leading, 16)
                    .padding(.top, 12)
                    .allowsHitTesting(false)
                }
                Spacer()
                VStack(spacing: 10) {
                    ForEach(BuildMode.allCases) { candidate in
                        Button {
                            mode = candidate
                        } label: {
                            Image(systemName: candidate.icon)
                                .frame(width: 44, height: 44)
                                .background(
                                    mode == candidate ? AnyShapeStyle(.tint) : AnyShapeStyle(.ultraThinMaterial),
                                    in: Circle()
                                )
                                .foregroundStyle(mode == candidate ? .white : .primary)
                        }
                    }
                    Button {
                        session.flying.toggle()
                    } label: {
                        Image(systemName: session.flying ? "airplane" : "figure.walk")
                            .frame(width: 44, height: 44)
                            .background(.ultraThinMaterial, in: Circle())
                    }
                    Button {
                        session.teleport(spawn: true)
                    } label: {
                        Image(systemName: "arrow.uturn.backward")
                            .frame(width: 44, height: 44)
                            .background(.ultraThinMaterial, in: Circle())
                    }
                    daylightControl
                }
                .padding(.trailing, 20)
                .padding(.top, 12)
            }
            Spacer()
        }
    }

    /// Dragging this re-bakes the sky channel: night makes torches matter.
    private var daylightControl: some View {
        VStack(spacing: 2) {
            Image(systemName: daylight > 0.5 ? "sun.max.fill" : "moon.fill")
                .font(.caption)
            Slider(value: $daylight, in: 0...1) { editing in
                if !editing {
                    session.daylight = Float(daylight)
                }
            }
            .frame(width: 96)
            .rotationEffect(.degrees(-90))
            .frame(width: 44, height: 96)
        }
        .padding(6)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 14))
        .onChange(of: daylight) { _, newValue in
            session.daylight = Float(newValue)
        }
    }

    // MARK: Actions

    private func performAction() {
        switch mode {
        case .breakBlock:
            session.breakTargeted()
        case .placeBlock:
            session.placeTargeted(blockID: selectedBlock)
        case .none:
            break
        }
    }
}

/// A small cross with a dot, drawn rather than using SF Symbols so it stays
/// crisp at any size and does not shift with Dynamic Type.
struct Crosshair: View {
    var body: some View {
        ZStack {
            Rectangle().frame(width: 2, height: 18)
            Rectangle().frame(width: 18, height: 2)
            Circle().frame(width: 3, height: 3)
        }
        .foregroundStyle(.white.opacity(0.85))
        .shadow(color: .black.opacity(0.6), radius: 1)
        .frame(width: 24, height: 24)
    }
}