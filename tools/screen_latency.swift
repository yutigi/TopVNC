// Measures how long frames take to reach the screen in a TopVNC viewer on
// macOS: from latency_bench publishing a frame to the frame being displayed
// in the viewer's window, including encoding, transport, decoding, upload,
// presentation, and window-server composition.
//
// 1. cargo run --release --example latency_bench -- --serve 127.0.0.1:5999 --publish-log /tmp/publish.log
// 2. cargo run --release -- 127.0.0.1:5999 --allow-insecure, then Connect,
//    and leave the window in Fit mode with the scene's aspect ratio.
// 3. swift tools/screen_latency.swift VIEWER_PID SECONDS /tmp/publish.log
//
// The scene's frame number is read from the 32-pixel blocks latency_bench
// draws at its bottom-left. Needs Screen Recording permission for the
// terminal. Usage: screen_latency PID SECONDS PUBLISH_LOG [WIDTH HEIGHT]
import AppKit
import CoreMedia
import CoreVideo
import Foundation
import ScreenCaptureKit

let arguments = CommandLine.arguments
guard arguments.count >= 4, let pid = Int32(arguments[1]), let seconds = Double(arguments[2]) else {
    print("usage: screen_latency PID SECONDS PUBLISH_LOG [SCENE_WIDTH SCENE_HEIGHT]")
    exit(2)
}
let publishLog = arguments[3]
let sceneWidth = arguments.count > 4 ? Double(arguments[4])! : 1920
let sceneHeight = arguments.count > 5 ? Double(arguments[5])! : 1080

// ScreenCaptureKit needs this process's window server connection.
_ = NSApplication.shared
_ = CGMainDisplayID()

var timebase = mach_timebase_info_data_t()
mach_timebase_info(&timebase)

/// The frame number shown in each captured frame, and when it was displayed.
final class Probe: NSObject, SCStreamOutput {
    var last = -1
    var seen: [(frame: Int, nanos: UInt64)] = []
    let lock = NSLock()

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
        let base = CVPixelBufferGetBaseAddress(pixels)!.assumingMemoryBound(to: UInt8.self)
        // The scene fills the window's width; the title bar is above it.
        let scale = Double(width) / sceneWidth
        let top = Double(height) - sceneHeight * scale
        var frame = 0
        for bit in 0..<16 {
            let column = Double(bit % 4), row = Double(bit / 4)
            let x = (column * 32 + 16) * scale
            let y = top + (sceneHeight - 128 + row * 32 + 16) * scale
            guard y >= 0, Int(y) < height, Int(x) < width else { return }
            if base[Int(y) * stride + Int(x) * 4 + 1] >= 128 { frame |= 1 << bit }
        }
        lock.lock()
        if frame != last {
            last = frame
            seen.append((frame, nanos))
        }
        lock.unlock()
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

// Publish times by frame number; the log restarts at 2 and wraps at 65536.
var published: [Int: UInt64] = [:]
for line in (try String(contentsOfFile: publishLog, encoding: .utf8)).split(separator: "\n") {
    let fields = line.split(separator: " ")
    if fields.count == 2, let frame = Int(fields[0]), let nanos = UInt64(fields[1]) {
        published[frame] = nanos
    }
}
probe.lock.lock()
let latencies = probe.seen.compactMap { entry -> Double? in
    guard let publish = published[entry.frame], entry.nanos > publish else { return nil }
    let milliseconds = Double(entry.nanos - publish) / 1e6
    return milliseconds < 1000 ? milliseconds : nil
}.sorted()
probe.lock.unlock()
guard !latencies.isEmpty else {
    print("no frame numbers matched the publish log")
    exit(1)
}
let mean = latencies.reduce(0, +) / Double(latencies.count)
print(String(format: "window %.0fx%.0f pt: %.1f distinct frames/s on screen, latency mean %.1f ms, median %.1f ms, p95 %.1f ms, min %.1f ms",
             window.frame.width, window.frame.height, Double(latencies.count) / seconds, mean,
             latencies[latencies.count / 2], latencies[latencies.count * 95 / 100], latencies[0]))
