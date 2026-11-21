//
//  MetalGameView.swift
//  HorizonPad
//
//  SwiftUI wrapper around MTKView. The controls live in Views/PlaySurface.swift
//  so this file stays a pure hosting shim.
//

import MetalKit
import SwiftUI

struct MetalGameView: UIViewRepresentable {
    let session: GameSession

    func makeUIView(context: Context) -> MTKView {
        let view = MTKView()
        view.device = MTLCreateSystemDefaultDevice()
        view.colorPixelFormat = .bgra8Unorm
        view.depthStencilPixelFormat = .depth32Float
        view.clearColor = MTLClearColor(red: 0.62, green: 0.74, blue: 0.92, alpha: 1)
        view.preferredFramesPerSecond = 60
        view.isMultipleTouchEnabled = true
        if let renderer = session.renderer {
            view.delegate = renderer
            renderer.camera.aspect = Float(view.drawableSize.width / max(view.drawableSize.height, 1))
        }
        return view
    }

    func updateUIView(_ view: MTKView, context: Context) {}

    static func dismantleUIView(_ view: MTKView, coordinator: ()) {
        view.delegate = nil
    }
}
