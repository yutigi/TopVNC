// Measures how far ahead of the screen edge a TopVNC viewer on macOS shows the
// center of each update (spec 008, viewer early present): in every display
// frame of the viewer's window, the frame number at the center is compared
// with the one at the bottom-left edge.
//
// 1. cargo run --release --example fovea_latency -- --frame PATH.rgb
//      --serve 127.0.0.1:5999 --link 150:8 --fps 97
// 2. cargo run --release -- 127.0.0.1:5999 --allow-insecure, then Connect,
//    and leave the window in Fit mode with the frame's aspect.
// 3. swift tools/center_lead.swift VIEWER_PID SECONDS
//
// With --publish-log /tmp/publish.log in step 1 (macOS), and the same path
// after the other arguments in step 3, it also reports the time from
// publishing each frame to its being on screen, at the center and at the edge.
//
// fovea_latency draws the frame number in 16-pixel blocks at (896, 512), in
// the fovea, and at (0, 960), in the periphery. A viewer that presents whole
// updates always shows the same number at both; one that presents the center
// early shows the newer number at the center for part of every update. Pick a
// frame rate that is not a divisor of the display's refresh rate (97 fps on a
// 120 Hz display) so the phase between them drifts and the sampling averages
// out. Needs Screen Recording permission for the terminal. Usage:
// center_lead PID SECONDS [SCENE_WIDTH SCENE_HEIGHT] [--publish-log PATH]
import AppKit
import CoreMedia
import CoreVideo
import Foundation
import ScreenCaptureKit

var arguments = CommandLine.arguments
var publishLog: String?
if let flag = arguments.firstIndex(of: "--publish-log"), flag + 1 < arguments.count {
    publishLog = arguments[flag + 1]
    arguments.removeSubrange(flag...(flag + 1))
}
guard arguments.count >= 3, let pid = Int32(arguments[1]), let seconds = Double(arguments[2]) else {
    print("usage: center_lead PID SECONDS [SCENE_WIDTH SCENE_HEIGHT] [--publish-log PATH]")
    exit(2)
}
let sceneWidth = arguments.count > 3 ? Double(arguments[3])! : 1920
let sceneHeight = arguments.count > 4 ? Double(arguments[4])! : 1080
let centerAt = (x: 896.0, y: 512.0)
let edgeAt = (x: 0.0, y: 960.0)

// ScreenCaptureKit needs this process's window server connection.
_ = NSApplication.shared
_ = CGMainDisplayID()

var timebase = mach_timebase_info_data_t()
mach_timebase_info(&timebase)

/// The frame numbers shown at the center and the edge of each display frame.
final class Probe: NSObject, SCStreamOutput {
    var captured = 0
    /// Display frames by center number minus edge number.
    var differences: [Int: Int] = [:]
    /// When a number first appeared at the center, and at the edge.
    var centerSeen: [Int: UInt64] = [:]
    var edgeSeen: [Int: UInt64] = [:]
    var lastCenter = -1
    var lastEdge = -1
    var firstReads: [(center: Int, edge: Int)] = []
    let lock = NSLock()

    /// The 16-bit number drawn as a 4x4 grid of 16-pixel blocks at `origin`.
    func read(_ base: UnsafePointer<UInt8>, stride: Int, width: Int, height: Int, scale: Double, top: Double,
              at origin: (x: Double, y: Double)) -> Int? {
        var value = 0
        for bit in 0..<16 {
            let column = Double(bit % 4), row = Double(bit / 4)
            let x = (origin.x + column * 16 + 8) * scale
            let y = top + (origin.y + row * 16 + 8) * scale
            guard x >= 0, Int(x) < width, y >= 0, Int(y) < height else { return nil }
            if base[Int(y) * stride + Int(x) * 4 + 1] >= 128 { value |= 1 << bit }
        }
        return value
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let info = attachments.first,
              let status = info[.status] as? Int, status == SCFrameStatus.complete.rawValue,
              let displayTime = info[.displayTime] as? UInt64,
              let pixels = sampleBuffer.imageBuffer
        else { return }
        let nanos = displayTime * UInt64(timebase.numer) / UInt64(timebase.denom)
        CVPixelBufferLockBaseAddress(pixels, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pixels, .readOnly) }
        let width = CVPixelBufferGetWidth(pixels)
        let height = CVPixelBufferGetHeight(pixels)
        let stride = CVPixelBufferGetBytesPerRow(pixels)
        let base = UnsafePointer(CVPixelBufferGetBaseAddress(pixels)!.assumingMemoryBound(to: UInt8.self))
        // The frame fills the window's width; the title bar is above it.
        let scale = Double(width) / sceneWidth
        let top = Double(height) - sceneHeight * scale
        guard let center = read(base, stride: stride, width: width, height: height, scale: scale, top: top, at: centerAt),
              let edge = read(base, stride: stride, width: width, height: height, scale: scale, top: top, at: edgeAt)
        else { return }
        lock.lock()
        defer { lock.unlock() }
        captured += 1
        differences[Int(Int16(truncatingIfNeeded: center - edge)), default: 0] += 1
        if center != lastCenter {
            lastCenter = center
            centerSeen[center] = nanos
        }
        if edge != lastEdge {
            lastEdge = edge
            edgeSeen[edge] = nanos
        }
        if firstReads.count < 4 { firstReads.append((center, edge)) }
    }
}

let found = DispatchSemaphore(value: 0)
var target: SCWindow?
SCShareableContent.getExcludingDesktopWindows(true, onScreenWindowsOnly: true) { content, _ in
    target = content?.windows
        .filter { $0.owningApplication?.processID == pid }
        .max { $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height }
    found.signal()
}
found.wait()
guard let window = target else {
    print("no on-screen window for pid \(pid); is the screen locked?")
    exit(1)
}
let configuration = SCStreamConfiguration()
configuration.width = Int(window.frame.width * 2)
configuration.height = Int(window.frame.height * 2)
configuration.minimumFrameInterval = CMTime(value: 1, timescale: 240)
configuration.pixelFormat = kCVPixelFormatType_32BGRA
configuration.queueDepth = 8
configuration.showsCursor = false
let probe = Probe()
let stream = SCStream(filter: SCContentFilter(desktopIndependentWindow: window), configuration: configuration, delegate: nil)
try stream.addStreamOutput(probe, type: .screen, sampleHandlerQueue: DispatchQueue(label: "probe", qos: .userInteractive))
let started = DispatchSemaphore(value: 0)
stream.startCapture { error in
    if let error { print("capture failed: \(error)") }
    started.signal()
}
started.wait()
Thread.sleep(forTimeInterval: seconds)
let stopped = DispatchSemaphore(value: 0)
stream.stopCapture { _ in stopped.signal() }
stopped.wait()

probe.lock.lock()
let total = probe.captured
guard total > 0 else {
    print("no display frames with readable frame numbers; is the window in Fit mode, and the scene running?")
    exit(1)
}
let ahead = probe.differences.filter { $0.key > 0 }.values.reduce(0, +)
print(String(format: "window %.0fx%.0f pt: %d display frames in %.0f s, center ahead of the edge in %.1f%% of them",
             window.frame.width, window.frame.height, total, seconds, 100 * Double(ahead) / Double(total)))
print("first readings (center, edge): \(probe.firstReads)")
let histogram = probe.differences.sorted { $0.key < $1.key }
    .map { "\($0.key >= 0 ? "+" : "")\($0.key): \($0.value)" }.joined(separator: "  ")
print("display frames by center number minus edge number: \(histogram)")

// For each number the capture saw at both places: how much later the edge
// showed it than the center did. Display frames quantize this to the
// refresh interval; the mean is unbiased when the phases drift.
var leads: [Double] = []
for (number, centerNanos) in probe.centerSeen {
    if let edgeNanos = probe.edgeSeen[number], edgeNanos >= centerNanos {
        leads.append(Double(edgeNanos - centerNanos) / 1e6)
    }
}
leads.sort()
probe.lock.unlock()
if leads.isEmpty {
    print("no frame number was seen at both the center and the edge")
} else {
    let mean = leads.reduce(0, +) / Double(leads.count)
    print(String(format: "lead of the center over the edge, per frame (%d frames): mean %.1f ms, median %.1f ms, p95 %.1f ms",
                 leads.count, mean, leads[leads.count / 2], leads[leads.count * 95 / 100]))
}

// With the server's publish log: the time from publishing each frame to its
// first appearance at the center and at the edge.
if let publishLog {
    var published: [Int: UInt64] = [:]
    for line in (try String(contentsOfFile: publishLog, encoding: .utf8)).split(separator: "\n") {
        let fields = line.split(separator: " ")
        if fields.count == 2, let frame = Int(fields[0]), let nanos = UInt64(fields[1]) {
            published[frame] = nanos
        }
    }
    func latencies(_ seen: [Int: UInt64]) -> [Double] {
        seen.compactMap { entry -> Double? in
            guard let publish = published[entry.key], entry.value > publish else { return nil }
            let milliseconds = Double(entry.value - publish) / 1e6
            return milliseconds < 1000 ? milliseconds : nil
        }.sorted()
    }
    probe.lock.lock()
    let center = latencies(probe.centerSeen)
    let edge = latencies(probe.edgeSeen)
    probe.lock.unlock()
    for (name, values) in [("center", center), ("edge", edge)] where !values.isEmpty {
        let mean = values.reduce(0, +) / Double(values.count)
        print(String(format: "published -> on screen at the %@ (%d frames): mean %.1f ms, median %.1f ms, p95 %.1f ms",
                     name, values.count, mean, values[values.count / 2], values[values.count * 95 / 100]))
    }
}
