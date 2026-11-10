// Protocol conformance against Sill's own StreamProtocol (the reference implementation).
//
//   SillConformance dump  DIR   writes the bytes Sill's code produces (messages and crypto vectors)
//   SillConformance check DIR   decodes the bytes the Rust host wrote (cargo test writes them) with
//                               Sill's own decoders and verifiers; exits 1 on any mismatch
//   SillConformance dial  PORT CODEFILE
//                               a real device-side run over Network.framework (the iPad's TLS stack):
//                               ask, pair with the code in CODEFILE, then a pinned sill/1 session
import Foundation
import CoreGraphics
import CryptoKit
import Network
import Security
import StreamProtocol

let ts = 1234.5
func message(_ kind: StreamMessageKind, _ payload: Data, key: Bool = false) -> Data {
    StreamMessage(kind: kind, timestamp: ts, isKeyframe: key, payload: payload).serialized()
}
func hex(_ d: Data) -> String { d.map { String(format: "%02x", $0) }.joined() }
func unhex(_ s: String) -> Data {
    var d = Data(); var i = s.startIndex
    while i < s.endIndex { let j = s.index(i, offsetBy: 2); d.append(UInt8(s[i..<j], radix: 16)!); i = j }
    return d
}
func keyBytes(_ k: SymmetricKey) -> Data { k.withUnsafeBytes { Data($0) } }
var failures = 0
func expect(_ ok: Bool, _ what: String) {
    print(ok ? "ok    \(what)" : "FAIL  \(what)")
    if !ok { failures += 1 }
}

// Fixed keys, so both sides compute the same vectors.
let macKey = try! P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 0x01, count: 32))
let deviceKey = try! P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 0x02, count: 32))
let macPoint = macKey.publicKey.x963Representation
let devicePoint = deviceKey.publicKey.x963Representation
let fpMac = SPKI.fingerprint(point: macPoint)
let fpDevice = SPKI.fingerprint(point: devicePoint)
let secret = Data(repeating: 0x22, count: 16)
let recognitionKey = Data(repeating: 0x11, count: 32)
let nonce = Data([1, 2, 3, 4, 5, 6])
let code11 = [4, 8, 2, 9, 1, 3, 5, 5, 7, 2, 0]

func dump(_ dir: URL) throws {
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    func write(_ name: String, _ data: Data) throws { try data.write(to: dir.appendingPathComponent(name)) }

    // Host → device: what a host sends, as Sill's types serialize it.
    let vps = unhex("40010c01ffff016000000300b0000003000003005dac09")
    let sps = unhex("420101016000000300b0000003000003005da00280802d16")
    let pps = unhex("4401c172b46240")
    try write("ps.bin", message(.parameterSets, ParameterSets(nalUnitHeaderLength: 4, sets: [vps, sps, pps]).encoded(), key: true))
    try write("frame.bin", message(.frame, unhex("000000032601af00000002d001"), key: true))
    try write("tick.bin", message(.tick, Data()))
    var ping = Data(); withUnsafeBytes(of: (99.25).bitPattern.bigEndian) { ping.append(contentsOf: $0) }
    try write("pong.bin", message(.pong, ping))
    try write("windowlist.bin", message(.windowList, Wire.encode(WindowList(
        macName: "Test PC", windows: [], active: .desktop, launchID: "L1", hostVersion: "0.1.0", protocol: 1))))
    try write("hostsettings.bin", message(.hostSettings, Wire.encode(HostSettingsState(
        settings: StreamSettings(maxFPS: 60, bitrate: 15_000_000, captureScale: 1, prioritizeSpeed: true, virtualDisplay: false,
                                 directWireless: nil, sendAudio: nil),
        persistent: false, virtualDisplayAvailable: false, virtualDisplayNote: "Not on Windows.", softwareEncoder: false,
        stream: RunningStream(width: 1920, height: 1080, fps: 60, mbps: 15, onVirtualDisplay: false),
        answering: 3, sessionPaired: true))))
    let proofM = PairingProof.macProof(key: PairingProof.qrKey(secret: secret), macFingerprint: fpMac, deviceFingerprint: fpDevice)
    try write("pairresult_ok.bin", message(.pairResult, Wire.encode(PairResult(
        ok: true, proof: Base64URL.encode(proofM), macID: MacID.make(fingerprint: fpMac), name: "Test PC",
        recognitionKey: Base64URL.encode(recognitionKey)))))
    try write("pairresult_shown.bin", message(.pairResult, Wire.encode(PairResult(ok: false, reason: PairResult.shown))))
    try write("pairresult_code.bin", message(.pairResult, Wire.encode(PairResult(ok: false, reason: PairResult.code, triesLeft: 3))))
    try write("pairresult_busy.bin", message(.pairResult, Wire.encode(PairResult(ok: false, reason: PairResult.busy, retryAfter: 1.5))))
    try write("goodbye_quit.bin", message(.goodbye, Wire.encode(Goodbye(reason: Goodbye.quit))))

    // Device → host: what a device sends, which the host must decode.
    let inputs: [(String, InputEvent)] = [
        ("in_move", .pointer(.move, x: 0.25, y: 0.75)),
        ("in_leftdown", .pointer(.leftDown, x: 0.5, y: 0.5)),
        ("in_leftup", .pointer(.leftUp, x: 0.5, y: 0.5)),
        ("in_rightdown", .pointer(.rightDown, x: 0.1, y: 0.2)),
        ("in_rightup", .pointer(.rightUp, x: 0.1, y: 0.2)),
        ("in_scroll", .scroll(x: 0.5, y: 0.5, dx: -0.01, dy: 0.05)),
        ("in_scrollgesture", .scrollGesture(.began, x: 0.5, y: 0.5)),
        ("in_momentum", .scrollGesture(.momentumEnded, x: 0.5, y: 0.5)),
        ("in_text", .text("Hé\n")),
        ("in_key", .key(hidUsage: 0x06, down: true, modifiers: 1 << 20)),
    ]
    for (name, event) in inputs { try write("\(name).bin", message(.input, Wire.encode(event))) }
    try write("hello.bin", message(.hello, Wire.encode(Hello(appVersion: "1.0", build: "42", protocol: 1, device: "iPad (iPad14,1)",
                                                            audio: ["aac-eld"]))))
    try write("viewport.bin", message(.viewport, Wire.encode(Viewport(width: 1180, height: 820, scale: nil, localCursor: true, fps: 120))))
    try write("select_desktop.bin", message(.selectSource, Wire.encode(StreamSource.desktop)))
    try write("select_window.bin", message(.selectSource, Wire.encode(StreamSource.window(77))))
    try write("select_none.bin", message(.selectSource, Wire.encode(StreamSource.none)))
    try write("ping.bin", message(.ping, ping))
    try write("keyframe_request.bin", message(.keyframeRequest, Data()))
    try write("change_settings.bin", message(.changeSettings, Wire.encode(HostSettingsChange(token: 7, maxFPS: 120))))
    try write("pair_ask.bin", message(.pairRequest, Wire.encode(PairRequest(method: PairRequest.ask, proof: "", name: "iPad", model: "iPad14,1"))))
    let proofD = PairingProof.deviceProof(key: PairingProof.qrKey(secret: secret), deviceFingerprint: fpDevice, macFingerprint: fpMac)
    try write("pair_qr.bin", message(.pairRequest, Wire.encode(PairRequest(method: PairRequest.qr, proof: Base64URL.encode(proofD),
                                                                          name: "iPad", model: "iPad14,1"))))

    try write("gesture.bin", message(.gesture, Wire.encode(TrackpadGesture(gesture: TrackpadGesture.swipeUp, fingers: 3))))
    try write("clipboard_sub.bin", message(.clipboard, Wire.encode(Clipboard())))
    try write("clipboard_text.bin", message(.clipboard, Wire.encode(Clipboard(text: "Hé"))))
    try write("clipboard_stop.bin", message(.clipboard, Wire.encode(Clipboard(stop: true))))
    try write("fetch_menu_top.bin", message(.fetchMenu, Wire.encode(FetchMenu(token: 1))))
    try write("fetch_menu.bin", message(.fetchMenu, Wire.encode(FetchMenu(version: 3, id: "0", title: "File", token: 2))))
    try write("press_menu.bin", message(.pressMenuItem, Wire.encode(PressMenuItem(version: 3, id: "0.0", title: "Save", token: 3))))
    try write("launch_app.bin", message(.launchApp, Wire.encode(LaunchApp(bundleID: "C:\\apps\\calc.lnk"))))
    try write("window_command.bin", message(.windowCommand, Wire.encode(WindowCommand(id: 77, action: .fullScreen))))
    try write("viewport_scaled.bin", message(.viewport, Wire.encode(Viewport(width: 1180, height: 820, scale: 1.25))))
    var full = WindowList(macName: "Test PC", windows: [WindowInfo(id: 77, title: "Doc", appName: "Word", bundleID: "C:\\w.exe", width: 800, height: 600)],
                          active: .window(77), launchID: "L1", hostVersion: "0.1.0", protocol: 1, gestures: 1, clipboard: 1)
    full.keyRepeat = 1
    try write("windowlist_full.bin", message(.windowList, Wire.encode(full)))
    try write("thumbnail.bin", message(.thumbnail, ImageBlob.encodeThumbnail(windowID: 77, jpeg: Data([0xFF, 0xD8, 1]))))
    try write("icon.bin", message(.appIcon, ImageBlob.encodeIcon(bundleID: "C:\\w.exe", png: Data([0x89, 0x50, 1]))))
    try write("applist.bin", message(.appList, Wire.encode([AppInfo(name: "Calc", bundleID: "C:\\c.lnk")])))
    try write("cursor.bin", message(.cursorShape, CursorShapeBlob.encode(hotspot: CGPoint(x: 1.5, y: 2), size: CGSize(width: 32, height: 32), png: Data([0x89, 0x50, 1]))))
    try write("pointer.bin", message(.macPointer, Wire.encode(MacPointer(x: 0.25, y: 0.5, inside: true, seen: 12))))
    try write("pointer_off.bin", message(.macPointer, Wire.encode(MacPointer(inside: false, seen: 12))))
    try write("clipboard_out.bin", message(.clipboard, Wire.encode(Clipboard(text: "Hé"))))

    // Crypto and text vectors.
    let code = code11.map(String.init).joined() + String(PairingCode.checkDigit(code11))
    let codeKey = PairingProof.codeKey(code: code, macFingerprint: fpMac)!
    let addresses = [try! AddressParser.parse("192.168.1.20").get(), try! AddressParser.parse("fd7a:115c:a1e0::1234").get()]
    let link = PairLink(fingerprint: fpMac, secret: secret, name: "Maddie's PC & Co", port: 7455, addresses: addresses)
    let certKey = RemoteKey.importPrivate(macKey.x963Representation)!
    let cert = SelfSignedCertificate.make(key: certKey)!
    let vectors: [String: String] = [
        "macPoint": hex(macPoint), "devicePoint": hex(devicePoint),
        "macSPKI": hex(SPKI.der(point: macPoint)), "fpMac": hex(fpMac), "fpDevice": hex(fpDevice),
        "fpMacB64U": Base64URL.encode(fpMac), "macID": MacID.make(fingerprint: fpMac),
        "secret": hex(secret), "qrDeviceProof": hex(proofD), "qrMacProof": hex(proofM),
        "code": code, "codeKey": hex(keyBytes(codeKey)),
        "codeDeviceProof": hex(PairingProof.deviceProof(key: codeKey, deviceFingerprint: fpDevice, macFingerprint: fpMac)),
        "codeMacProof": hex(PairingProof.macProof(key: codeKey, macFingerprint: fpMac, deviceFingerprint: fpDevice)),
        "recognitionKey": hex(recognitionKey), "nonce": hex(nonce),
        "tag": RecognitionTag.make(recognitionKey: recognitionKey, nonce: nonce)!,
        "linkURL": link.url, "certDER": hex(cert),
    ]
    let json = try JSONSerialization.data(withJSONObject: vectors, options: [.prettyPrinted, .sortedKeys])
    try write("vectors.json", json)
    print("Wrote Swift fixtures to \(dir.path)")
}

func readMessage(_ data: Data) -> (StreamHeader, Data)? {
    guard let h = StreamMessage.parseHeader(data), data.count == StreamMessage.headerLength + h.payloadLength else { return nil }
    return (h, data.subdata(in: StreamMessage.headerLength..<data.count))
}

func check(_ dir: URL) throws {
    func load(_ name: String) -> (StreamHeader, Data)? {
        guard let d = try? Data(contentsOf: dir.appendingPathComponent(name)) else { expect(false, "\(name) exists"); return nil }
        let m = readMessage(d)
        expect(m != nil, "\(name): header parses and length matches")
        return m
    }
    if let (h, p) = load("ps.bin") {
        let ps = ParameterSets(encoded: p)
        expect(h.kind == .parameterSets && h.isKeyframe && ps?.nalUnitHeaderLength == 4 && ps?.sets.count == 3, "ps.bin decodes: 3 sets, NAL length 4")
    }
    if let (h, p) = load("frame.bin") { expect(h.kind == .frame && h.isKeyframe && !p.isEmpty, "frame.bin is a keyframe") }
    if let (h, p) = load("tick.bin") { expect(h.kind == .tick && p.isEmpty, "tick.bin is empty") }
    if let (h, p) = load("pong.bin") { expect(h.kind == .pong && p.count == 8, "pong.bin echoes 8 bytes") }
    if let (h, p) = load("windowlist.bin") {
        let l = Wire.decode(WindowList.self, from: p)
        expect(h.kind == .windowList && l != nil && l?.active == .desktop && l?.windows.isEmpty == true && l?.protocol == 1,
               "windowlist.bin decodes as WindowList, active desktop: \(String(data: p, encoding: .utf8) ?? "")")
    }
    if let (h, p) = load("hostsettings.bin") {
        let s = Wire.decode(HostSettingsState.self, from: p)
        expect(h.kind == .hostSettings && s != nil && s?.answering == 3, "hostsettings.bin decodes as HostSettingsState: \(String(data: p, encoding: .utf8) ?? "")")
    }
    if let (h, p) = load("goodbye.bin") {
        expect(h.kind == .goodbye && Wire.decode(Goodbye.self, from: p)?.reason == Goodbye.quit, "goodbye.bin decodes, reason quit")
    }
    if let (h, p) = load("pairresult_shown.bin") {
        let r = Wire.decode(PairResult.self, from: p)
        expect(h.kind == .pairResult && r?.ok == false && r?.reason == PairResult.shown && r?.unknownReasonMessage == nil, "pairresult_shown.bin")
    }
    if let (h, p) = load("pairresult_code.bin") {
        let r = Wire.decode(PairResult.self, from: p)
        expect(h.kind == .pairResult && r?.reason == PairResult.code && r?.triesLeft == 3, "pairresult_code.bin")
    }
    if let (h, p) = load("windowlist_full.bin") {
        let l = Wire.decode(WindowList.self, from: p)
        expect(h.kind == .windowList && l?.active == .window(77) && l?.windows.first?.bundleID == "C:\\w.exe" && l?.gestures == 1 && l?.clipboard == 1 && l?.keyRepeat == 1,
               "windowlist_full.bin: a window, active, with gestures, clipboard and key repeat")
    }
    if let (h, p) = load("thumbnail.bin") {
        let t = ImageBlob.decodeThumbnail(p)
        expect(h.kind == .thumbnail && t?.windowID == 77 && t?.jpeg == Data([0xFF, 0xD8, 1]), "thumbnail.bin")
    }
    if let (h, p) = load("icon.bin") {
        let i = ImageBlob.decodeIcon(p)
        expect(h.kind == .appIcon && i?.bundleID == "C:\\w.exe", "icon.bin")
    }
    if let (h, p) = load("applist.bin") {
        expect(h.kind == .appList && Wire.decode([AppInfo].self, from: p)?.first?.bundleID == "C:\\c.lnk", "applist.bin")
    }
    if let (h, p) = load("cursor.bin") {
        let c = CursorShapeBlob.decode(p)
        expect(h.kind == .cursorShape && c?.hotspot.x == 1.5 && c?.hotspot.y == 2 && c?.size.width == 32 && c?.size.height == 32, "cursor.bin")
    }
    if let (h, p) = load("pointer.bin") {
        let m = Wire.decode(MacPointer.self, from: p)
        expect(h.kind == .macPointer && m?.position?.x == 0.25 && m?.seen == 12, "pointer.bin")
    }
    if let (_, p) = load("pointer_off.bin") {
        let m = Wire.decode(MacPointer.self, from: p)
        expect(m?.inside == false && m?.position == nil && m?.seen == 12, "pointer_off.bin")
    }
    if let (h, p) = load("menu_top.bin") {
        let m = Wire.decode(MacMenu.self, from: p)
        expect(h.kind == .macMenu && m?.version == 3 && m?.answering == 1 && m?.app == "Word" && m?.bundleID == "C:\\apps\\word.exe"
               && m?.menus?.first?.id == "0" && m?.menus?.first?.title == "File" && m?.menus?.first?.submenu == true,
               "menu_top.bin decodes as MacMenu's top level: \(String(data: p, encoding: .utf8) ?? "")")
    }
    if let (h, p) = load("menu_items.bin") {
        let m = Wire.decode(MacMenu.self, from: p)
        let i = m?.items ?? []
        expect(h.kind == .macMenu && m?.menu == "0" && i.count == 4 && i[0].key == "⌘S" && i[1].separator == true
               && i[2].enabled == false && i[3].mark == "✓" && i[3].id == "0.3",
               "menu_items.bin decodes as one menu's items: \(String(data: p, encoding: .utf8) ?? "")")
    }
    if let (h, p) = load("menu_pressed.bin") {
        let m = Wire.decode(MacMenu.self, from: p)
        expect(h.kind == .macMenu && m?.pressed == true && m?.answering == 3, "menu_pressed.bin")
    }
    if let (h, p) = load("menu_refused.bin") {
        let m = Wire.decode(MacMenu.self, from: p)
        expect(h.kind == .macMenu && m?.pressed == false && m?.note == "The menus changed. Open the menu again.", "menu_refused.bin")
    }
    if let (h, p) = load("clipboard_out.bin") {
        expect(h.kind == .clipboard && Wire.decode(Clipboard.self, from: p)?.text == "Hé", "clipboard_out.bin")
    }
    if let (h, p) = load("audio_format.bin") {
        if case .format(let f)? = AudioMessage.parse(p) {
            expect(h.kind == .audio && f.codec == AudioCodec.aacELD && f.sampleRate == 48000 && f.channels == 2 && f.framesPerPacket == 480 && f.epoch == 7 && f.cookie?.isEmpty == false,
                   "audio_format.bin parses: AAC-ELD, \(f.primingFrames ?? -1) priming frames, cookie \(f.cookie.map(hex) ?? "none")")
        } else { expect(false, "audio_format.bin parses") }
    }
    if let (h, p) = load("audio_packet.bin") {
        if case .packets(let a)? = AudioMessage.parse(p) {
            expect(h.kind == .audio && a.epoch == 7 && a.seq == 9 && a.segmentStart && a.packets.count == 1, "audio_packet.bin parses")
        } else { expect(false, "audio_packet.bin parses") }
    }
    if let (h, p) = load("macinfo.bin") {
        let s = Wire.decode(SignedMacInfo.self, from: p)
        let v = s?.verified()
        expect(h.kind == .macInfo && v != nil && v?.info.remotePort == 7455 && v?.info.addresses.first?.kind == "vpn",
               "macinfo.bin: the signature, Mac ID and fields verify with SignedMacInfo.verified()")
    }
    // The Rust host's ok, its proof, its link, its tag and its certificate, checked by Sill's own code.
    guard let vdata = try? Data(contentsOf: dir.appendingPathComponent("vectors.json")),
          let v = try? JSONSerialization.jsonObject(with: vdata) as? [String: String] else { expect(false, "vectors.json"); return }
    if let (h, p) = load("pairresult_ok.bin"), let r = Wire.decode(PairResult.self, from: p) {
        let proof = Base64URL.decode(r.proof ?? "") ?? Data()
        expect(h.kind == .pairResult && r.ok && r.macID == MacID.make(fingerprint: fpMac) && Base64URL.decode(r.recognitionKey ?? "")?.count == 32,
               "pairresult_ok.bin: ok, macID of the Mac key, 32-byte recognition key")
        expect(PairingProof.isValidMacProof(proof, key: PairingProof.qrKey(secret: secret), macFingerprint: fpMac, deviceFingerprint: fpDevice),
               "pairresult_ok.bin: proof_M checks as the device checks it")
    }
    switch PairLink.parse(v["linkURL"] ?? "") {
    case .success(let link):
        expect(link.fingerprint == fpMac && link.secret == secret && link.port == 7455 && link.name == "Maddie's PC & Co" && link.addresses.count == 2,
               "the Rust link parses with PairLink.parse: \(v["linkURL"] ?? "")")
    case .failure(let e): expect(false, "the Rust link parses: \(e)")
    }
    expect(RecognitionTag.matches(v["tag"] ?? "", recognitionKey: recognitionKey), "the Rust recognition tag matches")
    let certData = unhex(v["certDER"] ?? "")
    if let cert = SecCertificateCreateWithData(nil, certData as CFData) {
        expect(SPKI.fingerprint(of: cert).map(hex) == v["certFingerprint"], "the Rust certificate parses and its SPKI fingerprint matches")
    } else {
        expect(false, "the Rust certificate parses with SecCertificateCreateWithData")
    }
}

// MARK: dial: the device's side over Network.framework, as the iPad runs it.

final class Dialer {
    let queue = DispatchQueue(label: "dial")
    let port: NWEndpoint.Port
    let identity: RemoteIdentity
    init(port: UInt16) {
        self.port = NWEndpoint.Port(rawValue: port)!
        identity = RemoteIdentity(privateKey: RemoteKey.generate()!)!
    }

    /// One connection with `alpn`, pinned to `pin` (nil: any key). Returns the connection and the Mac's key.
    func connect(_ alpn: String, pin: Data?) -> (NWConnection, Data)? {
        var seen: Data?
        let tls = RemoteTLS.options(identity: identity.tls, role: .client(alpn: alpn), verify: { fp in
            seen = fp
            return fp != nil && (pin == nil || fp == pin)
        }, queue: queue)
        let c = NWConnection(host: "127.0.0.1", port: port, using: RemoteTLS.parameters(tls: tls, dialing: true))
        let ready = DispatchSemaphore(value: 0)
        var ok = false
        c.stateUpdateHandler = { state in
            switch state {
            case .ready: ok = true; ready.signal()
            case .failed(let e), .waiting(let e): print("  \(alpn): \(e)"); ready.signal()
            default: break
            }
        }
        c.start(queue: queue)
        _ = ready.wait(timeout: .now() + 10)
        guard ok, let fp = seen else { c.cancel(); return nil }
        print("  \(alpn) ready, negotiated \(RemoteTLS.negotiatedALPN(c) ?? "none"), Mac \(RemoteIdentity.shortName(fp))")
        return (c, fp)
    }

    func send(_ c: NWConnection, _ kind: StreamMessageKind, _ payload: Data) {
        c.send(content: StreamMessage(kind: kind, timestamp: Date().timeIntervalSince1970, isKeyframe: false, payload: payload).serialized(),
               completion: .contentProcessed { _ in })
    }

    func read(_ c: NWConnection, timeout: Double = 15) -> (StreamHeader, Data)? {
        let done = DispatchSemaphore(value: 0)
        var result: (StreamHeader, Data)?
        c.receive(minimumIncompleteLength: 14, maximumLength: 14) { data, _, _, error in
            guard let data, let h = StreamMessage.parseHeader(data) else {
                if let error { print("  read: \(error)") }
                done.signal(); return
            }
            if h.payloadLength == 0 { result = (h, Data()); done.signal(); return }
            c.receive(minimumIncompleteLength: h.payloadLength, maximumLength: h.payloadLength) { p, _, _, _ in
                if let p { result = (h, p) }
                done.signal()
            }
        }
        _ = done.wait(timeout: .now() + timeout)
        return result
    }

    func pairRequest(_ c: NWConnection, _ method: String, proof: Data?) -> PairResult? {
        send(c, .pairRequest, Wire.encode(PairRequest(method: method, proof: proof.map(Base64URL.encode) ?? "", name: "Conformance", model: "Mac")))
        guard let (h, p) = read(c), h.kind == .pairResult else { return nil }
        print("  kind 20: \(String(data: p, encoding: .utf8) ?? "")")
        return Wire.decode(PairResult.self, from: p)
    }

    func run(codeFile: String) {
        print("ask:")
        guard let (c1, askedKey) = connect(RemoteTLS.pairingALPN, pin: nil) else { expect(false, "pairing connection"); return }
        let asked = pairRequest(c1, PairRequest.ask, proof: nil)
        c1.cancel()
        expect(asked?.reason == PairResult.shown, "the ask is answered shown")
        guard let text = try? String(contentsOfFile: codeFile, encoding: .utf8) else { expect(false, "code file \(codeFile)"); return }
        guard case .success(let code) = PairingCode.check(text) else { expect(false, "the code file holds a valid code"); return }
        print("code \(PairingCode.grouped(code)):")
        guard let (c2, fp) = connect(RemoteTLS.pairingALPN, pin: askedKey) else { expect(false, "second pairing connection"); return }
        let key = PairingProof.codeKey(code: code, macFingerprint: fp)!
        let proofD = PairingProof.deviceProof(key: key, deviceFingerprint: identity.fingerprint, macFingerprint: fp)
        let r = pairRequest(c2, PairRequest.code, proof: proofD)
        c2.cancel()
        let proofM = Base64URL.decode(r?.proof ?? "") ?? Data()
        expect(r?.ok == true && PairingProof.isValidMacProof(proofM, key: key, macFingerprint: fp, deviceFingerprint: identity.fingerprint)
               && r?.macID == MacID.make(fingerprint: fp) && Base64URL.decode(r?.recognitionKey ?? "")?.count == 32,
               "paired with the typed code: proof_M, macID and recognition key check")
        print("session:")
        guard let (s, _) = connect(RemoteTLS.sessionALPN, pin: fp) else { expect(false, "pinned sill/1 session"); return }
        send(s, .hello, Wire.encode(Hello(appVersion: "1.0", build: "1", protocol: 1, device: "Conformance (Mac)")))
        send(s, .viewport, Wire.encode(Viewport(width: 1180, height: 820, scale: nil, localCursor: true, fps: 60)))
        send(s, .selectSource, Wire.encode(StreamSource.desktop))
        var ping = Data(); withUnsafeBytes(of: (4321.125).bitPattern.bigEndian) { ping.append(contentsOf: $0) }
        send(s, .ping, ping)
        var kinds: [StreamMessageKind] = []
        var gotList = false, gotSettings = false, gotPS = false, gotKey = false, gotPong = false
        let deadline = Date().addingTimeInterval(8)
        while Date() < deadline, !(gotList && gotSettings && gotPS && gotKey && gotPong) {
            guard let (h, p) = read(s, timeout: 8) else { break }
            if kinds.count < 12 { kinds.append(h.kind) }
            switch h.kind {
            case .windowList: gotList = Wire.decode(WindowList.self, from: p) != nil
            case .hostSettings: gotSettings = Wire.decode(HostSettingsState.self, from: p) != nil
            case .parameterSets: gotPS = ParameterSets(encoded: p) != nil
            case .frame: if h.isKeyframe && gotPS { gotKey = true }
            case .pong: gotPong = p == ping
            default: break
            }
        }
        print("  first kinds: \(kinds.map { String($0.rawValue) }.joined(separator: " "))")
        expect(gotList, "the session's window list decodes")
        expect(gotSettings, "the session's host settings decode")
        expect(gotPS && gotKey, "parameter sets, then a keyframe")
        expect(gotPong, "a ping is answered with its payload unchanged")
        s.cancel()
        print("an unpaired key on sill/1:")
        let stranger = Dialer(port: port.rawValue)
        if let (u, _) = stranger.connect(RemoteTLS.sessionALPN, pin: fp) {
            stranger.send(u, .hello, Wire.encode(Hello()))
            var refusal: NWError?
            let done = DispatchSemaphore(value: 0)
            u.receive(minimumIncompleteLength: 1, maximumLength: 14) { data, _, _, error in refusal = error; if data != nil { refusal = nil }; done.signal() }
            _ = done.wait(timeout: .now() + 5)
            // -9825 or -9829 is what a device reads as "this computer removed me" (DiscoveryPolicy).
            let status = refusal.flatMap(RemoteTLS.status(of:))
            expect(status == -9825 || status == -9829, "an unpaired key is refused inside TLS with -9825 or -9829 (got \(status.map(String.init) ?? "no error"))")
            u.cancel()
        } else {
            expect(true, "an unpaired key is refused in the handshake")
        }
    }
}

let args = CommandLine.arguments
switch args.count > 1 ? args[1] : "" {
case "dump" where args.count == 3: try dump(URL(fileURLWithPath: args[2]))
case "check" where args.count == 3: try check(URL(fileURLWithPath: args[2]))
case "dial" where args.count == 4: Dialer(port: UInt16(args[2])!).run(codeFile: args[3])
default:
    print("usage: SillConformance dump DIR | check DIR | dial PORT CODEFILE")
    exit(2)
}
if failures > 0 { print("\(failures) FAILED"); exit(1) }
print("all checks passed")
