// A tile: the remote screen as a Flutter Texture (letterboxed), overlay, and input (plan 9.6, without grab).
import 'dart:async';

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'layout.dart';
import 'model.dart';

class TileWidget extends StatefulWidget {
  const TileWidget({super.key, required this.model, required this.tile, required this.showStats});
  final HubModel model;
  final TileView tile;
  final bool showStats;

  @override
  State<TileWidget> createState() => _TileWidgetState();
}

class _TileWidgetState extends State<TileWidget> {
  final _focus = FocusNode();
  Rect _video = Rect.zero;
  final Set<int> _down = {};
  int _lastButton = -1, _clicks = 0;
  DateTime _lastDown = DateTime.fromMillisecondsSinceEpoch(0);
  Offset _lastDownPos = Offset.zero;
  MouseCursor _cursor = SystemMouseCursors.basic;
  Timer? _cursorPoll;

  HubModel get m => widget.model;
  TileView get t => widget.tile;
  bool get focused => m.focusedTile == t.id;

  @override
  void dispose() {
    _cursorPoll?.cancel();
    _focus.dispose();
    super.dispose();
  }

  (double, double)? _norm(Offset local) {
    if (_video.isEmpty) return null;
    final x = (local.dx - _video.left) / _video.width, y = (local.dy - _video.top) / _video.height;
    if (x < 0 || y < 0 || x > 1 || y > 1) return null;
    return (x, y);
  }

  static int _buttonOf(int buttons) {
    if (buttons & kPrimaryMouseButton != 0) return 0;
    if (buttons & kSecondaryMouseButton != 0) return 1;
    if (buttons & kMiddleMouseButton != 0) return 2;
    if (buttons & kBackMouseButton != 0) return 3;
    if (buttons & kForwardMouseButton != 0) return 4;
    return 0;
  }

  void _onDown(PointerDownEvent e) {
    m.focus(t.id);
    _focus.requestFocus();
    final p = _norm(e.localPosition);
    if (p == null) return;
    final b = _buttonOf(e.buttons);
    final now = DateTime.now();
    // Click count: same button, < 400 ms, < 5 px apart (macOS-like).
    if (b == _lastButton && now.difference(_lastDown).inMilliseconds < 400 && (e.localPosition - _lastDownPos).distance < 5) {
      _clicks++;
    } else {
      _clicks = 1;
    }
    _lastButton = b;
    _lastDown = now;
    _lastDownPos = e.localPosition;
    _down.add(b);
    m.api.button(t.id, b, true, p.$1, p.$2, _clicks);
  }

  void _onUp(PointerEvent e) {
    final p = _norm(e.localPosition) ?? (0.5, 0.5);
    // `buttons` on the up event no longer has the released button: release whatever is not held any more.
    for (final b in _down.toList()) {
      if (e.buttons & (1 << b) == 0) {
        _down.remove(b);
        m.api.button(t.id, b, false, p.$1.clamp(0, 1), p.$2.clamp(0, 1), _clicks);
      }
    }
  }

  void _onMove(PointerEvent e) {
    final p = _norm(e.localPosition);
    if (p != null) m.api.pointer(t.id, p.$1, p.$2);
  }

  void _onSignal(PointerSignalEvent e) {
    if (e is PointerScrollEvent) {
      // Flutter: positive dy = content moves up; protocol (CG): positive dy = scroll up / content moves down.
      m.api.scroll(t.id, -e.scrollDelta.dx, -e.scrollDelta.dy, precise: true);
    }
  }

  // Trackpad: pan/zoom gestures carry phases (plan: NSEventPhase 1 began, 4 changed, 8 ended).
  void _onPanStart(PointerPanZoomStartEvent e) => m.api.scroll(t.id, 0, 0, precise: true, phase: 1);
  void _onPanUpdate(PointerPanZoomUpdateEvent e) =>
      m.api.scroll(t.id, e.panDelta.dx, e.panDelta.dy, precise: true, phase: 4);
  void _onPanEnd(PointerPanZoomEndEvent e) => m.api.scroll(t.id, 0, 0, precise: true, phase: 8);

  KeyEventResult _onKey(FocusNode node, KeyEvent e) {
    final hid = e.physicalKey.usbHidUsage;
    if (e is KeyDownEvent || e is KeyRepeatEvent) {
      m.api.key(t.id, hid, true);
    } else if (e is KeyUpEvent) {
      m.api.key(t.id, hid, false);
    }
    return KeyEventResult.handled; // no system beep, no Flutter shortcuts
  }

  void _pollCursor(bool hovering) {
    _cursorPoll?.cancel();
    if (!hovering) return;
    _cursorPoll = Timer.periodic(const Duration(milliseconds: 100), (_) async {
      final c = await m.api.cursor(t.id);
      if (c == null || !mounted) return;
      final kind = m.agents[t.agent]?.cursorKinds[c.shape] ?? 'arrow';
      final next = switch (kind) {
        'ibeam' => SystemMouseCursors.text,
        'pointer' => SystemMouseCursors.click,
        'crosshair' => SystemMouseCursors.precise,
        'grab' => SystemMouseCursors.grab,
        'grabbing' => SystemMouseCursors.grabbing,
        'resize-h' => SystemMouseCursors.resizeLeftRight,
        'resize-v' => SystemMouseCursors.resizeUpDown,
        'not-allowed' => SystemMouseCursors.forbidden,
        'copy' => SystemMouseCursors.copy,
        _ => SystemMouseCursors.basic,
      };
      if (next != _cursor) setState(() => _cursor = next);
    });
  }

  @override
  Widget build(BuildContext context) {
    final a = m.agents[t.agent];
    final dpr = MediaQuery.devicePixelRatioOf(context);
    return LayoutBuilder(builder: (context, c) {
      final cell = Offset.zero & c.biggest;
      _video = letterbox(cell, t.aspect);
      // TierController: ask for the tile's physical size.
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) m.wantSize(t, (_video.width * dpr).round(), (_video.height * dpr).round());
      });
      final status = <String>[
        if (a != null && a.state != 'connected') a.state == 'pairing' ? 'párování…' : 'odpojeno — zkouším znovu',
        if (a?.locked ?? false) 'obrazovka zamčená',
        if (!(a?.screenRecording ?? true)) 'bez oprávnění Nahrávání obrazovky',
        if (t.state == 'error') t.detail,
      ];
      final s = t.stats;
      return Focus(
        focusNode: _focus,
        onKeyEvent: _onKey,
        onFocusChange: (f) {
          if (!f && focused) m.api.releaseAll(t.id);
        },
        child: MouseRegion(
          cursor: _cursor,
          onEnter: (_) => _pollCursor(true),
          onExit: (_) => _pollCursor(false),
          child: Listener(
            onPointerDown: _onDown,
            onPointerUp: _onUp,
            onPointerCancel: _onUp,
            onPointerHover: _onMove,
            onPointerMove: _onMove,
            onPointerSignal: _onSignal,
            onPointerPanZoomStart: _onPanStart,
            onPointerPanZoomUpdate: _onPanUpdate,
            onPointerPanZoomEnd: _onPanEnd,
            child: Stack(children: [
              Positioned.fromRect(rect: cell, child: const ColoredBox(color: Color(0xFF0B0D10))),
              Positioned.fromRect(rect: _video, child: Texture(textureId: t.texture, filterQuality: FilterQuality.medium)),
              Positioned.fromRect(
                rect: _video,
                child: IgnorePointer(
                  child: DecoratedBox(
                    decoration: BoxDecoration(
                      border: Border.all(color: focused ? const Color(0xFF4EA1FF) : const Color(0x22FFFFFF), width: focused ? 2 : 1),
                    ),
                  ),
                ),
              ),
              Positioned(
                left: _video.left + 8,
                top: _video.top + 6,
                child: IgnorePointer(
                  child: _Label(
                    text: '${a?.label ?? '?'} · ${t.source.title}'
                        '${widget.showStats && s != null ? '\n${t.w}×${t.h}  ${(s['fps'] as num).toStringAsFixed(0)} fps  '
                            '${((s['bitrate_kbps'] as num) / 1000).toStringAsFixed(1)} Mb/s  '
                            'P50 ${(s['latency_p50_ms'] as num).toStringAsFixed(1)} ms' : ''}',
                    dot: t.state == 'live' && (a?.state == 'connected') ? const Color(0xFF3DDC84) : const Color(0xFFFFB020),
                  ),
                ),
              ),
              if (status.isNotEmpty)
                Positioned.fromRect(
                  rect: _video,
                  child: IgnorePointer(
                    child: Center(
                      child: Container(
                        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
                        decoration: BoxDecoration(color: const Color(0xCC000000), borderRadius: BorderRadius.circular(8)),
                        child: Text(status.join('\n'), textAlign: TextAlign.center, style: const TextStyle(color: Colors.white)),
                      ),
                    ),
                  ),
                ),
            ]),
          ),
        ),
      );
    });
  }
}

class _Label extends StatelessWidget {
  const _Label({required this.text, required this.dot});
  final String text;
  final Color dot;

  @override
  Widget build(BuildContext context) => Container(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
        decoration: BoxDecoration(color: const Color(0xAA000000), borderRadius: BorderRadius.circular(6)),
        child: Row(mainAxisSize: MainAxisSize.min, crossAxisAlignment: CrossAxisAlignment.start, children: [
          Padding(
            padding: const EdgeInsets.only(top: 4, right: 6),
            child: Container(width: 8, height: 8, decoration: BoxDecoration(color: dot, shape: BoxShape.circle)),
          ),
          Text(text, style: const TextStyle(color: Colors.white, fontSize: 12, height: 1.35)),
        ]),
      );
}
