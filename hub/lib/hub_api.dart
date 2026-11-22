// Thin wrapper over the Swift plugin (macos/Runner/MainFlutterWindow.swift) → Rust hub core (rz-hub-ffi).
import 'dart:convert';

import 'package:flutter/services.dart';

class HubApi {
  static const _ch = MethodChannel('rozhledna/hub');
  static const _events = EventChannel('rozhledna/hub/events');

  /// JSON events from the Rust core (see crates/rz-hub-core/src/events.rs).
  Stream<Map<String, dynamic>> get events =>
      _events.receiveBroadcastStream().map((e) => jsonDecode(e as String) as Map<String, dynamic>);

  Future<({String fingerprint, Map<String, String> env})> init({String? stateDir, String? name}) async {
    final r = await _ch.invokeMapMethod<String, dynamic>('init', {'stateDir': stateDir, 'name': name});
    return (
      fingerprint: r?['fingerprint'] as String? ?? '',
      env: Map<String, String>.from(r?['env'] as Map? ?? {}),
    );
  }

  Future<String> appSupportDir() async => await _ch.invokeMethod<String>('appSupportDir') ?? '';
  Future<bool> startDiscovery() async => await _ch.invokeMethod<bool>('startDiscovery') ?? false;
  Future<int> addAgent(String addr) async => await _ch.invokeMethod<int>('addAgent', {'addr': addr}) ?? 0;
  Future<void> removeAgent(int agent) => _ch.invokeMethod('removeAgent', {'agent': agent});

  Future<({int tile, int texture})?> subscribe(int agent, int source,
      {required int maxW, required int maxH, required int fps, bool focus = false, int bitrate = 0}) async {
    final r = await _ch.invokeMapMethod<String, dynamic>('subscribe', {
      'agent': agent,
      'source': source,
      'maxW': maxW,
      'maxH': maxH,
      'fps': fps,
      'focus': focus,
      'bitrate': bitrate,
    });
    if (r == null) return null;
    return (tile: r['tile'] as int, texture: r['texture'] as int);
  }

  Future<void> updateTile(int tile, int source,
          {required int maxW, required int maxH, required int fps, bool focus = false, int bitrate = 0}) =>
      _ch.invokeMethod('updateTile', {
        'tile': tile,
        'source': source,
        'maxW': maxW,
        'maxH': maxH,
        'fps': fps,
        'focus': focus,
        'bitrate': bitrate,
      });

  Future<void> unsubscribe(int tile) => _ch.invokeMethod('unsubscribe', {'tile': tile});
  void key(int tile, int hid, bool down) => _ch.invokeMethod('key', {'tile': tile, 'hid': hid, 'down': down});
  void button(int tile, int button, bool down, double x, double y, int clicks) => _ch.invokeMethod(
      'button', {'tile': tile, 'button': button, 'down': down, 'x': x, 'y': y, 'clicks': clicks});
  void scroll(int tile, double dx, double dy, {bool precise = true, int phase = 0}) => _ch.invokeMethod(
      'scroll', {'tile': tile, 'dx': dx, 'dy': dy, 'precise': precise, 'phase': phase});
  void text(int tile, String text) => _ch.invokeMethod('text', {'tile': tile, 'text': text});
  void pointer(int tile, double x, double y) => _ch.invokeMethod('pointer', {'tile': tile, 'x': x, 'y': y});
  void releaseAll(int tile) => _ch.invokeMethod('releaseAll', {'tile': tile});

  Future<({double x, double y, bool visible, int shape})?> cursor(int tile) async {
    final r = await _ch.invokeMapMethod<String, dynamic>('cursor', {'tile': tile});
    if (r == null) return null;
    return (
      x: (r['x'] as num).toDouble(),
      y: (r['y'] as num).toDouble(),
      visible: r['visible'] as bool,
      shape: r['shape'] as int,
    );
  }

  void answer(int request, bool accept) => _ch.invokeMethod('answer', {'request': request, 'accept': accept});
  Future<bool> forget(String fingerprint) async =>
      await _ch.invokeMethod<bool>('forget', {'fingerprint': fingerprint}) ?? false;
  Future<void> quit() => _ch.invokeMethod('quit');
}
