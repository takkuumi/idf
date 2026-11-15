// Read-only CoreBluetooth traffic generator for full_link_stress.py.
// Connect ONLY to the supplied, previously identified peripheral UUID; verify
// configured Ethernet MAC with C6 before generating any test traffic.
import Foundation
import CoreBluetooth

let serviceID = CBUUID(string: "4FAFC201-1FB5-459E-8FCC-C5C9C331914B")
let characteristicID = CBUUID(string: "BEB5483E-36E1-4688-B7F5-EA07361B26A8")
func crc16(_ bytes: [UInt8]) -> UInt16 {
    var crc: UInt16 = 0xffff
    for byte in bytes {
        crc ^= UInt16(byte)
        for _ in 0..<8 { crc = crc & 1 != 0 ? (crc >> 1) ^ 0xa001 : crc >> 1 }
    }
    return crc
}
func hex(_ bytes: [UInt8]) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
func unhex(_ text: String) -> [UInt8] {
    var result = [UInt8](); var i = text.startIndex
    while i < text.endIndex {
        let j = text.index(i, offsetBy: 2, limitedBy: text.endIndex) ?? text.endIndex
        guard let byte = UInt8(text[i..<j], radix: 16) else { return [] }
        result.append(byte); i = j
    }
    return result
}
func mono() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e9 }
let parameters = CommandLine.arguments
if parameters.count != 3 { fputs("Usage: imc_ble_load <control.json> <fixture.json>\n", stderr); exit(2) }
let controlPath = parameters[1]
let fixture = try JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: parameters[2]))) as! [String: Any]
let wantedID = UUID(uuidString: fixture["ble_peripheral"] as! String)!
let wantedMAC = unhex(fixture["ethernet_mac_hex"] as! String)
let baseline = fixture["baseline_pdu_hex"] as! [String: String]
let outputDirectory = fixture["output_directory"] as! String
let eventPath = outputDirectory + "/ble.jsonl"
FileManager.default.createFile(atPath: eventPath, contents: nil)
let output = FileHandle(forWritingAtPath: eventPath)!
func emit(_ type: String, _ fields: [String: Any] = [:]) {
    var object = fields
    object["event"] = type; object["epoch_s"] = Date().timeIntervalSince1970
    if let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]) {
        output.write(data); output.write(Data([10]))
    }
}
struct Pending {
    let transaction: UInt16
    let phase: String
    let kind: String
    let key: String
    let started: Double
    let expected: [UInt8]
}
final class Generator: NSObject, CBCentralManagerDelegate, CBPeripheralDelegate {
    var manager: CBCentralManager!
    var peer: CBPeripheral?
    var characteristic: CBCharacteristic?
    var phase = "initial"
    var enabled = false
    var profile = "mixed"
    var gap = 0.0
    var reconnectSeconds = 0.0
    var terminal = false
    var ready = false
    var connecting = false
    var disconnectPlanned = false
    var connectionStarted = 0.0
    var connectedAt = 0.0
    var lastAttempt = -100.0
    var nextRequestAt = 0.0
    var pending: Pending?
    var receive = [UInt8]()
    var sequence: UInt16 = 0x7000
    var ordinal = 0
    var timer: Timer?

    override init() {
        super.init()
        manager = CBCentralManager(delegate: self, queue: .main)
        timer = Timer.scheduledTimer(withTimeInterval: 0.01, repeats: true) { [weak self] _ in self?.tick() }
        emit("process_started", ["target_uuid": wantedID.uuidString])
    }
    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        emit("adapter_state", ["raw": central.state.rawValue])
    }
    var lastControlAt = -100.0
    func tick() {
        let now = mono()
        if now - lastControlAt > 0.1 {
            lastControlAt = now
            if let data = try? Data(contentsOf: URL(fileURLWithPath: controlPath)),
               let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] {
                let newPhase = object["phase"] as? String ?? "idle"
                if phase != newPhase { phase = newPhase; emit("phase_seen", ["phase": phase]) }
                enabled = object["ble_enabled"] as? Bool ?? false
                profile = object["profile"] as? String ?? "mixed"
                gap = (object["ble_gap_ms"] as? Double ?? 0) / 1000
                reconnectSeconds = object["ble_reconnect_s"] as? Double ?? 0
                terminal = object["stop"] as? Bool ?? false
            }
        }
        if terminal {
            enabled = false
            if pending == nil { stop(); return }
        }
        if let request = pending, now - request.started > 5 {
            emit("request_error", ["phase": request.phase, "kind": request.kind, "key": request.key,
                                  "transaction": request.transaction, "error": "application_response_timeout",
                                  "latency_ms": (now - request.started) * 1000])
            pending = nil; close(planned: true); return
        }
        if !enabled {
            if pending == nil && (peer != nil || connecting) { close(planned: true) }
            return
        }
        if connecting && now - connectionStarted > 12 {
            emit("connection_error", ["phase": phase, "error": "connect_or_discovery_timeout"])
            close(planned: true); return
        }
        if ready {
            if pending == nil && reconnectSeconds > 0 && now - connectedAt >= reconnectSeconds {
                emit("planned_reconnect", ["phase": phase]); close(planned: true); return
            }
            if pending == nil && now >= nextRequestAt { requestNext() }
        } else if !connecting && peer == nil && now - lastAttempt >= 1 && manager.state == .poweredOn {
            connectTarget()
        }
    }
    func connectTarget() {
        lastAttempt = mono(); connectionStarted = mono(); connecting = true
        if let existing = manager.retrievePeripherals(withIdentifiers: [wantedID]).first {
            peer = existing; existing.delegate = self
            emit("connect_start", ["phase": phase, "target_uuid": existing.identifier.uuidString])
            manager.connect(existing, options: nil)
        } else {
            manager.scanForPeripherals(withServices: [serviceID], options: nil)
            emit("scan_start", ["phase": phase])
        }
    }
    func centralManager(_ central: CBCentralManager, didDiscover peripheral: CBPeripheral,
                        advertisementData: [String: Any], rssi RSSI: NSNumber) {
        guard peripheral.identifier == wantedID else { return }
        manager.stopScan(); peer = peripheral; peripheral.delegate = self
        emit("target_discovered", ["phase": phase, "rssi_dbm": RSSI])
        manager.connect(peripheral, options: nil)
    }
    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        receive.removeAll(keepingCapacity: true)
        emit("gatt_connected", ["phase": phase, "connection_ms": (mono() - connectionStarted) * 1000])
        peripheral.readRSSI()
        peripheral.discoverServices([serviceID])
    }
    func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
        emit("connection_error", ["phase": phase, "error": String(describing: error)])
        peer = nil; characteristic = nil; ready = false; connecting = false
    }
    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?) {
        if let request = pending {
            emit("request_error", ["phase": request.phase, "kind": request.kind, "key": request.key,
                                  "error": "disconnected_before_response", "latency_ms": (mono() - request.started) * 1000])
            pending = nil
        }
        emit("disconnected", ["phase": phase, "planned": disconnectPlanned,
                              "error": String(describing: error)])
        peer = nil; characteristic = nil; ready = false; connecting = false
        disconnectPlanned = false; lastAttempt = mono()
        receive.removeAll(keepingCapacity: true)
    }
    func peripheral(_ peripheral: CBPeripheral, didReadRSSI RSSI: NSNumber, error: Error?) {
        emit("rssi", ["phase": phase, "dbm": RSSI, "error": String(describing: error)])
    }
    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard error == nil, let service = peripheral.services?.first(where: { $0.uuid == serviceID }) else {
            emit("connection_error", ["phase": phase, "error": "service_discovery_failed: \(String(describing: error))"])
            close(planned: true); return
        }
        peripheral.discoverCharacteristics([characteristicID], for: service)
    }
    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        guard error == nil, let value = service.characteristics?.first(where: { $0.uuid == characteristicID }) else {
            emit("connection_error", ["phase": phase, "error": "characteristic_discovery_failed"])
            close(planned: true); return
        }
        characteristic = value; peripheral.setNotifyValue(true, for: value)
    }
    func peripheral(_ peripheral: CBPeripheral, didUpdateNotificationStateFor characteristic: CBCharacteristic, error: Error?) {
        guard error == nil, characteristic.isNotifying else {
            emit("connection_error", ["phase": phase, "error": "notify_enable_failed"])
            close(planned: true); return
        }
        send(payload: [1, 0xc6], key: "identity", kind: "identity", expected: [0xc6, 0] + wantedMAC)
    }
    func requestNext() {
        let counts = profile == "bulk" ? [100, 125] : [1, 2, 8, 32, 100, 125]
        let count = counts[ordinal % counts.count]; ordinal += 1
        let key = "3:2300:\(count)"
        guard let expectedText = baseline[key] else { fatalError("Missing fixture for \(key)") }
        let payload: [UInt8] = [1, 3, 0x08, 0xfc, UInt8(count >> 8), UInt8(count & 255)]
        send(payload: payload, key: key, kind: "read", expected: [1] + unhex(expectedText))
    }
    func send(payload: [UInt8], key: String, kind: String, expected: [UInt8]) {
        guard let peripheral = peer, let value = characteristic, pending == nil else { return }
        sequence = sequence &+ 1
        if sequence < 0x100 { sequence = 0x7000 }
        var frame: [UInt8] = [UInt8(sequence >> 8), UInt8(sequence & 255), 0, 0,
                              UInt8(payload.count >> 8), UInt8(payload.count & 255)] + payload
        let crc = crc16(frame); frame += [UInt8(crc & 255), UInt8(crc >> 8)]
        pending = Pending(transaction: sequence, phase: phase, kind: kind, key: key,
                          started: mono(), expected: expected)
        // All generated read requests fit legacy 20-byte write-with-response.
        peripheral.writeValue(Data(frame), for: value, type: .withResponse)
    }
    func peripheral(_ peripheral: CBPeripheral, didWriteValueFor characteristic: CBCharacteristic, error: Error?) {
        if let error = error {
            if let request = pending {
                emit("request_error", ["phase": request.phase, "kind": request.kind, "key": request.key,
                                      "error": "gatt_write: \(error)", "latency_ms": (mono() - request.started) * 1000])
                pending = nil
            }
            close(planned: true)
        }
    }
    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        guard error == nil, let data = characteristic.value else {
            emit("notification_error", ["phase": phase, "error": String(describing: error)]); return
        }
        receive += [UInt8](data)
        while receive.count >= 8 {
            let length = Int(receive[4]) * 256 + Int(receive[5])
            if receive[2] != 0 || receive[3] != 0 || length < 2 || length > 504 {
                emit("framing_error", ["phase": phase, "head_hex": hex(Array(receive.prefix(8)))])
                receive.removeFirst(); continue
            }
            let size = 6 + length + 2
            if receive.count < size { break }
            let frame = Array(receive.prefix(size)); receive.removeFirst(size)
            let returnedCRC = UInt16(frame[size - 2]) | UInt16(frame[size - 1]) << 8
            guard crc16(Array(frame.prefix(size - 2))) == returnedCRC else {
                emit("crc_error", ["phase": phase, "frame_hex": hex(frame)]); continue
            }
            let transaction = UInt16(frame[0]) << 8 | UInt16(frame[1])
            let payload = Array(frame[6..<(size - 2)])
            guard let request = pending, transaction == request.transaction else {
                if transaction <= 1 { emit("unsolicited", ["phase": phase, "transaction": transaction, "payload_hex": hex(payload)]) }
                else { emit("unexpected_transaction", ["phase": phase, "transaction": transaction, "frame_hex": hex(frame)]) }
                continue
            }
            pending = nil
            let ok = payload == request.expected
            emit(ok ? "request_ok" : "request_error", ["phase": request.phase, "kind": request.kind,
                    "key": request.key, "transaction": request.transaction,
                    "latency_ms": (mono() - request.started) * 1000, "response_bytes": frame.count,
                    "useful_bytes": request.kind == "read" ? max(0, payload.count - 3) : 6,
                    "error": ok ? "" : "payload_mismatch", "response_hex": ok ? "" : hex(frame)])
            if request.kind == "identity" {
                if !ok { emit("fatal_identity_mismatch"); stop(); return }
                ready = true; connecting = false; connectedAt = mono()
                emit("ready", ["phase": phase, "mac": hex(wantedMAC),
                               "ready_ms": (mono() - connectionStarted) * 1000,
                               "max_write_with_response": peripheral.maximumWriteValueLength(for: .withResponse),
                               "max_write_without_response": peripheral.maximumWriteValueLength(for: .withoutResponse)])
            }
            nextRequestAt = mono() + gap
            DispatchQueue.main.asyncAfter(deadline: .now() + gap) { [weak self] in
                guard let self = self, self.enabled, self.ready, self.pending == nil, mono() >= self.nextRequestAt else { return }
                self.requestNext()
            }
        }
    }
    func close(planned: Bool) {
        disconnectPlanned = planned; manager.stopScan()
        if let peripheral = peer { manager.cancelPeripheralConnection(peripheral) }
        else { connecting = false; ready = false }
    }
    func stop() {
        timer?.invalidate(); manager.stopScan()
        disconnectPlanned = true
        if let peripheral = peer { manager.cancelPeripheralConnection(peripheral) }
        emit("process_stopped"); try? output.synchronize()
        CFRunLoopStop(CFRunLoopGetMain())
    }
}
let generator = Generator()
RunLoop.main.run()
