//
//  HorizonPadTests.swift
//  HorizonPadTests
//
//  These tests exercise the Swift↔Rust seam with a pack built by the engine
//  itself. They need a host build of hz-ffi to be linkable, so they run on the
//  iOS simulator rather than as pure logic tests.
//

import XCTest
@testable import HorizonPad

@MainActor
final class EngineABITests: XCTestCase {
    func testAPIVersionMatchesHeader() {
        XCTAssertEqual(Engine.apiVersion, UInt32(HZ_API_VERSION))
    }

    func testErrorSurfaceIsNotEmptyOnBadPath() {
        XCTAssertThrowsError(try Engine.summary(ofPackAt: "/nonexistent/pack.hzp")) { error in
            let message = error.localizedDescription
            XCTAssertFalse(message.isEmpty)
        }
    }
}

@MainActor
final class ReportParsingTests: XCTestCase {
    func testParsesCoverageAndJVM() throws {
        let json = """
        {"coverage":0.87,"java_classes":1234,
         "jvm":{"classes":42,"methods":900,"bytecode_bytes":65536,"call_edges":77,
                "hot_targets":[{"owner":"net/minecraft/Foo","method":"bar","call_sites":9}]}}
        """
        let parsed = ImportController.parseReport(json)
        XCTAssertEqual(parsed.coverage, 0.87, accuracy: 0.0001)
        XCTAssertEqual(parsed.javaClasses, 1234)
        let jvm = try XCTUnwrap(parsed.jvm)
        XCTAssertEqual(jvm.classes, 42)
        XCTAssertEqual(jvm.methods, 900)
        XCTAssertEqual(jvm.topTargets.first?.owner, "net.minecraft.Foo")
        XCTAssertEqual(jvm.topTargets.first?.callSites, 9)
    }

    func testEmptyObjectIsSafe() {
        let parsed = ImportController.parseReport("{}")
        XCTAssertEqual(parsed.coverage, 0)
        XCTAssertNil(parsed.jvm)
    }
}

@MainActor
final class ProgressTests: XCTestCase {
    func testFractionIsMonotonicAcrossStages() {
        var previous = -1.0
        for stage in PortStage.allCases {
            for step in [0, 50, 100] {
                let p = PortProgress(stage: stage, done: UInt32(step), total: 100)
                XCTAssertGreaterThanOrEqual(p.fraction, previous)
                previous = p.fraction
            }
        }
        XCTAssertLessThanOrEqual(previous, 1.0)
    }

    func testZeroTotalIsSafe() {
        let p = PortProgress(stage: .models, done: 0, total: 0)
        XCTAssertEqual(p.fraction, Double(PortStage.models.rawValue) / Double(PortStage.allCases.count))
    }
}

@MainActor
final class RayHitTests: XCTestCase {
    func testAdjacentFollowsTheNormal() {
        let down = GameHandle.RayHit(block: SIMD3(4, 7, -2),
                                     normal: SIMD3(0, 1, 0),
                                     distance: 3)
        XCTAssertEqual(down.adjacent, SIMD3(4, 8, -2))

        let side = GameHandle.RayHit(block: SIMD3(0, 0, 0),
                                     normal: SIMD3(-1, 0, 0),
                                     distance: 5)
        XCTAssertEqual(side.adjacent, SIMD3(-1, 0, 0))

        let diagonalish = GameHandle.RayHit(block: SIMD3(-3, 1, 9),
                                            normal: SIMD3(0, 0, -1),
                                            distance: 2)
        XCTAssertEqual(diagonalish.adjacent, SIMD3(-3, 1, 8))
    }

    func testVertexStrideMatchesTheHeader() {
        // 16 floats is what hz_game::frame::FLOATS_PER_VERTEX declares; if that
        // changes, the Metal descriptor in Renderer.swift has to change too.
        XCTAssertEqual(GameHandle.floatsPerVertex, 16)
        XCTAssertEqual(MemoryLayout<Float>.stride * 16, 64)
    }
}

@MainActor
final class PathTests: XCTestCase {
    func testContainerDirectoriesExist() {
        XCTAssertTrue(FileManager.default.fileExists(atPath: Paths.root.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: Paths.packs.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: Paths.worlds.path))
    }
}