import Cocoa
import CoreVideo
import FlutterMacOS

// Rozhledna hub: Flutter UI over the Rust hub core (crates/rz-hub-ffi, librz_hub_ffi.a).
// Plugin in this file so the generated Xcode project needs no edits (same approach as the T0.6 spike).

// MARK: Rust C ABI (crates/rz-hub-ffi/include/rz_hub.h)

typealias RzFrameCallback = @convention(c) (UnsafeMutableRawPointer?, UInt64) -> Void

@_silgen_name("rz_hub_new") func rz_hub_new(
  _ stateDir: UnsafePointer<CChar>?, _ name: UnsafePointer<CChar>?, _ onFrame: RzFrameCallback,
  _ ctx: UnsafeMutableRawPointer?) -> OpaquePointer?
@_silgen_name("rz_hub_fingerprint") func rz_hub_fingerprint(_ h: OpaquePointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("rz_hub_next_event") func rz_hub_next_event(_ h: OpaquePointer?, _ timeoutMs: UInt32)
  -> UnsafeMutablePointer<CChar>?
@_silgen_name("rz_hub_free_string") func rz_hub_free_string(_ s: UnsafeMutablePointer<CChar>?)
@_silgen_name("rz_hub_start_discovery") func rz_hub_start_discovery(_ h: OpaquePointer?) -> Bool
@_silgen_name("rz_hub_add_agent") func rz_hub_add_agent(_ h: OpaquePointer?, _ addr: UnsafePointer<CChar>) -> UInt64
@_silgen_name("rz_hub_remove_agent") func rz_hub_remove_agent(_ h: OpaquePointer?, _ agent: UInt64)
@_silgen_name("rz_hub_subscribe") func rz_hub_subscribe(
  _ h: OpaquePointer?, _ agent: UInt64, _ source: UInt64, _ maxW: UInt32, _ maxH: UInt32, _ fps: UInt32,
  _ focus: Bool, _ bitrate: UInt32) -> UInt64
@_silgen_name("rz_hub_update_tile") func rz_hub_update_tile(
  _ h: OpaquePointer?, _ tile: UInt64, _ source: UInt64, _ maxW: UInt32, _ maxH: UInt32, _ fps: UInt32,
  _ focus: Bool, _ bitrate: UInt32) -> Bool
@_silgen_name("rz_hub_unsubscribe") func rz_hub_unsubscribe(_ h: OpaquePointer?, _ tile: UInt64)
@_silgen_name("rz_hub_take_frame") func rz_hub_take_frame(_ h: OpaquePointer?, _ tile: UInt64) -> UnsafeMutableRawPointer?
@_silgen_name("rz_hub_key") func rz_hub_key(_ h: OpaquePointer?, _ tile: UInt64, _ hid: UInt32, _ down: Bool)
@_silgen_name("rz_hub_key_mac") func rz_hub_key_mac(_ h: OpaquePointer?, _ tile: UInt64, _ keycode: UInt16, _ down: Bool)
  -> Bool
@_silgen_name("rz_hub_button") func rz_hub_button(
  _ h: OpaquePointer?, _ tile: UInt64, _ button: UInt32, _ down: Bool, _ x: Float, _ y: Float, _ clicks: UInt32)
@_silgen_name("rz_hub_scroll") func rz_hub_scroll(
  _ h: OpaquePointer?, _ tile: UInt64, _ dx: Float, _ dy: Float, _ precise: Bool, _ phase: UInt32)
@_silgen_name("rz_hub_text") func rz_hub_text(_ h: OpaquePointer?, _ tile: UInt64, _ utf8: UnsafePointer<CChar>)
@_silgen_name("rz_hub_pointer") func rz_hub_pointer(_ h: OpaquePointer?, _ tile: UInt64, _ x: Float, _ y: Float)
@_silgen_name("rz_hub_release_all") func rz_hub_release_all(_ h: OpaquePointer?, _ tile: UInt64)
struct RzCursor {
  var x: Float = 0
  var y: Float = 0
  var visible: Bool = false
  var shapeId: UInt32 = 0
}
@_silgen_name("rz_hub_cursor") func rz_hub_cursor(
  _ h: OpaquePointer?, _ tile: UInt64, _ out: UnsafeMutablePointer<RzCursor>) -> Bool
@_silgen_name("rz_hub_answer") func rz_hub_answer(_ h: OpaquePointer?, _ request: UInt64, _ accept: Bool)
@_silgen_name("rz_hub_forget") func rz_hub_forget(_ h: OpaquePointer?, _ fp: UnsafePointer<CChar>) -> Bool

// MARK: Texture per tile

final class RzTileTexture: NSObject, FlutterTexture {
  let tile: UInt64
  weak var plugin: RzHubPlugin?

  init(tile: UInt64, plugin: RzHubPlugin) {
    self.tile = tile
    self.plugin = plugin
  }

  /// Flutter raster thread: take the newest decoded frame (+1 retained by Rust; Flutter releases it).
  func copyPixelBuffer() -> Unmanaged<CVPixelBuffer>? {
    guard let h = plugin?.hub, let p = rz_hub_take_frame(h, tile) else { return nil }
    return Unmanaged<CVPixelBuffer>.fromOpaque(p)
  }
}

// MARK: Key grab (T1.7)

/// While active, a session event tap swallows every key event system-wide (including ⌘Tab, ⌘Space, ⌘Q) and sends
/// it to the grabbed tile. Release chord: ⌃⌥⎋. Needs Accessibility (or Input Monitoring) for the hub; without it
/// `CGEvent.tapCreate` returns nil and `start` fails. Ends by itself when the hub stops being the active app.
final class KeyGrab {
  private var tap: CFMachPort?
  private var source: CFRunLoopSource?
  private(set) var tile: UInt64 = 0
  private var heldModifiers = Set<UInt16>()
  weak var plugin: RzHubPlugin?
  var onEnded: ((String) -> Void)?

  var active: Bool { tap != nil }

  func start(tile: UInt64) -> String? {
    stop(reason: nil)
    let mask: CGEventMask =
      (1 << CGEventType.keyDown.rawValue) | (1 << CGEventType.keyUp.rawValue) | (1 << CGEventType.flagsChanged.rawValue)
    let me = Unmanaged.passUnretained(self).toOpaque()
    guard
      let t = CGEvent.tapCreate(
        tap: .cgSessionEventTap, place: .headInsertEventTap, options: .defaultTap, eventsOfInterest: mask,
        callback: { _, type, event, ctx in
          guard let ctx = ctx else { return Unmanaged.passUnretained(event) }
          return Unmanaged<KeyGrab>.fromOpaque(ctx).takeUnretainedValue().handle(type, event)
        }, userInfo: me)
    else {
      return "Povol Rozhledně Zpřístupnění (Nastavení → Soukromí a zabezpečení → Zpřístupnění) a zkus znovu."
    }
    self.tile = tile
    tap = t
    source = CFMachPortCreateRunLoopSource(nil, t, 0)
    CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
    CGEvent.tapEnable(tap: t, enable: true)
    return nil
  }

  func stop(reason: String?) {
    guard let t = tap else { return }
    CGEvent.tapEnable(tap: t, enable: false)
    if let s = source { CFRunLoopRemoveSource(CFRunLoopGetMain(), s, .commonModes) }
    tap = nil
    source = nil
    heldModifiers.removeAll()
    if let h = plugin?.hub { rz_hub_release_all(h, tile) }
    let t0 = tile
    tile = 0
    if let r = reason { onEnded?(r) }
    _ = t0
  }

  private func handle(_ type: CGEventType, _ event: CGEvent) -> Unmanaged<CGEvent>? {
    if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
      if let t = tap { CGEvent.tapEnable(tap: t, enable: true) }
      return Unmanaged.passUnretained(event)
    }
    guard let h = plugin?.hub, tile != 0 else { return Unmanaged.passUnretained(event) }
    let code = UInt16(event.getIntegerValueField(.keyboardEventKeycode))
    let f = event.flags
    switch type {
    case .keyDown:
      if code == 53 && f.contains(.maskControl) && f.contains(.maskAlternate) {  // ⌃⌥⎋
        DispatchQueue.main.async { self.stop(reason: "chord") }
        return nil
      }
      _ = rz_hub_key_mac(h, tile, code, true)
    case .keyUp:
      _ = rz_hub_key_mac(h, tile, code, false)
    case .flagsChanged:
      if code == 57 {  // Caps Lock toggles: one press each change
        _ = rz_hub_key_mac(h, tile, code, true)
        _ = rz_hub_key_mac(h, tile, code, false)
      } else if heldModifiers.contains(code) {
        heldModifiers.remove(code)
        _ = rz_hub_key_mac(h, tile, code, false)
      } else {
        heldModifiers.insert(code)
        _ = rz_hub_key_mac(h, tile, code, true)
      }
    default:
      return Unmanaged.passUnretained(event)
    }
    return nil  // swallowed: the hub's own Mac does not see it
  }
}

// MARK: Plugin

final class RzHubPlugin: NSObject, FlutterPlugin, FlutterStreamHandler {
  private let registry: FlutterTextureRegistry
  fileprivate var hub: OpaquePointer?
  private var sink: FlutterEventSink?
  private var poller: Thread?
  private let lock = NSLock()
  private var textures: [UInt64: (RzTileTexture, Int64)] = [:]
  private let grab = KeyGrab()
  private var channel: FlutterMethodChannel?

  init(registry: FlutterTextureRegistry) {
    self.registry = registry
    super.init()
    grab.plugin = self
    grab.onEnded = { [weak self] reason in self?.channel?.invokeMethod("grabEnded", arguments: reason) }
    NotificationCenter.default.addObserver(
      forName: NSApplication.didResignActiveNotification, object: nil, queue: .main
    ) { [weak self] _ in self?.grab.stop(reason: "inactive") }
  }

  static func register(with registrar: FlutterPluginRegistrar) {
    let instance = RzHubPlugin(registry: registrar.textures)
    let channel = FlutterMethodChannel(name: "rozhledna/hub", binaryMessenger: registrar.messenger)
    instance.channel = channel
    registrar.addMethodCallDelegate(instance, channel: channel)
    let events = FlutterEventChannel(name: "rozhledna/hub/events", binaryMessenger: registrar.messenger)
    events.setStreamHandler(instance)
  }

  // Rust decode thread → mark the tile's texture dirty (thread-safe in the embedder).
  fileprivate func frameAvailable(_ tile: UInt64) {
    lock.lock()
    let id = textures[tile]?.1
    lock.unlock()
    if let id = id { registry.textureFrameAvailable(id) }
  }

  func onListen(withArguments arguments: Any?, eventSink events: @escaping FlutterEventSink) -> FlutterError? {
    sink = events
    return nil
  }

  func onCancel(withArguments arguments: Any?) -> FlutterError? {
    sink = nil
    return nil
  }

  private func startPoller() {
    guard poller == nil, let h = hub else { return }
    let t = Thread { [weak self] in
      while let s = self, s.hub != nil {
        guard let p = rz_hub_next_event(h, 250) else { continue }
        let json = String(cString: p)
        rz_hub_free_string(p)
        DispatchQueue.main.async { self?.sink?(json) }
      }
    }
    t.name = "rz-hub-events"
    t.start()
    poller = t
  }

  private func u64(_ a: [String: Any], _ k: String) -> UInt64 { UInt64(a[k] as? Int ?? 0) }
  private func u32(_ a: [String: Any], _ k: String, _ d: Int = 0) -> UInt32 { UInt32(a[k] as? Int ?? d) }
  private func f32(_ a: [String: Any], _ k: String) -> Float { Float(a[k] as? Double ?? 0) }
  private func bool(_ a: [String: Any], _ k: String) -> Bool { a[k] as? Bool ?? false }

  func handle(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
    let a = call.arguments as? [String: Any] ?? [:]
    switch call.method {
    case "init":
      if hub == nil {
        let ctx = Unmanaged.passUnretained(self).toOpaque()
        let cb: RzFrameCallback = { ctx, tile in
          guard let ctx = ctx else { return }
          Unmanaged<RzHubPlugin>.fromOpaque(ctx).takeUnretainedValue().frameAvailable(tile)
        }
        let dir = (a["stateDir"] as? String) ?? ProcessInfo.processInfo.environment["RZ_HUB_STATE_DIR"]
        let name = a["name"] as? String
        hub = dir.withOptionalCString { d in name.withOptionalCString { n in rz_hub_new(d, n, cb, ctx) } }
        guard hub != nil else {
          return result(FlutterError(code: "init", message: "rz_hub_new failed (see log)", details: nil))
        }
        startPoller()
      }
      var fp = ""
      if let p = rz_hub_fingerprint(hub) {
        fp = String(cString: p)
        rz_hub_free_string(p)
      }
      result(["fingerprint": fp, "env": ProcessInfo.processInfo.environment.filter { $0.key.hasPrefix("RZ_HUB_") }])
    case "startDiscovery":
      result(rz_hub_start_discovery(hub))
    case "addAgent":
      let addr = a["addr"] as? String ?? ""
      result(Int(addr.withCString { rz_hub_add_agent(hub, $0) }))
    case "removeAgent":
      rz_hub_remove_agent(hub, u64(a, "agent"))
      result(nil)
    case "subscribe":
      let tile = rz_hub_subscribe(
        hub, u64(a, "agent"), u64(a, "source"), u32(a, "maxW", 1920), u32(a, "maxH", 1080), u32(a, "fps", 30),
        bool(a, "focus"), u32(a, "bitrate"))
      guard tile != 0 else { return result(FlutterError(code: "subscribe", message: "failed", details: nil)) }
      let tex = RzTileTexture(tile: tile, plugin: self)
      let id = registry.register(tex)
      lock.lock()
      textures[tile] = (tex, id)
      lock.unlock()
      result(["tile": Int(tile), "texture": Int(id)])
    case "updateTile":
      result(
        rz_hub_update_tile(
          hub, u64(a, "tile"), u64(a, "source"), u32(a, "maxW"), u32(a, "maxH"), u32(a, "fps"), bool(a, "focus"),
          u32(a, "bitrate")))
    case "unsubscribe":
      let tile = u64(a, "tile")
      rz_hub_unsubscribe(hub, tile)
      lock.lock()
      let t = textures.removeValue(forKey: tile)
      lock.unlock()
      if let t = t { registry.unregisterTexture(t.1) }
      result(nil)
    case "key":
      rz_hub_key(hub, u64(a, "tile"), u32(a, "hid"), bool(a, "down"))
      result(nil)
    case "button":
      rz_hub_button(
        hub, u64(a, "tile"), u32(a, "button"), bool(a, "down"), f32(a, "x"), f32(a, "y"), u32(a, "clicks", 1))
      result(nil)
    case "scroll":
      rz_hub_scroll(hub, u64(a, "tile"), f32(a, "dx"), f32(a, "dy"), bool(a, "precise"), u32(a, "phase"))
      result(nil)
    case "text":
      let t = a["text"] as? String ?? ""
      t.withCString { rz_hub_text(hub, u64(a, "tile"), $0) }
      result(nil)
    case "pointer":
      rz_hub_pointer(hub, u64(a, "tile"), f32(a, "x"), f32(a, "y"))
      result(nil)
    case "releaseAll":
      rz_hub_release_all(hub, u64(a, "tile"))
      result(nil)
    case "cursor":
      var c = RzCursor()
      if rz_hub_cursor(hub, u64(a, "tile"), &c) {
        result(["x": Double(c.x), "y": Double(c.y), "visible": c.visible, "shape": Int(c.shapeId)])
      } else {
        result(nil)
      }
    case "answer":
      rz_hub_answer(hub, u64(a, "request"), bool(a, "accept"))
      result(nil)
    case "forget":
      let fp = a["fingerprint"] as? String ?? ""
      result(fp.withCString { rz_hub_forget(hub, $0) })
    case "grabStart":
      if let err = grab.start(tile: u64(a, "tile")) {
        result(err)
      } else {
        result(nil)
      }
    case "grabStop":
      grab.stop(reason: "stopped")
      result(nil)
    case "appSupportDir":
      let d = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
      result(d.appendingPathComponent("Rozhledna").path)
    case "quit":
      result(nil)
      DispatchQueue.main.async { NSApp.terminate(nil) }
    default:
      result(FlutterMethodNotImplemented)
    }
  }
}

extension Optional where Wrapped == String {
  func withOptionalCString<R>(_ body: (UnsafePointer<CChar>?) -> R) -> R {
    switch self {
    case .some(let s): return s.withCString { body($0) }
    case .none: return body(nil)
    }
  }
}

class MainFlutterWindow: NSWindow {
  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    let windowFrame = self.frame
    self.contentViewController = flutterViewController
    self.setFrame(windowFrame, display: true)
    self.title = "Rozhledna"

    RegisterGeneratedPlugins(registry: flutterViewController)
    RzHubPlugin.register(with: flutterViewController.registrar(forPlugin: "RzHubPlugin"))

    super.awakeFromNib()
  }
}
