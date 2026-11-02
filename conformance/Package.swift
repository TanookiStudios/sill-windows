// swift-tools-version:5.9
// The protocol conformance tool: builds Sill's own StreamProtocol (the reference) and
// (1) dumps the bytes it produces, for the Rust tests to compare against, and
// (2) decodes the bytes the Rust host produces with Sill's own decoders.
// SILL_PATH points at a checkout of github.com/Saffsanity/sill (default: ../../sill-reference).
import PackageDescription
import Foundation

let sill = ProcessInfo.processInfo.environment["SILL_PATH"] ?? "../../sill-reference"

let package = Package(
    name: "SillConformance",
    platforms: [.macOS(.v14)],
    dependencies: [.package(name: "Sill", path: sill)],
    targets: [
        .executableTarget(name: "SillConformance", dependencies: [.product(name: "StreamProtocol", package: "Sill")]),
    ]
)
