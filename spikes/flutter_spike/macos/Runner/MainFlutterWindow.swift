import Cocoa
import CoreVideo
import FlutterMacOS

// T0.6 Flutter spike: Rust client (librz_spike_ffi.a) → CVPixelBuffer → FlutterTexture.
// The plugin lives in this file so the generated Xcode project needs no edits.

// MARK: Rust C ABI (spikes/rz-spike-ffi/src/lib.rs)

@_silgen_name("rz_spike_start")
func rz_spike_start(
  _ addr: UnsafePointer<CChar>, _ maxW: UInt32, _ maxH: UInt32, _ fps: UInt32,
  _ onFrame: @convention(c) (UnsafeMutableRawPointer?) -> Void, _ ctx: UnsafeMutableRawPointer?
) -> OpaquePointer?

@_silgen_name("rz_spike_take_latest")
func rz_spike_take_latest(_ h: OpaquePointer?) -> UnsafeMutableRawPointer?

@_silgen_name("rz_spike_stats")
func rz_spike_stats(_ h: OpaquePointer?, _ buf: UnsafeMutablePointer<CChar>, _ len: Int) -> Int

@_silgen_name("rz_spike_stop")
func rz_spike_stop(
  _ h: OpaquePointer?, _ csv: UnsafePointer<CChar>?, _ buf: UnsafeMutablePointer<CChar>, _ len: Int
) -> Int

// MARK: Texture

final class RzTexture: NSObject, FlutterTexture {
  var handle: OpaquePointer?

  /// Called by the Flutter raster thread when it composites. Rust stamps present_ns here.
  func copyPixelBuffer() -> Unmanaged<CVPixelBuffer>? {
    guard let h = handle, let p = rz_spike_take_latest(h) else { return nil }
    return Unmanaged<CVPixelBuffer>.fromOpaque(p)  // +1 retained by Rust; Flutter releases it.
  }
}

// MARK: Plugin

final class RzSpikePlugin: NSObject, FlutterPlugin {
  private let registry: FlutterTextureRegistry
  private let texture = RzTexture()
  private var textureId: Int64 = -1

  init(registry: FlutterTextureRegistry) {
    self.registry = registry
  }

  static func register(with registrar: FlutterPluginRegistrar) {
    let channel = FlutterMethodChannel(name: "rozhledna/spike", binaryMessenger: registrar.messenger)
    let instance = RzSpikePlugin(registry: registrar.textures)
    registrar.addMethodCallDelegate(instance, channel: channel)
  }

  fileprivate func frameAvailable() {
    // FlutterEngineMarkExternalTextureFrameAvailable is thread-safe; calling it directly from the
    // Rust decode thread avoids a main-thread hop.
    registry.textureFrameAvailable(textureId)
  }

  func handle(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
    let args = call.arguments as? [String: Any] ?? [:]
    switch call.method {
    case "env":
      result(ProcessInfo.processInfo.environment.filter { $0.key.hasPrefix("RZ_SPIKE_") })
    case "start":
      guard texture.handle == nil else { return result(textureId) }
      let addr = args["addr"] as? String ?? "127.0.0.1:47470"
      let maxW = UInt32(args["maxW"] as? Int ?? 3840)
      let maxH = UInt32(args["maxH"] as? Int ?? 2160)
      let fps = UInt32(args["fps"] as? Int ?? 60)
      textureId = registry.register(texture)
      let ctx = Unmanaged.passUnretained(self).toOpaque()
      let h = addr.withCString { cstr in
        rz_spike_start(cstr, maxW, maxH, fps, { ctx in
          guard let ctx = ctx else { return }
          Unmanaged<RzSpikePlugin>.fromOpaque(ctx).takeUnretainedValue().frameAvailable()
        }, ctx)
      }
      guard let handle = h else {
        registry.unregisterTexture(textureId)
        return result(FlutterError(code: "start", message: "rz_spike_start failed (bad address?)", details: nil))
      }
      texture.handle = handle
      result(textureId)
    case "stats":
      guard let h = texture.handle else { return result("") }
      var buf = [CChar](repeating: 0, count: 4096)
      _ = rz_spike_stats(h, &buf, buf.count)
      result(String(cString: buf))
    case "stop":
      guard let h = texture.handle else { return result("") }
      texture.handle = nil
      registry.unregisterTexture(textureId)
      var buf = [CChar](repeating: 0, count: 8192)
      if let csv = args["csv"] as? String, !csv.isEmpty {
        _ = csv.withCString { rz_spike_stop(h, $0, &buf, buf.count) }
      } else {
        _ = rz_spike_stop(h, nil, &buf, buf.count)
      }
      let report = String(cString: buf)
      print(report)  // stdout, for automated runs
      result(report)
    case "quit":
      result(nil)
      DispatchQueue.main.async { NSApp.terminate(nil) }
    default:
      result(FlutterMethodNotImplemented)
    }
  }
}

class MainFlutterWindow: NSWindow {
  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    let windowFrame = self.frame
    self.contentViewController = flutterViewController
    self.setFrame(windowFrame, display: true)

    RegisterGeneratedPlugins(registry: flutterViewController)
    RzSpikePlugin.register(with: flutterViewController.registrar(forPlugin: "RzSpikePlugin"))

    super.awakeFromNib()
  }
}
