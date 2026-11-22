// Hub state fed by core events. One ChangeNotifier for the whole app (small enough for Phase 1).
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

import 'hub_api.dart';

class SourceInfo {
  SourceInfo(Map<String, dynamic> j)
      : id = j['id'] as int,
        kind = j['kind'] as String,
        title = j['title'] as String,
        nativeW = j['native_w'] as int,
        nativeH = j['native_h'] as int,
        refreshHz = (j['refresh_hz'] as num).toDouble();
  final int id;
  final String kind;
  final String title;
  final int nativeW, nativeH;
  final double refreshHz;
  double get aspect => nativeH > 0 ? nativeW / nativeH : 16 / 9;
}

class AgentView {
  AgentView(this.id, this.addr);
  final int id;
  final String addr;
  String name = '';
  String fingerprint = '';
  String state = 'connecting';
  String detail = '';
  List<SourceInfo> sources = [];
  bool screenRecording = true, accessibility = true, locked = false, asleep = false;
  final Map<int, String> cursorKinds = {};
  String get label => name.isNotEmpty ? name : addr;
}

class TileView {
  TileView(this.id, this.texture, this.agent, this.source);
  final int id, texture, agent;
  final SourceInfo source;
  String state = 'waiting';
  String detail = '';
  int w = 0, h = 0, fps = 0;
  // Last requested stream size (TierController).
  int reqW = 0, reqH = 0, reqFps = 0;
  bool reqFocus = false;
  Map<String, dynamic>? stats;
  double get aspect => (w > 0 && h > 0) ? w / h : source.aspect;
}

class Discovered {
  Discovered(this.key, this.name, this.os, this.fpShort, this.addrs);
  final String key, name, os, fpShort;
  final List<String> addrs;
}

class PairPrompt {
  PairPrompt({required this.agent, required this.name, required this.sas, this.request});
  final int agent;
  final String name, sas;
  /// Set for "agent knows us, we do not know it" (answer with HubApi.answer).
  final int? request;
}

class HubModel extends ChangeNotifier {
  HubModel(this.api);
  final HubApi api;
  final Map<int, AgentView> agents = {};
  final Map<int, TileView> tiles = {};
  final Map<String, Discovered> discovered = {};
  final List<PairPrompt> prompts = [];
  String fingerprint = '';
  Map<String, String> env = {};
  int? focusedTile;
  /// Tile currently holding the keyboard grab (T1.7).
  int? grabbedTile;
  /// Last user-facing message (shown as a snack bar), e.g. why grab failed.
  String? notice;
  String _stateDir = '';
  StreamSubscription<Map<String, dynamic>>? _sub;

  /// Agents to reconnect at start; next to the hub identity (`RZ_HUB_STATE_DIR` or Application Support).
  File get _agentsFile => File('$_stateDir/agents.json');

  Future<void> start() async {
    _sub = api.events.listen(_onEvent);
    api.onNative((method, args) {
      if (method == 'grabEnded') {
        grabbedTile = null;
        notifyListeners();
      }
    });
    final r = await api.init();
    fingerprint = r.fingerprint;
    env = r.env;
    _stateDir = env['RZ_HUB_STATE_DIR'] ?? '${await api.appSupportDir()}/hub';
    await api.startDiscovery();
    for (final a in _loadSaved()) {
      await addAgent(a);
    }
    for (final a in (env['RZ_HUB_ADD'] ?? '').split(',').where((s) => s.isNotEmpty)) {
      if (!agents.values.any((x) => x.addr == a)) await addAgent(a);
    }
    notifyListeners();
  }

  List<String> _loadSaved() {
    try {
      final j = jsonDecode(_agentsFile.readAsStringSync()) as List;
      return j.map((e) => (e as Map)['addr'] as String).toList();
    } catch (_) {
      return [];
    }
  }

  void _save() {
    try {
      _agentsFile.parent.createSync(recursive: true);
      final list = agents.values.map((a) => {'addr': a.addr, 'name': a.name, 'fingerprint': a.fingerprint}).toList();
      _agentsFile.writeAsStringSync(const JsonEncoder.withIndent('  ').convert(list));
    } catch (e) {
      debugPrint('save agents: $e');
    }
  }

  Future<void> addAgent(String addr) async {
    final id = await api.addAgent(addr);
    if (id == 0) return;
    agents[id] = AgentView(id, addr);
    _save();
    notifyListeners();
  }

  Future<void> removeAgent(int id) async {
    for (final t in tiles.values.where((t) => t.agent == id).toList()) {
      await api.unsubscribe(t.id);
      tiles.remove(t.id);
    }
    await api.removeAgent(id);
    agents.remove(id);
    _save();
    notifyListeners();
  }

  void answer(PairPrompt p, bool accept) {
    if (p.request != null) api.answer(p.request!, accept);
    prompts.remove(p);
    notifyListeners();
  }

  void dismissPrompt(PairPrompt p) {
    prompts.remove(p);
    notifyListeners();
  }

  Future<void> toggleGrab(int tile) async {
    if (grabbedTile != null) {
      await api.grabStop();
      grabbedTile = null;
    } else {
      final err = await api.grabStart(tile);
      if (err != null) {
        notice = err;
      } else {
        focus(tile);
        grabbedTile = tile;
      }
    }
    notifyListeners();
  }

  void focus(int? tile) {
    if (focusedTile == tile) return;
    if (grabbedTile != null && grabbedTile != tile) {
      api.grabStop();
      grabbedTile = null;
    }
    if (focusedTile != null) api.releaseAll(focusedTile!);
    focusedTile = tile;
    notifyListeners();
  }

  /// One tile per display, created when the agent reports its sources.
  Future<void> _ensureTiles(AgentView a) async {
    for (final s in a.sources.where((s) => s.kind == 'display')) {
      if (tiles.values.any((t) => t.agent == a.id && t.source.id == s.id)) continue;
      final r = await api.subscribe(a.id, s.id, maxW: 1280, maxH: 720, fps: 30);
      if (r == null) continue;
      tiles[r.tile] = TileView(r.tile, r.texture, a.id, s)
        ..reqW = 1280
        ..reqH = 720
        ..reqFps = 30;
    }
    notifyListeners();
  }

  /// TierController (plan 9.1, simplified): stream size = tile's physical pixels, FOCUS ≤ 2560×1440 @ 60 fps
  /// (DECISIONS: 4K60 is beyond the encoder), others ≤ 30 fps.
  final Map<int, Timer> _debounce = {};
  void wantSize(TileView t, int physW, int physH) {
    final focus = focusedTile == t.id;
    final capW = focus ? 2560 : 1920, capH = focus ? 1440 : 1080;
    final w = physW.clamp(320, capW), h = physH.clamp(180, capH);
    final fps = focus ? 60 : 30;
    // Ignore small changes (< 10 %) to avoid restarting the stream for nothing.
    bool close(int a, int b) => b > 0 && (a - b).abs() / b < 0.1;
    if (close(w, t.reqW) && close(h, t.reqH) && fps == t.reqFps && focus == t.reqFocus) return;
    _debounce[t.id]?.cancel();
    _debounce[t.id] = Timer(const Duration(milliseconds: 150), () {
      t
        ..reqW = w
        ..reqH = h
        ..reqFps = fps
        ..reqFocus = focus;
      api.updateTile(t.id, t.source.id, maxW: w, maxH: h, fps: fps, focus: focus);
    });
  }

  void _onEvent(Map<String, dynamic> e) {
    if (env['RZ_HUB_TRACE'] == '1') stdout.writeln(jsonEncode(e));
    final type = e['type'] as String;
    AgentView? agent() => agents[e['agent']];
    switch (type) {
      case 'agent_found':
        discovered[e['key'] as String] = Discovered(e['key'] as String, e['name'] as String, e['os'] as String,
            e['fp_short'] as String, List<String>.from(e['addrs'] as List));
      case 'agent_lost':
        discovered.remove(e['key']);
      case 'agent_state':
        final a = agent();
        if (a == null) break;
        a.state = e['state'] as String;
        a.detail = e['detail'] as String;
        if (a.state == 'connected' || a.state == 'disconnected') {
          prompts.removeWhere((p) => p.agent == a.id && p.request == null);
        }
      case 'agent_info':
        final a = agent();
        if (a == null) break;
        a.name = e['name'] as String;
        a.fingerprint = e['fingerprint'] as String;
        _save();
      case 'pair_code':
        prompts.add(PairPrompt(agent: e['agent'] as int, name: e['name'] as String, sas: e['sas'] as String));
      case 'confirm_agent':
        prompts.add(PairPrompt(
            agent: e['agent'] as int, name: e['name'] as String, sas: e['sas'] as String, request: e['request'] as int));
      case 'sources':
        final a = agent();
        if (a == null) break;
        a.sources = (e['sources'] as List).map((s) => SourceInfo(s as Map<String, dynamic>)).toList();
        _ensureTiles(a);
      case 'status':
        final a = agent();
        if (a == null) break;
        a
          ..screenRecording = e['screen_recording'] as bool
          ..accessibility = e['accessibility'] as bool
          ..locked = e['screen_locked'] as bool
          ..asleep = e['display_asleep'] as bool;
      case 'tile_state':
        final t = tiles[e['tile']];
        if (t == null) break;
        t
          ..state = e['state'] as String
          ..detail = e['detail'] as String;
        if (t.state == 'live') {
          t
            ..w = e['w'] as int
            ..h = e['h'] as int
            ..fps = e['fps'] as int;
        }
      case 'tile_stats':
        tiles[e['tile']]?.stats = e;
      case 'cursor_shape':
        agent()?.cursorKinds[e['shape_id'] as int] = e['kind'] as String;
    }
    notifyListeners();
  }

  @override
  void dispose() {
    _sub?.cancel();
    super.dispose();
  }
}
