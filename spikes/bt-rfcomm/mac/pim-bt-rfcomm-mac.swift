// pim-bt-rfcomm-mac.swift
//
// Mac BT auto-discovery sidecar over RFCOMM/SPP.
//
//  - Polls IOBluetoothDevice.pairedDevices() every 30s.
//  - Filters by name prefix "PIM-".
//  - For each candidate without an active RFCOMM channel:
//      * Opens RFCOMM channel 1 (SPP convention).
//      * Sends `Hello` (JSON, length-prefixed u32 BE).
//      * Awaits `HelloAck`.
//      * On success: prints `{"event":"discovered","peer":...}` on stdout.
//      * On close: prints `{"event":"lost","peer":...,"reason":...}` on stdout.
//  - Also accepts inbound RFCOMM connections by registering a channel
//    handler — paired Linux can dial us back if it scans paired devices too.
//
// Build:  swiftc -framework IOBluetooth -O -o pim-bt-rfcomm-mac pim-bt-rfcomm-mac.swift
// Run:    ./pim-bt-rfcomm-mac [--name=PIM-pepe] [--node-id=<hex>] [--prefix=PIM-]
//
// The output is newline-delimited JSON on stdout, suitable to be piped
// into pim-daemon's IPC consumer (next iteration).

import Foundation
import IOBluetooth

// MARK: - Args

struct Args {
    var localName: String = "PIM-mac"
    var localNodeId: String = ""
    var prefix: String = "PIM-"
    var rfcommChannel: BluetoothRFCOMMChannelID = 22
    var pollInterval: TimeInterval = 30
}

func parseArgs() -> Args {
    var a = Args()
    for arg in CommandLine.arguments.dropFirst() {
        if let v = arg.value(after: "--name=") { a.localName = v }
        else if let v = arg.value(after: "--node-id=") { a.localNodeId = v }
        else if let v = arg.value(after: "--prefix=") { a.prefix = v }
        else if let v = arg.value(after: "--channel="), let n = UInt8(v) {
            a.rfcommChannel = BluetoothRFCOMMChannelID(n)
        }
        else if let v = arg.value(after: "--poll="), let s = TimeInterval(v) {
            a.pollInterval = s
        }
    }
    if a.localNodeId.isEmpty {
        a.localNodeId = randomHex(bytes: 32)
    }
    return a
}

extension String {
    func value(after prefix: String) -> String? {
        guard hasPrefix(prefix) else { return nil }
        return String(dropFirst(prefix.count))
    }
}

func randomHex(bytes: Int) -> String {
    var b = [UInt8](repeating: 0, count: bytes)
    _ = SecRandomCopyBytes(kSecRandomDefault, bytes, &b)
    return b.map { String(format: "%02x", $0) }.joined()
}

let ARGS = parseArgs()

// MARK: - Logging (newline-delimited JSON to stdout/stderr)

let stdoutLock = NSLock()

func emit(_ obj: [String: Any]) {
    stdoutLock.lock(); defer { stdoutLock.unlock() }
    if let data = try? JSONSerialization.data(withJSONObject: obj, options: []),
       var line = String(data: data, encoding: .utf8) {
        line.append("\n")
        FileHandle.standardOutput.write(line.data(using: .utf8)!)
    }
}

func logErr(_ s: String) {
    FileHandle.standardError.write("[pim-bt-rfcomm-mac] \(s)\n".data(using: .utf8)!)
}

// MARK: - Frame I/O (u32 BE length prefix + utf8 JSON payload)

enum FrameError: Error { case tooLarge, malformed, eof }

class FrameReader {
    private var buffer = Data()
    private let maxPayload = 65_536

    /// Feed raw bytes; returns any complete payloads available.
    func feed(_ chunk: Data) throws -> [Data] {
        buffer.append(chunk)
        var out: [Data] = []
        while true {
            guard buffer.count >= 4 else { break }
            let len: UInt32 = buffer.prefix(4).withUnsafeBytes {
                let p = $0.bindMemory(to: UInt8.self)
                return (UInt32(p[0]) << 24) | (UInt32(p[1]) << 16)
                     | (UInt32(p[2]) << 8)  |  UInt32(p[3])
            }
            if len == 0 || Int(len) > maxPayload { throw FrameError.tooLarge }
            guard buffer.count >= 4 + Int(len) else { break }
            let payload = buffer.subdata(in: 4..<(4 + Int(len)))
            buffer.removeSubrange(0..<(4 + Int(len)))
            out.append(payload)
        }
        return out
    }
}

func encodeFrame(_ json: [String: Any]) throws -> Data {
    let payload = try JSONSerialization.data(withJSONObject: json, options: [.sortedKeys])
    guard payload.count <= 65_536 else { throw FrameError.tooLarge }
    var hdr = Data(count: 4)
    let n = UInt32(payload.count)
    hdr[0] = UInt8((n >> 24) & 0xff)
    hdr[1] = UInt8((n >> 16) & 0xff)
    hdr[2] = UInt8((n >> 8)  & 0xff)
    hdr[3] = UInt8( n        & 0xff)
    var out = Data(); out.append(hdr); out.append(payload); return out
}

// MARK: - Per-channel session

final class Session: NSObject, IOBluetoothRFCOMMChannelDelegate {
    let bdAddr: String
    let bdName: String
    weak var channel: IOBluetoothRFCOMMChannel?
    let isInitiator: Bool
    let reader = FrameReader()
    var helloSent = false
    var ackReceived = false
    var peerInfo: [String: Any] = [:]
    let openedAt = Date()

    init(addr: String, name: String, channel: IOBluetoothRFCOMMChannel, initiator: Bool) {
        self.bdAddr = addr
        self.bdName = name
        self.channel = channel
        self.isInitiator = initiator
        super.init()
    }

    func send(_ json: [String: Any]) {
        guard let ch = channel else { return }
        do {
            let frame = try encodeFrame(json)
            frame.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
                let p = UnsafeMutableRawPointer(mutating: raw.baseAddress!)
                _ = ch.writeSync(p, length: UInt16(frame.count))
            }
        } catch {
            logErr("send error to \(bdAddr): \(error)")
        }
    }

    func sendHello() {
        send([
            "type": "hello",
            "v": 1,
            "node_id": ARGS.localNodeId,
            "name": ARGS.localName,
            "platform": "macos",
            "caps": ["mesh-v1"],
        ])
        helloSent = true
    }

    func sendHelloAck() {
        send([
            "type": "hello-ack",
            "v": 1,
            "node_id": ARGS.localNodeId,
            "name": ARGS.localName,
            "platform": "macos",
            "caps": ["mesh-v1"],
        ])
    }

    // MARK: IOBluetoothRFCOMMChannelDelegate

    func rfcommChannelOpenComplete(_ ch: IOBluetoothRFCOMMChannel!, status err: IOReturn) {
        if err != kIOReturnSuccess {
            emit(["event": "open_failed", "bd_addr": bdAddr, "name": bdName,
                  "code": String(format: "0x%x", err)])
            registry.remove(addr: bdAddr)
            return
        }
        if isInitiator { sendHello() }
        // If acceptor, wait for peer's hello then ack.
    }

    func rfcommChannelData(_ ch: IOBluetoothRFCOMMChannel!,
                           data ptr: UnsafeMutableRawPointer!, length n: Int) {
        let chunk = Data(bytes: ptr, count: n)
        do {
            for payload in try reader.feed(chunk) {
                handlePayload(payload)
            }
        } catch {
            logErr("decode error from \(bdAddr): \(error)")
            ch.close()
        }
    }

    func rfcommChannelClosed(_ ch: IOBluetoothRFCOMMChannel!) {
        emit(["event": "lost",
              "peer": peerInfo.isEmpty ? ["bd_addr": bdAddr, "name": bdName] : peerInfo,
              "reason": "channel_closed"])
        registry.remove(addr: bdAddr)
    }

    private func handlePayload(_ payload: Data) {
        guard let obj = try? JSONSerialization.jsonObject(with: payload) as? [String: Any],
              let type = obj["type"] as? String else {
            logErr("bad payload from \(bdAddr): \(String(data: payload, encoding: .utf8) ?? "<bin>")")
            return
        }
        switch type {
        case "hello":
            // We are the acceptor. Capture peer info, ack.
            peerInfo = Session.extractIdentity(obj)
            sendHelloAck()
            emitDiscovered()
        case "hello-ack":
            // We are the initiator.
            peerInfo = Session.extractIdentity(obj)
            ackReceived = true
            emitDiscovered()
        case "error":
            emit(["event": "peer_error", "bd_addr": bdAddr, "detail": obj])
        default:
            // Future mesh frames go here. Echo for now to prove duplex.
            emit(["event": "frame", "bd_addr": bdAddr, "type": type])
        }
    }

    /// Strip protocol meta fields (type, v); keep only peer identity.
    static func extractIdentity(_ msg: [String: Any]) -> [String: Any] {
        var out = msg
        out.removeValue(forKey: "type")
        out.removeValue(forKey: "v")
        return out
    }

    private func emitDiscovered() {
        var peer: [String: Any] = peerInfo
        peer["bd_addr"] = bdAddr
        peer["since"] = ISO8601DateFormatter().string(from: openedAt)
        emit(["event": "discovered", "peer": peer])
    }
}

// MARK: - Registry of active sessions

final class Registry {
    private let q = DispatchQueue(label: "registry")
    private var sessions: [String: Session] = [:]
    func has(addr: String) -> Bool { q.sync { sessions[addr] != nil } }
    func put(_ s: Session) { q.sync { sessions[s.bdAddr] = s } }
    func remove(addr: String) { q.sync { sessions.removeValue(forKey: addr) } }
    func allAddrs() -> [String] { q.sync { Array(sessions.keys) } }
}

let registry = Registry()

// MARK: - Outbound discovery loop (poll paired devices)

func discoveryTick() {
    let paired = IOBluetoothDevice.pairedDevices() ?? []
    for any in paired {
        guard let dev = any as? IOBluetoothDevice else { continue }
        let name = dev.name ?? ""
        let addr = dev.addressString ?? ""
        guard name.hasPrefix(ARGS.prefix), !addr.isEmpty else { continue }
        if registry.has(addr: addr) { continue }
        // Try outbound RFCOMM.
        emit(["event": "scan_attempt", "bd_addr": addr, "name": name,
              "channel": Int(ARGS.rfcommChannel)])
        var ch: IOBluetoothRFCOMMChannel?
        let session = Session(addr: addr, name: name, channel: ch ?? IOBluetoothRFCOMMChannel(),
                              initiator: true)
        // openRFCOMMChannelAsync wants &ch, set delegate to session.
        let r = dev.openRFCOMMChannelAsync(&ch, withChannelID: ARGS.rfcommChannel,
                                            delegate: session)
        if r == kIOReturnSuccess, let opened = ch {
            session.channel = opened
            registry.put(session)
        } else {
            emit(["event": "open_failed", "bd_addr": addr, "name": name,
                  "code": String(format: "0x%x", r)])
        }
    }
}

// MARK: - Inbound: register an RFCOMM channel listener
//
// The IOBluetoothRFCOMMChannel notification API calls the selector with
// TWO arguments — the IOBluetoothUserNotification and the channel — not
// a Foundation.Notification. Selector signature must match exactly,
// otherwise Swift's auto-bridging tries to cast IOBluetoothUserNotification
// to NSNotification and crashes with `unrecognized selector sent to
// instance` on -[IOBluetoothConcreteUserNotification name].

final class InboundHandler: NSObject {
    @objc func handleIncoming(
        _ userNotification: IOBluetoothUserNotification,
        channel ch: IOBluetoothRFCOMMChannel
    ) {
        guard let dev = ch.getDevice() else { ch.close(); return }
        let addr = dev.addressString ?? "unknown"
        let name = dev.name ?? ""
        // Filter to PIM-* peers; close anyone else.
        guard name.hasPrefix(ARGS.prefix) else { ch.close(); return }
        if registry.has(addr: addr) {
            // Outbound session already exists with this peer — drop the
            // duplicate inbound channel for now. Production may multiplex.
            ch.close(); return
        }
        let s = Session(addr: addr, name: name, channel: ch, initiator: false)
        ch.setDelegate(s)
        registry.put(s)
        emit(["event": "inbound", "bd_addr": addr, "name": name])
    }
}

let inboundHandler = InboundHandler()
let userInfoChannel: BluetoothRFCOMMChannelID = ARGS.rfcommChannel
IOBluetoothRFCOMMChannel.register(
    forChannelOpenNotifications: inboundHandler,
    selector: #selector(InboundHandler.handleIncoming(_:channel:)),
    withChannelID: userInfoChannel,
    direction: kIOBluetoothUserNotificationChannelDirectionIncoming
)

// MARK: - Main loop

emit(["event": "boot", "name": ARGS.localName, "node_id": ARGS.localNodeId,
      "prefix": ARGS.prefix, "channel": Int(ARGS.rfcommChannel),
      "poll_s": ARGS.pollInterval])

DispatchQueue.global().async {
    while true {
        discoveryTick()
        Thread.sleep(forTimeInterval: ARGS.pollInterval)
    }
}

RunLoop.main.run()
